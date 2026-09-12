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
}
