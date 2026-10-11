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
    "as", "async", "await", "break", "catch", "class", "const", "continue", "else", "enum", "export", "extern",
    "false", "finally", "fn", "for", "from", "if", "impl", "import", "in", "let", "loop", "match", "py", "return",
    "self", "Self", "struct", "super", "throw", "trait", "true", "try", "type", "var", "while", "with",
];

const SYMBOLS: &[&str] = &[
    "...", "..=", "**=", "~/=", "<<=", ">>=", "=>", "->", "|>", "..", "==", "!=", "<=", ">=", "&&", "||", "**", "~/",
    "<<", ">>", "+=", "-=", "*=", "/=", "%=", "@=", "&=", "|=", "^=", "+", "-", "*", "/", "%", "@", "&", "|", "^", "~",
    "!", "<", ">", "=", ".", ",", ";", ":", "?", "(", ")", "[", "]", "{", "}",
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
                if word == "as" && matches!(bytes.get(at), Some(b'!') | Some(b'?')) && bytes.get(at + 1) != Some(&b'=')
                {
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

fn number(src: &str, start: usize, int_only: bool) -> Result<(Tok, usize), String> {
    let bytes = src.as_bytes();
    let mut at = start;
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if src[at..].starts_with(prefix) {
            at += 2;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let digits = src[start + 2..at].replace('_', "");
            let value = i64::from_str_radix(&digits, radix).map_err(|_| "invalid integer literal")?;
            return Ok((Tok::Int(value), at));
        }
    }
    let digits = |at: &mut usize| {
        while *at < bytes.len() && (bytes[*at].is_ascii_digit() || bytes[*at] == b'_') {
            *at += 1;
        }
    };
    digits(&mut at);
    let mut is_float = false;
    if !int_only && bytes.get(at) == Some(&b'.') && bytes.get(at + 1).is_some_and(|b| b.is_ascii_digit()) {
        is_float = true;
        at += 1;
        digits(&mut at);
    }
    if !int_only && matches!(bytes.get(at), Some(b'e') | Some(b'E')) {
        let mut probe = at + 1;
        if matches!(bytes.get(probe), Some(b'+') | Some(b'-')) {
            probe += 1;
        }
        if bytes.get(probe).is_some_and(|b| b.is_ascii_digit()) {
            is_float = true;
            at = probe;
            digits(&mut at);
        }
    }
    let text = src[start..at].replace('_', "");
    let tok = if is_float {
        Tok::Float(text.parse().map_err(|_| "invalid float literal")?)
    } else {
        Tok::Int(text.parse().map_err(|_| "integer literal too large for Int")?)
    };
    Ok((tok, at))
}

fn escape(bytes: &[u8], at: &mut usize, out: &mut String) -> Result<(), String> {
    let c = bytes[*at];
    *at += 1;
    match c {
        b'n' => out.push('\n'),
        b't' => out.push('\t'),
        b'r' => out.push('\r'),
        b'0' => out.push('\0'),
        b'\\' | b'"' | b'`' | b'$' => out.push(c as char),
        b'x' | b'u' => {
            let (open, close) = if c == b'u' { (b'{', b'}') } else { (0, 0) };
            let digits_start = if open != 0 {
                if bytes.get(*at) != Some(&open) {
                    return Err("expected { after \\u".into());
                }
                *at + 1
            } else {
                *at
            };
            let mut end = digits_start;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() && (open != 0 || end < digits_start + 2) {
                end += 1;
            }
            let code = u32::from_str_radix(std::str::from_utf8(&bytes[digits_start..end]).unwrap(), 16)
                .map_err(|_| "invalid escape")?;
            out.push(char::from_u32(code).ok_or("invalid unicode escape")?);
            *at = end;
            if open != 0 {
                if bytes.get(*at) != Some(&close) {
                    return Err("expected } in \\u escape".into());
                }
                *at += 1;
            }
        }
        _ => return Err(format!("unknown escape \\{}", c as char)),
    }
    Ok(())
}

fn string_literal(src: &str, start: usize, quote: u8) -> Result<(String, usize), String> {
    let bytes = src.as_bytes();
    let mut at = start + 1;
    let mut out = String::new();
    loop {
        match bytes.get(at) {
            None => return Err("unterminated string".into()),
            Some(&c) if c == quote => return Ok((out, at + 1)),
            Some(b'\\') => {
                at += 1;
                escape(bytes, &mut at, &mut out)?;
            }
            Some(_) => {
                let ch = src[at..].chars().next().unwrap();
                out.push(ch);
                at += ch.len_utf8();
            }
        }
    }
}

fn template(src: &str, start: usize, base: u32) -> Result<(Vec<TemplatePart>, usize), String> {
    let bytes = src.as_bytes();
    let mut at = start + 1;
    let mut parts = Vec::new();
    let mut text = String::new();
    loop {
        match bytes.get(at) {
            None => return Err("unterminated template string".into()),
            Some(b'`') => break,
            Some(b'\\') => {
                at += 1;
                escape(bytes, &mut at, &mut text)?;
            }
            Some(b'$') if bytes.get(at + 1) == Some(&b'{') => {
                if !text.is_empty() {
                    parts.push(TemplatePart::Text(std::mem::take(&mut text)));
                }
                let code_start = at + 2;
                let mut depth = 1;
                at = code_start;
                while depth > 0 {
                    match bytes.get(at) {
                        None => return Err("unterminated ${ in template string".into()),
                        Some(b'{') => depth += 1,
                        Some(b'}') => depth -= 1,
                        Some(b'"') => at = string_literal(src, at, b'"')?.1 - 1,
                        _ => {}
                    }
                    at += 1;
                }
                parts.push(TemplatePart::Code(src[code_start..at - 1].to_string(), base + code_start as u32));
            }
            Some(_) => {
                let ch = src[at..].chars().next().unwrap();
                text.push(ch);
                at += ch.len_utf8();
            }
        }
    }
    if !text.is_empty() || parts.is_empty() {
        parts.push(TemplatePart::Text(text));
    }
    Ok((parts, at + 1))
}
