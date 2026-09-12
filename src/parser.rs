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

    fn import_from(&mut self, py: bool, star: Option<String>, names: Vec<(String, String, Pos)>, pos: Pos) -> PResult<Import> {
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
            return Ok(Param { name: "self".into(), pos, kind: ParamKind::SelfParam, ty: None, default: None, optional: false, var: false });
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
            Tok::Kw("fn") | Tok::Kw("struct") | Tok::Kw("enum") | Tok::Kw("trait") | Tok::Kw("impl")
            | Tok::Kw("const") | Tok::Kw("type") | Tok::Sym("@") => Stmt::Item(self.item()?),
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
                    Tok::Sym(sym) if sym.len() >= 2 && sym.ends_with('=') && !matches!(*sym, "==" | "!=" | "<=" | ">=") => {
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
}
