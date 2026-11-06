//! A small JSON reader that keeps the original lexeme of every number (spec §5: numbers printed
//! without filters must keep their source text, e.g. `12345678901234567890`).

use crate::error::{Error, ErrorCode, Result};
use serde_json::{Map, Number, Value as SJson};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// Number lexeme exactly as written.
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    /// Keys in source order; on duplicates the last value wins (like `JSON.parse`).
    Obj(Vec<(String, Json)>),
}

/// Nesting limit: keeps recursion well inside the 1 MiB wasm stack.
const MAX_DEPTH: usize = 128;

pub fn parse(text: &str) -> Result<Json> {
    let mut p = Parser { s: text.as_bytes(), i: 0, src: text };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("unexpected trailing characters"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    src: &'a str,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> Error {
        Error::new(ErrorCode::InvalidRequest, format!("Invalid JSON at offset {}: {msg}", self.i))
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'n') if self.eat("null") => Ok(Json::Null),
            Some(b't') if self.eat("true") => Ok(Json::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Json::Bool(false)),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                loop {
                    self.ws();
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(out));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut out: Vec<(String, Json)> = Vec::new();
                let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(out));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected a key"));
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    if let Some(&at) = index.get(&k) {
                        out[at].1 = v;
                    } else {
                        index.insert(k.clone(), out.len());
                        out.push((k, v));
                    }
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(out));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.err("unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Json> {
        let st = self.i;
        let s = self.s;
        if s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match s.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(c) if c.is_ascii_digit() => {
                while self.i < s.len() && s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            }
            _ => return Err(self.err("invalid number")),
        }
        if s.get(self.i) == Some(&b'.') {
            self.i += 1;
            let d = self.i;
            while self.i < s.len() && s[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if d == self.i {
                return Err(self.err("invalid number"));
            }
        }
        if matches!(s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let d = self.i;
            while self.i < s.len() && s[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if d == self.i {
                return Err(self.err("invalid number"));
            }
        }
        Ok(Json::Num(self.src[st..self.i].to_string()))
    }

    fn hex4(&mut self) -> Result<u32> {
        if self.i + 4 > self.s.len() {
            return Err(self.err("bad \\u escape"));
        }
        let h = std::str::from_utf8(&self.s[self.i..self.i + 4]).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let st = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\' && self.s[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(&self.src[st..self.i]);
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.s.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    self.i = save;
                                    0xFFFD
                                }
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                Some(_) => return Err(self.err("control character in string")),
                None => return Err(self.err("unterminated string")),
            }
        }
    }
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Converts to a `serde_json::Value` (for typed deserialization of options and models).
    pub fn to_serde(&self) -> SJson {
        match self {
            Json::Null => SJson::Null,
            Json::Bool(b) => SJson::Bool(*b),
            Json::Num(lex) => {
                if let Ok(i) = lex.parse::<i64>() {
                    SJson::from(i)
                } else if let Ok(u) = lex.parse::<u64>() {
                    SJson::from(u)
                } else {
                    lex.parse::<f64>().ok().and_then(Number::from_f64).map(SJson::Number).unwrap_or(SJson::Null)
                }
            }
            Json::Str(s) => SJson::String(s.clone()),
            Json::Arr(a) => SJson::Array(a.iter().map(Json::to_serde).collect()),
            Json::Obj(o) => {
                let mut m = Map::new();
                for (k, v) in o {
                    m.insert(k.clone(), v.to_serde());
                }
                SJson::Object(m)
            }
        }
    }

    pub fn from_serde(v: &SJson) -> Json {
        match v {
            SJson::Null => Json::Null,
            SJson::Bool(b) => Json::Bool(*b),
            SJson::Number(n) => Json::Num(n.to_string()),
            SJson::String(s) => Json::Str(s.clone()),
            SJson::Array(a) => Json::Arr(a.iter().map(Json::from_serde).collect()),
            SJson::Object(o) => Json::Obj(o.iter().map(|(k, v)| (k.clone(), Json::from_serde(v))).collect()),
        }
    }
}

/// Appends `s` as a JSON string literal (same escaping as `JSON.stringify`).
pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Writes a `Json` tree as compact text, keeping number lexemes.
pub fn write(out: &mut String, v: &Json) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(n),
        Json::Str(s) => write_str(out, s),
        Json::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(out, x);
            }
            out.push(']');
        }
        Json::Obj(o) => {
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write(out, x);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_number_lexemes() {
        let v = parse(r#"{"a": 12345678901234567890, "b": [1.50, -0, 1e3], "s": "xA😀\n"}"#).unwrap();
        assert_eq!(v.get("a"), Some(&Json::Num("12345678901234567890".into())));
        let mut out = String::new();
        write(&mut out, &v);
        assert_eq!(out, "{\"a\":12345678901234567890,\"b\":[1.50,-0,1e3],\"s\":\"xA😀\\n\"}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("01").is_err());
        assert!(parse("\"a").is_err());
        assert!(parse(&"[".repeat(1000)).is_err());
    }
}
