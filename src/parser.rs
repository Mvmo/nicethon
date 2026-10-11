use crate::ast::*;
use crate::lexer::{lex, TemplatePart, Tok, Token};

pub struct Parser {
    tokens: Vec<Token>,
    at: usize,
    pub errors: Vec<(Pos, String)>,
}

type PResult<T> = Result<T, ()>;

pub fn parse_module(src: &str) -> (Module, Vec<(Pos, String)>) {
    let tokens = match lex(src, 0) {
        Ok(tokens) => tokens,
        Err(error) => return (Module { imports: vec![], items: vec![] }, vec![error]),
    };
    let mut parser = Parser { tokens, at: 0, errors: vec![] };
    let module = parser.module();
    (module, parser.errors)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.at].tok
    }

    fn peek_at(&self, offset: usize) -> &Tok {
        &self.tokens[(self.at + offset).min(self.tokens.len() - 1)].tok
    }

    fn pos(&self) -> Pos {
        self.tokens[self.at].pos
    }

    fn next(&mut self) -> Tok {
        let tok = self.tokens[self.at].tok.clone();
        if self.at < self.tokens.len() - 1 {
            self.at += 1;
        }
        tok
    }

    fn is(&self, sym: &str) -> bool {
        matches!(self.peek(), Tok::Sym(s) | Tok::Kw(s) if *s == sym)
    }

    fn eat(&mut self, sym: &str) -> bool {
        if self.is(sym) {
            self.next();
            true
        } else {
            false
        }
    }

    fn error<T>(&mut self, pos: Pos, message: impl Into<String>) -> PResult<T> {
        self.errors.push((pos, message.into()));
        Err(())
    }

    fn describe(tok: &Tok) -> String {
        match tok {
            Tok::Ident(name) => format!("'{name}'"),
            Tok::Kw(word) | Tok::Sym(word) => format!("'{word}'"),
            Tok::Eof => "end of file".into(),
            _ => "literal".into(),
        }
    }

    fn expect(&mut self, sym: &str) -> PResult<()> {
        if sym == ">" && self.is(">>") {
            self.tokens[self.at].tok = Tok::Sym(">");
            self.tokens[self.at].pos += 1;
            return Ok(());
        }
        if self.eat(sym) {
            return Ok(());
        }
        let found = Self::describe(self.peek());
        self.error(self.pos(), format!("expected {sym}, found {found}"))
    }

    fn expect_semicolon(&mut self) {
        if !self.eat(";") {
            let end = self.tokens[self.at.saturating_sub(1)].end;
            self.errors.push((end, "expected ;".into()));
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.next();
                Ok(name)
            }
            other => self.error(self.pos(), format!("expected name, found {}", Self::describe(&other))),
        }
    }

    fn name_or_keyword(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Tok::Kw(word) => {
                self.next();
                Ok(word.to_string())
            }
            _ => self.ident(),
        }
    }

    fn string(&mut self) -> PResult<String> {
        match self.next() {
            Tok::Str(text) => Ok(text),
            _ => self.error(self.tokens[self.at - 1].pos, "expected string"),
        }
    }

    fn comma_list<T>(&mut self, close: &str, mut item: impl FnMut(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        let mut items = Vec::new();
        while !self.is(close) {
            items.push(item(self)?);
            if !self.eat(",") {
                break;
            }
        }
        self.expect(close)?;
        Ok(items)
    }

    fn sync(&mut self) {
        let mut depth = 0;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Sym("{") | Tok::Sym("(") | Tok::Sym("[") => depth += 1,
                Tok::Sym("}") | Tok::Sym(")") | Tok::Sym("]") => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                Tok::Sym(";") if depth == 0 => {
                    self.next();
                    return;
                }
                _ => {}
            }
            self.next();
        }
    }

    // Module level:

    fn module(&mut self) -> Module {
        let mut module = Module { imports: vec![], items: vec![] };
        while self.is("import") {
            match self.import() {
                Ok(import) => module.imports.push(import),
                Err(()) => self.sync(),
            }
        }
        while !matches!(self.peek(), Tok::Eof) {
            let start = self.at;
            match self.item() {
                Ok(item) => module.items.push(item),
                Err(()) => {
                    self.sync();
                    if self.at == start {
                        self.next();
                    }
                }
            }
        }
        module
    }

    fn import(&mut self) -> PResult<Import> {
        let pos = self.pos();
        self.expect("import")?;
        let py = self.eat("py");
        let mut star = None;
        let mut names = vec![];
        if self.eat("*") {
            self.expect("as")?;
            star = Some(self.ident()?);
            if !self.eat(",") {
                return self.import_from(py, star, names, pos);
            }
        }
        self.expect("{")?;
        names = self.comma_list("}", |p| {
            let pos = p.pos();
            let keyword = matches!(p.peek(), Tok::Kw(_));
            let name = p.name_or_keyword()?;
            let alias = if p.eat("as") {
                p.ident()?
            } else if keyword {
                return p.error(pos, format!("'{name}' is a keyword, import it with as"));
            } else {
                name.clone()
            };
            Ok((name, alias, pos))
        })?;
        self.import_from(py, star, names, pos)
    }

    fn import_from(
        &mut self,
        py: bool,
        star: Option<String>,
        names: Vec<(String, String, Pos)>,
        pos: Pos,
    ) -> PResult<Import> {
        self.expect("from")?;
        let from = self.string()?;
        self.expect_semicolon();
        Ok(Import { py, star, names, from, pos })
    }

    fn item(&mut self) -> PResult<Item> {
        let pos = self.pos();
        let mut decorators = vec![];
        while self.eat("@") {
            decorators.push(self.postfix()?);
        }
        let export = self.eat("export");
        let kind = match self.peek() {
            Tok::Kw("fn") | Tok::Kw("async") => ItemKind::Fn(self.fn_decl(true)?),
            Tok::Kw("struct") => {
                self.next();
                let name = self.ident()?;
                let generics = self.generics()?;
                ItemKind::Struct(StructDecl { name, generics, fields: self.fields()? })
            }
            Tok::Kw("enum") => self.enum_decl()?,
            Tok::Kw("trait") => {
                self.next();
                let name = self.ident()?;
                let generics = self.generics()?;
                self.expect("{")?;
                let mut methods = vec![];
                while !self.eat("}") {
                    methods.push(self.fn_decl(false)?);
                }
                ItemKind::Trait(TraitDecl { name, generics, methods })
            }
            Tok::Kw("impl") => ItemKind::Impl(self.impl_decl()?),
            Tok::Kw("const") => {
                self.next();
                let name = self.ident()?;
                self.expect(":")?;
                let ty = self.ty()?;
                self.expect("=")?;
                let value = self.expr()?;
                self.expect_semicolon();
                ItemKind::Const(name, ty, value)
            }
            Tok::Kw("type") => {
                self.next();
                let name = self.ident()?;
                let generics = self.generics()?;
                self.expect("=")?;
                let ty = self.ty()?;
                self.expect_semicolon();
                ItemKind::TypeAlias(name, generics, ty)
            }
            Tok::Kw("extern") => self.extern_block()?,
            Tok::Kw("py") => ItemKind::PyClass(self.py_class()?),
            other => {
                let found = Self::describe(other);
                return self.error(pos, format!("expected item, found {found}"));
            }
        };
        Ok(Item { decorators, export, kind, pos })
    }

    fn fn_decl(&mut self, body_required: bool) -> PResult<FnDecl> {
        let is_async = self.eat("async");
        self.expect("fn")?;
        let pos = self.pos();
        let name = self.name_or_keyword()?;
        let generics = self.generics()?;
        self.expect("(")?;
        let params = self.comma_list(")", Self::param)?;
        let ret = if self.eat("->") { Some(self.ty()?) } else { None };
        let body = if self.is("{") {
            Some(self.block()?)
        } else {
            if body_required && !self.is(";") {
                self.expect("{")?;
            }
            self.expect_semicolon();
            None
        };
        Ok(FnDecl { name, pos, is_async, generics, params, ret, body })
    }

    fn param(&mut self) -> PResult<Param> {
        let pos = self.pos();
        if self.eat("self") {
            return Ok(Param {
                name: "self".into(),
                pos,
                kind: ParamKind::SelfParam,
                ty: None,
                default: None,
                optional: false,
                var: false,
            });
        }
        let kind = if self.eat("**") {
            ParamKind::StarStar
        } else if self.eat("*") {
            ParamKind::Star
        } else {
            ParamKind::Normal
        };
        let var = self.eat("var");
        let name = self.ident()?;
        let optional = self.eat("?");
        self.expect(":")?;
        let ty = Some(self.ty()?);
        let default = if self.eat("=") { Some(self.expr()?) } else { None };
        Ok(Param { name, pos, kind, ty, default, optional, var })
    }

    fn generics(&mut self) -> PResult<Vec<Generic>> {
        if !self.eat("<") {
            return Ok(vec![]);
        }
        self.comma_list(">", |p| {
            let name = p.ident()?;
            let mut bounds = vec![];
            if p.eat(":") {
                bounds.push(p.ty()?);
                while p.eat("+") {
                    bounds.push(p.ty()?);
                }
            }
            Ok(Generic { name, bounds })
        })
    }

    fn fields(&mut self) -> PResult<Vec<Field>> {
        self.expect("{")?;
        let mut fields = vec![];
        while !self.eat("}") {
            fields.push(self.field()?);
        }
        Ok(fields)
    }

    fn field(&mut self) -> PResult<Field> {
        let name = self.ident()?;
        self.expect(":")?;
        let ty = self.ty()?;
        let default = if self.eat("=") { Some(self.expr()?) } else { None };
        self.expect_semicolon();
        Ok(Field { name, ty, default })
    }

    fn enum_decl(&mut self) -> PResult<ItemKind> {
        self.next();
        let name = self.ident()?;
        let generics = self.generics()?;
        self.expect("{")?;
        let variants = self.comma_list("}", |p| {
            let pos = p.pos();
            let name = p.ident()?;
            let mut fields = vec![];
            if p.eat("(") {
                fields = p.comma_list(")", |p| {
                    let label = match (p.peek().clone(), p.peek_at(1)) {
                        (Tok::Ident(label), Tok::Sym(":")) => {
                            p.next();
                            p.next();
                            Some(label)
                        }
                        _ => None,
                    };
                    Ok((label, p.ty()?))
                })?;
            }
            Ok(Variant { name, pos, fields })
        })?;
        Ok(ItemKind::Enum(EnumDecl { name, generics, variants }))
    }

    fn impl_decl(&mut self) -> PResult<ImplDecl> {
        self.next();
        let generics = self.generics()?;
        let first = self.ty()?;
        let (trait_name, target) = if self.eat("for") { (Some(first), self.ty()?) } else { (None, first) };
        self.expect("{")?;
        let mut methods = vec![];
        while !self.eat("}") {
            methods.push(self.item()?);
        }
        Ok(ImplDecl { generics, trait_name, target, methods })
    }

    fn extern_block(&mut self) -> PResult<ItemKind> {
        self.next();
        self.expect("py")?;
        let module = self.string()?;
        self.expect("{")?;
        let mut items = vec![];
        while !self.eat("}") {
            if self.eat("const") {
                let name = self.name_or_keyword()?;
                self.expect(":")?;
                let ty = self.ty()?;
                self.expect_semicolon();
                items.push(ExternItem::Const(name, ty));
            } else if self.eat("type") {
                let name = self.ident()?;
                let generics = self.generics()?;
                let mut bases = vec![];
                if self.eat(":") {
                    bases.push(self.ty()?);
                    while self.eat(",") {
                        bases.push(self.ty()?);
                    }
                }
                self.expect("{")?;
                let (mut attrs, mut methods) = (vec![], vec![]);
                while !self.eat("}") {
                    if self.is("fn") || self.is("async") {
                        methods.push(self.fn_decl(false)?);
                    } else {
                        let attr = self.name_or_keyword()?;
                        self.expect(":")?;
                        attrs.push((attr, self.ty()?));
                        self.expect_semicolon();
                    }
                }
                items.push(ExternItem::Type(ExternType { name, generics, bases, attrs, methods }));
            } else {
                items.push(ExternItem::Fn(self.fn_decl(false)?));
            }
        }
        Ok(ItemKind::Extern(module, items))
    }

    fn py_class(&mut self) -> PResult<PyClassDecl> {
        self.next();
        self.expect("class")?;
        let name = self.ident()?;
        let mut bases = vec![];
        if self.eat(":") {
            bases.push(self.postfix()?);
            while self.eat(",") {
                bases.push(self.postfix()?);
            }
        }
        self.expect("{")?;
        let (mut fields, mut methods) = (vec![], vec![]);
        while !self.eat("}") {
            if matches!(self.peek(), Tok::Ident(_)) {
                fields.push(self.field()?);
            } else {
                methods.push(self.item()?);
            }
        }
        Ok(PyClassDecl { name, bases, fields, methods })
    }

    // Types:

    fn ty(&mut self) -> PResult<TypeExpr> {
        let pos = self.pos();
        if self.eat("(") {
            let items = self.comma_list(")", Self::ty)?;
            let trailing_comma = matches!(self.tokens[self.at - 2].tok, Tok::Sym(","));
            if self.eat("->") {
                return Ok(TypeExpr::Fn(items, Box::new(self.ty()?), pos));
            }
            if items.len() == 1 && !trailing_comma {
                return Ok(items.into_iter().next().unwrap());
            }
            return Ok(TypeExpr::Tuple(items, pos));
        }
        let mut path = vec![if self.eat("Self") { "Self".to_string() } else { self.ident()? }];
        while self.eat(".") {
            path.push(self.ident()?);
        }
        let args = if self.eat("<") { self.comma_list(">", Self::ty)? } else { vec![] };
        Ok(TypeExpr::Path(path, args, pos))
    }

    // Statements:

    fn block(&mut self) -> PResult<Block> {
        let pos = self.pos();
        self.expect("{")?;
        let mut stmts = vec![];
        let mut tail = None;
        while !self.is("}") && !matches!(self.peek(), Tok::Eof) {
            let start = self.at;
            match self.stmt() {
                Ok(Parsed::Stmt(stmt)) => stmts.push(stmt),
                Ok(Parsed::Tail(expr)) => tail = Some(Box::new(expr)),
                Err(()) => {
                    self.sync();
                    if self.at == start {
                        self.next();
                    }
                }
            }
        }
        self.expect("}")?;
        Ok(Block { stmts, tail, pos })
    }

    fn stmt(&mut self) -> PResult<Parsed> {
        let pos = self.pos();
        let stmt = match self.peek() {
            Tok::Kw("let") => {
                self.next();
                let pattern = self.pattern(false)?;
                let ty = if self.eat(":") { Some(self.ty()?) } else { None };
                self.expect("=")?;
                let value = self.expr()?;
                self.expect_semicolon();
                Stmt::Let(pattern, ty, value)
            }
            Tok::Kw("var") => {
                self.next();
                let name = self.ident()?;
                let ty = if self.eat(":") { Some(self.ty()?) } else { None };
                self.expect("=")?;
                let value = self.expr()?;
                self.expect_semicolon();
                Stmt::Var(name, ty, value)
            }
            Tok::Kw("try") if matches!(self.peek_at(1), Tok::Sym("{")) => {
                self.next();
                let body = self.block()?;
                let mut catches = vec![];
                while self.eat("catch") {
                    self.expect("(")?;
                    let name = self.ident()?;
                    let mut filters = vec![];
                    if self.eat(":") {
                        filters.push(self.postfix()?);
                        while self.eat("|") {
                            filters.push(self.postfix()?);
                        }
                    }
                    self.expect(")")?;
                    catches.push(Catch { name, filters, body: self.block()? });
                }
                let finally = if self.eat("finally") { Some(self.block()?) } else { None };
                Stmt::Try(body, catches, finally, pos)
            }
            Tok::Kw("with") => {
                self.next();
                let mut items = vec![];
                loop {
                    let manager = self.binary(0, true)?;
                    let name = if self.eat("as") { Some((self.ident()?, self.tokens[self.at - 1].pos)) } else { None };
                    items.push((manager, name));
                    if !self.eat(",") {
                        break;
                    }
                }
                Stmt::With(items, self.block()?, pos)
            }
            Tok::Kw("fn")
            | Tok::Kw("struct")
            | Tok::Kw("enum")
            | Tok::Kw("trait")
            | Tok::Kw("impl")
            | Tok::Kw("const")
            | Tok::Kw("type")
            | Tok::Sym("@") => Stmt::Item(self.item()?),
            Tok::Kw("async") if matches!(self.peek_at(1), Tok::Kw("fn")) => Stmt::Item(self.item()?),
            Tok::Sym(";") => {
                self.next();
                Stmt::Expr(Expr { kind: ExprKind::Unit, pos })
            }
            Tok::Kw("if") | Tok::Kw("match") | Tok::Kw("while") | Tok::Kw("for") | Tok::Kw("loop") | Tok::Sym("{") => {
                let expr = self.primary()?;
                if self.is("}") {
                    return Ok(Parsed::Tail(expr));
                }
                self.eat(";");
                Stmt::Expr(expr)
            }
            _ => {
                let expr = self.expr()?;
                let compound = match self.peek() {
                    Tok::Sym(sym)
                        if sym.len() >= 2 && sym.ends_with('=') && !matches!(*sym, "==" | "!=" | "<=" | ">=") =>
                    {
                        binop_of(&sym[..sym.len() - 1])
                    }
                    _ => None,
                };
                if self.is("=") || compound.is_some() {
                    let pos = self.pos();
                    self.next();
                    let value = self.expr()?;
                    self.expect_semicolon();
                    Stmt::Assign(expr, compound, value, pos)
                } else if self.is("}") {
                    return Ok(Parsed::Tail(expr));
                } else {
                    self.expect_semicolon();
                    Stmt::Expr(expr)
                }
            }
        };
        Ok(Parsed::Stmt(stmt))
    }

    // Expressions:

    pub fn expr(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        let kind = match self.peek() {
            Tok::Kw("return") => {
                self.next();
                ExprKind::Return(self.optional_expr()?)
            }
            Tok::Kw("break") => {
                self.next();
                ExprKind::Break(self.optional_expr()?)
            }
            Tok::Kw("continue") => {
                self.next();
                ExprKind::Continue
            }
            Tok::Kw("throw") => {
                self.next();
                ExprKind::Throw(Box::new(self.expr()?))
            }
            Tok::Kw("try") => {
                self.next();
                ExprKind::TryExpr(Box::new(self.binary(0, false)?))
            }
            Tok::Ident(_) if matches!(self.peek_at(1), Tok::Sym("=>")) => {
                let name = self.ident()?;
                self.next();
                ExprKind::Lambda(vec![(name, pos, None)], Box::new(self.lambda_body()?))
            }
            Tok::Sym("(") if self.lambda_ahead() => {
                self.next();
                let params = self.comma_list(")", |p| {
                    let pos = p.pos();
                    let name = p.ident()?;
                    let ty = if p.eat(":") { Some(p.ty()?) } else { None };
                    Ok((name, pos, ty))
                })?;
                self.expect("=>")?;
                ExprKind::Lambda(params, Box::new(self.lambda_body()?))
            }
            _ => return self.binary(0, false),
        };
        Ok(Expr { kind, pos })
    }

    fn optional_expr(&mut self) -> PResult<Option<Box<Expr>>> {
        if matches!(self.peek(), Tok::Sym(";") | Tok::Sym("}") | Tok::Sym(")") | Tok::Sym(",")) {
            Ok(None)
        } else {
            Ok(Some(Box::new(self.expr()?)))
        }
    }

    fn lambda_ahead(&self) -> bool {
        let mut depth = 0;
        for (index, token) in self.tokens[self.at..].iter().enumerate() {
            match token.tok {
                Tok::Sym("(") => depth += 1,
                Tok::Sym(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.peek_at(index + 1), Tok::Sym("=>"));
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn lambda_body(&mut self) -> PResult<Expr> {
        if self.is("{") {
            let pos = self.pos();
            return Ok(Expr { kind: ExprKind::Block(self.block()?), pos });
        }
        self.expr_or_assign()
    }

    // Match arms and lambdas may consist of a single assignment, which becomes a block.
    fn expr_or_assign(&mut self) -> PResult<Expr> {
        let target = self.expr()?;
        let pos = self.pos();
        let compound = match self.peek() {
            Tok::Sym(sym) if sym.len() >= 2 && sym.ends_with('=') && !matches!(*sym, "==" | "!=" | "<=" | ">=") => {
                binop_of(&sym[..sym.len() - 1])
            }
            _ => None,
        };
        if !self.is("=") && compound.is_none() {
            return Ok(target);
        }
        self.next();
        let value = self.expr()?;
        let assign = Stmt::Assign(target, compound, value, pos);
        Ok(Expr { kind: ExprKind::Block(Block { stmts: vec![assign], tail: None, pos }), pos })
    }

    fn binary(&mut self, min_prec: u8, no_as: bool) -> PResult<Expr> {
        let mut left = self.unary()?;
        loop {
            let pos = self.pos();
            let sym = match self.peek() {
                Tok::Sym(sym) | Tok::Kw(sym) => *sym,
                _ => break,
            };
            if matches!(sym, "as" | "as!" | "as?") {
                if 12 < min_prec || (no_as && sym == "as") {
                    break;
                }
                self.next();
                let kind = match sym {
                    "as" => CastKind::Static,
                    "as!" => CastKind::Checked,
                    _ => CastKind::Optional,
                };
                left = Expr { kind: ExprKind::Cast(Box::new(left), kind, self.ty()?), pos };
                continue;
            }
            let prec = match sym {
                "|>" => 1,
                ".." | "..=" => 2,
                "||" => 3,
                "&&" => 4,
                "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" => 5,
                "|" => 6,
                "^" => 7,
                "&" => 8,
                "<<" | ">>" => 9,
                "+" | "-" => 10,
                "*" | "/" | "~/" | "%" | "@" => 11,
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.next();
            let right = if prec == 1 { self.binary(2, no_as)? } else { self.binary(prec + 1, no_as)? };
            let kind = match sym {
                "|>" => ExprKind::Pipe(Box::new(left), Box::new(right)),
                ".." | "..=" => ExprKind::Range(Box::new(left), Box::new(right), sym == "..="),
                _ => ExprKind::Binary(binop_of(sym).unwrap(), Box::new(left), Box::new(right)),
            };
            left = Expr { kind, pos };
        }
        Ok(left)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        let op = match self.peek() {
            Tok::Sym("-") => Some(UnOp::Neg),
            Tok::Sym("!") => Some(UnOp::Not),
            Tok::Sym("~") => Some(UnOp::BitNot),
            Tok::Kw("await") => {
                self.next();
                return Ok(Expr { kind: ExprKind::Await(Box::new(self.unary()?)), pos });
            }
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            return Ok(Expr { kind: ExprKind::Unary(op, Box::new(self.unary()?)), pos });
        }
        let base = self.postfix()?;
        if self.is("**") {
            let pos = self.pos();
            self.next();
            return Ok(Expr { kind: ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(self.unary()?)), pos });
        }
        Ok(base)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.primary()?;
        loop {
            let pos = self.pos();
            let kind = if self.is("(") {
                self.next();
                ExprKind::Call(Box::new(expr), vec![], self.comma_list(")", Self::arg)?)
            } else if self.is("<") && matches!(expr.kind, ExprKind::Name(_) | ExprKind::Member(..)) {
                match self.speculative_type_args() {
                    Some(types) => {
                        self.expect("(")?;
                        ExprKind::Call(Box::new(expr), types, self.comma_list(")", Self::arg)?)
                    }
                    None => break,
                }
            } else if self.eat("[") {
                ExprKind::Index(Box::new(expr), self.comma_list("]", Self::index_arg)?)
            } else if self.eat(".") {
                if let Tok::Int(index) = self.peek().clone() {
                    self.next();
                    ExprKind::TupleField(Box::new(expr), index as usize)
                } else {
                    ExprKind::Member(Box::new(expr), self.name_or_keyword()?)
                }
            } else if self.eat("?") {
                ExprKind::Question(Box::new(expr))
            } else {
                break;
            };
            expr = Expr { kind, pos };
        }
        Ok(expr)
    }

    fn speculative_type_args(&mut self) -> Option<Vec<TypeExpr>> {
        let (start, errors) = (self.at, self.errors.len());
        self.next();
        let result = self.comma_list(">", Self::ty);
        if result.is_ok() && self.is("(") {
            return result.ok();
        }
        self.at = start;
        self.errors.truncate(errors);
        None
    }

    fn arg(&mut self) -> PResult<Arg> {
        if self.eat("**") {
            return Ok(Arg::StarStar(self.expr()?));
        }
        if self.eat("*") {
            return Ok(Arg::Star(self.expr()?));
        }
        if matches!(self.peek(), Tok::Ident(_) | Tok::Kw(_)) && matches!(self.peek_at(1), Tok::Sym("=")) {
            let name = self.name_or_keyword()?;
            self.next();
            return Ok(Arg::Named(name, self.expr()?));
        }
        Ok(Arg::Pos(self.expr()?))
    }

    fn index_arg(&mut self) -> PResult<IndexArg> {
        if self.eat("...") {
            return Ok(IndexArg::Ellipsis);
        }
        let start = if self.is(":") { None } else { Some(self.expr()?) };
        if !self.eat(":") {
            return Ok(IndexArg::Expr(start.unwrap()));
        }
        let bound = |p: &mut Self| {
            if matches!(p.peek(), Tok::Sym(":") | Tok::Sym(",") | Tok::Sym("]")) {
                Ok(None)
            } else {
                p.expr().map(Some)
            }
        };
        let stop = bound(self)?;
        let step = if self.eat(":") { bound(self)? } else { None };
        Ok(IndexArg::Slice(start, stop, step))
    }

    fn primary(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        let kind = match self.next() {
            Tok::Int(value) => ExprKind::Int(value),
            Tok::Float(value) => ExprKind::Float(value),
            Tok::Str(text) => ExprKind::Str(text),
            Tok::Bytes(bytes) => ExprKind::Bytes(bytes),
            Tok::Template(parts) => ExprKind::Template(self.template(parts)?),
            Tok::Kw("true") => ExprKind::Bool(true),
            Tok::Kw("false") => ExprKind::Bool(false),
            Tok::Kw("self") => ExprKind::SelfValue,
            Tok::Kw("super") => ExprKind::Super,
            Tok::Kw("Self") => ExprKind::Name("Self".into()),
            Tok::Ident(name) => ExprKind::Name(name),
            Tok::Sym("(") => {
                if self.eat(")") {
                    ExprKind::Unit
                } else {
                    let first = self.expr()?;
                    if self.eat(")") {
                        return Ok(first);
                    }
                    self.expect(",")?;
                    let mut items = vec![first];
                    items.extend(self.comma_list(")", Self::expr)?);
                    ExprKind::Tuple(items)
                }
            }
            Tok::Sym("[") => {
                if self.eat(":") {
                    self.expect("]")?;
                    ExprKind::Map(vec![])
                } else if self.eat("]") {
                    ExprKind::List(vec![])
                } else {
                    let first = self.expr()?;
                    if self.eat(":") {
                        let value = self.expr()?;
                        let mut entries = vec![(first, value)];
                        if self.eat(",") {
                            entries.extend(self.comma_list("]", |p| {
                                let key = p.expr()?;
                                p.expect(":")?;
                                Ok((key, p.expr()?))
                            })?);
                        } else {
                            self.expect("]")?;
                        }
                        ExprKind::Map(entries)
                    } else {
                        let mut items = vec![first];
                        if self.eat(",") {
                            items.extend(self.comma_list("]", Self::expr)?);
                        } else {
                            self.expect("]")?;
                        }
                        ExprKind::List(items)
                    }
                }
            }
            Tok::Sym("{") => {
                self.at -= 1;
                ExprKind::Block(self.block()?)
            }
            Tok::Kw("if") => self.if_expr()?,
            Tok::Kw("match") => {
                let scrutinee = self.binary(0, false)?;
                self.expect("{")?;
                let mut arms = vec![];
                while !self.eat("}") {
                    let pattern = self.pattern(true)?;
                    let guard = if self.eat("if") { Some(self.expr()?) } else { None };
                    self.expect("=>")?;
                    let block_body = self.is("{");
                    let body = self.expr_or_assign()?;
                    if !self.eat(",") && !block_body && !self.is("}") {
                        self.expect(",")?;
                    }
                    arms.push(Arm { pattern, guard, body });
                }
                ExprKind::Match(Box::new(scrutinee), arms)
            }
            Tok::Kw("while") => {
                let cond = self.cond()?;
                ExprKind::While(Box::new(cond), self.block()?)
            }
            Tok::Kw("for") => {
                let pattern = self.pattern(false)?;
                self.expect("in")?;
                let iterable = self.binary(0, false)?;
                ExprKind::For(Box::new(pattern), Box::new(iterable), self.block()?)
            }
            Tok::Kw("loop") => ExprKind::Loop(self.block()?),
            other => {
                self.at -= 1;
                return self.error(pos, format!("expected expression, found {}", Self::describe(&other)));
            }
        };
        Ok(Expr { kind, pos })
    }

    fn if_expr(&mut self) -> PResult<ExprKind> {
        let cond = self.cond()?;
        let then = self.block()?;
        let otherwise = if self.eat("else") {
            let pos = self.pos();
            let kind = if self.eat("if") { self.if_expr()? } else { ExprKind::Block(self.block()?) };
            Some(Box::new(Expr { kind, pos }))
        } else {
            None
        };
        Ok(ExprKind::If(Box::new(cond), then, otherwise))
    }

    fn cond(&mut self) -> PResult<Cond> {
        if self.eat("let") {
            let pattern = self.pattern(true)?;
            self.expect("=")?;
            return Ok(Cond::Let(pattern, self.binary(0, false)?));
        }
        Ok(Cond::Expr(self.binary(0, false)?))
    }

    fn template(&mut self, parts: Vec<TemplatePart>) -> PResult<Vec<TemplatePiece>> {
        let mut pieces = vec![];
        for part in parts {
            match part {
                TemplatePart::Text(text) => pieces.push(TemplatePiece::Text(text)),
                TemplatePart::Code(code, pos) => {
                    let tokens = match lex(&code, pos) {
                        Ok(tokens) => tokens,
                        Err((pos, message)) => return self.error(pos, message),
                    };
                    let mut inner = Parser { tokens, at: 0, errors: vec![] };
                    let expr = inner.expr();
                    if expr.is_ok() && !matches!(inner.peek(), Tok::Eof) {
                        inner.errors.push((inner.pos(), "unexpected token in template".into()));
                    }
                    self.errors.append(&mut inner.errors);
                    pieces.push(TemplatePiece::Code(expr?));
                }
            }
        }
        Ok(pieces)
    }

    // Patterns:

    fn pattern(&mut self, type_tests: bool) -> PResult<Pattern> {
        let pos = self.pos();
        let first = self.pattern_alt(type_tests)?;
        if !self.is("|") {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.eat("|") {
            alternatives.push(self.pattern_alt(type_tests)?);
        }
        Ok(Pattern { kind: PatKind::Or(alternatives), pos })
    }

    fn pattern_alt(&mut self, type_tests: bool) -> PResult<Pattern> {
        let pos = self.pos();
        let kind = match self.peek().clone() {
            Tok::Ident(name) if name == "_" => {
                self.next();
                PatKind::Wild
            }
            Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Kw("true") | Tok::Kw("false") => {
                PatKind::Lit(Box::new(self.primary()?))
            }
            Tok::Sym("-") => {
                self.next();
                let literal = self.primary()?;
                PatKind::Lit(Box::new(Expr { kind: ExprKind::Unary(UnOp::Neg, Box::new(literal)), pos }))
            }
            Tok::Sym("(") => {
                self.next();
                let items = self.comma_list(")", |p| p.sub_pattern(type_tests))?;
                let trailing_comma = matches!(self.tokens[self.at - 2].tok, Tok::Sym(","));
                if items.len() == 1 && !trailing_comma {
                    if let SubPat::Pat(inner) = &items[0] {
                        return Ok(inner.clone());
                    }
                }
                PatKind::Tuple(items)
            }
            Tok::Sym("[") => {
                self.next();
                PatKind::List(self.comma_list("]", |p| p.sub_pattern(type_tests))?)
            }
            Tok::Ident(name) => {
                self.next();
                if type_tests && self.eat(":") {
                    return Ok(Pattern { kind: PatKind::TypeTest(name, self.ty()?), pos });
                }
                let mut path = vec![name];
                while self.eat(".") {
                    path.push(self.ident()?);
                }
                if self.eat("(") {
                    PatKind::Ctor(path, self.comma_list(")", |p| p.sub_pattern(type_tests))?)
                } else if path.len() == 1 {
                    PatKind::Ident(path.pop().unwrap())
                } else {
                    PatKind::Ctor(path, vec![])
                }
            }
            other => return self.error(pos, format!("expected pattern, found {}", Self::describe(&other))),
        };
        Ok(Pattern { kind, pos })
    }

    fn sub_pattern(&mut self, type_tests: bool) -> PResult<SubPat> {
        if self.eat("..") {
            return Ok(SubPat::Rest);
        }
        if let (Tok::Ident(name), Tok::Sym("=")) = (self.peek().clone(), self.peek_at(1)) {
            self.next();
            self.next();
            return Ok(SubPat::Named(name, self.pattern(type_tests)?));
        }
        Ok(SubPat::Pat(self.pattern(type_tests)?))
    }
}

enum Parsed {
    Stmt(Stmt),
    Tail(Expr),
}

fn binop_of(sym: &str) -> Option<BinOp> {
    use BinOp::*;
    Some(match sym {
        "+" => Add,
        "-" => Sub,
        "*" => Mul,
        "/" => Div,
        "~/" => FloorDiv,
        "%" => Mod,
        "**" => Pow,
        "@" => MatMul,
        "&" => BitAnd,
        "|" => BitOr,
        "^" => BitXor,
        "<<" => Shl,
        ">>" => Shr,
        "==" => Eq,
        "!=" => Ne,
        "<" => Lt,
        "<=" => Le,
        ">" => Gt,
        ">=" => Ge,
        "in" => In,
        "&&" => And,
        "||" => Or,
        _ => return None,
    })
}
