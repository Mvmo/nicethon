#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Kw(&'static str),
    Sym(&'static str),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Template(Vec<TemplatePart>),
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TemplatePart {
    Text(String),
    Code(String, u32),
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub pos: u32,
    pub end: u32,
}

pub const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "catch", "class", "const", "continue", "else", "enum", "export",
    "extern", "false", "finally", "fn", "for", "from", "if", "impl", "import", "in", "let", "loop",
    "match", "py", "return", "self", "Self", "struct", "super", "throw", "trait", "true", "try",
    "type", "var", "while", "with",
];

const SYMBOLS: &[&str] = &[
    "...", "..=", "**=", "~/=", "<<=", ">>=", "=>", "->", "|>", "..", "==", "!=", "<=", ">=", "&&", "||",
    "**", "~/", "<<", ">>", "+=", "-=", "*=", "/=", "%=", "@=", "&=", "|=", "^=", "+", "-", "*", "/",
    "%", "@", "&", "|", "^", "~", "!", "<", ">", "=", ".", ",", ";", ":", "?", "(", ")", "[", "]", "{",
    "}",
];

pub fn lex(src: &str, base: u32) -> Result<Vec<Token>, (u32, String)> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if c.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        let pos = base + at as u32;
        if src[at..].starts_with("//") {
            at = src[at..].find('\n').map_or(bytes.len(), |n| at + n);
            continue;
        }
        if src[at..].starts_with("/*") {
            let mut depth = 0;
            while at < bytes.len() {
                if src[at..].starts_with("/*") {
                    depth += 1;
                    at += 2;
                } else if src[at..].starts_with("*/") {
                    depth -= 1;
                    at += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    at += 1;
                }
            }
            if depth != 0 {
                return Err((pos, "unterminated comment".into()));
            }
            continue;
        }
        let tok = if c.is_ascii_alphabetic() || c == b'_' {
            if c == b'b' && bytes.get(at + 1) == Some(&b'"') {
                let (text, next) = string_literal(src, at + 1, b'"').map_err(|m| (pos, m))?;
                at = next;
                Tok::Bytes(text.into_bytes())
            } else {
                while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                    at += 1;
                }
                let word = &src[start..at];
                if word == "as" && matches!(bytes.get(at), Some(b'!') | Some(b'?')) && bytes.get(at + 1) != Some(&b'=') {
                    at += 1;
                    Tok::Sym(if bytes[at - 1] == b'!' { "as!" } else { "as?" })
                } else if let Some(kw) = KEYWORDS.iter().find(|kw| **kw == word) {
                    Tok::Kw(kw)
                } else {
                    Tok::Ident(word.to_string())
                }
            }
        } else if c.is_ascii_digit() {
            let after_dot = matches!(tokens.last(), Some(Token { tok: Tok::Sym("."), .. }));
            let (tok, next) = number(src, at, after_dot).map_err(|m| (pos, m))?;
            at = next;
            tok
        } else if c == b'"' {
            let (text, next) = string_literal(src, at, b'"').map_err(|m| (pos, m))?;
            at = next;
            Tok::Str(text)
        } else if c == b'`' {
            let (parts, next) = template(src, at, base).map_err(|m| (pos, m))?;
            at = next;
            Tok::Template(parts)
        } else if let Some(sym) = SYMBOLS.iter().find(|s| src[at..].starts_with(**s)) {
            at += sym.len();
            Tok::Sym(sym)
        } else {
            return Err((pos, format!("unexpected character '{}'", &src[at..].chars().next().unwrap())));
        };
        tokens.push(Token { tok, pos, end: base + at as u32 });
    }
    tokens.push(Token { tok: Tok::Eof, pos: base + bytes.len() as u32, end: base + bytes.len() as u32 });
    Ok(tokens)
}
