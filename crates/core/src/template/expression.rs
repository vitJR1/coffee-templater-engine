//! Expression language used inside tags:
//!
//! ```text
//!   path            client.name, items[0].title, this, ., @index, ../title
//!   literal         "text", 'text', «text», “text”, 42, -1.5, true, false, null
//!   filters         value | upper | default: "—" | date: "DD.MM.YYYY"
//! ```

use crate::value::Value;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Key(String),
    Index(i64),
}

#[derive(Debug, Clone)]
pub enum Operand {
    Literal(Value),
    Path(PathOp),
}

#[derive(Debug, Clone)]
pub struct PathOp {
    pub up: usize,
    pub self_: bool,
    pub local: Option<String>,
    pub segments: Vec<Segment>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct FilterCall {
    pub name: String,
    pub args: Vec<Operand>,
}

#[derive(Debug, Clone)]
pub struct Expression {
    pub source: String,
    /// Main operand path, e.g. `client.name` for `client.name | upper`. Empty for literals.
    pub path: String,
    /// Every path referenced by the expression (operand and filter arguments).
    pub paths: Vec<String>,
    pub filters: Vec<String>,
    pub operand: Operand,
    pub calls: Vec<FilterCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpressionError(pub String);

// ---------------------------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------------------------

/// Data scope of a section. Loops push a child scope with loop locals (`@index`, ...).
pub struct Scope<'a> {
    pub data: Value,
    pub parent: Option<&'a Scope<'a>>,
    pub locals: Vec<(&'static str, Value)>,
}

impl<'a> Scope<'a> {
    pub fn root(data: Value) -> Scope<'static> {
        Scope { data, parent: None, locals: Vec::new() }
    }

    pub fn child(&'a self, data: Value, locals: Vec<(&'static str, Value)>) -> Scope<'a> {
        Scope { data, parent: Some(self), locals }
    }

    fn local(&self, name: &str) -> Option<&Value> {
        self.locals.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
    }
}

// ---------------------------------------------------------------------------------------------
// Lexing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Num(f64),
    Word(String),
    Punct(char),
}

fn close_quote(c: char) -> Option<char> {
    Some(match c {
        '"' => '"',
        '\'' => '\'',
        '“' => '”',
        '„' => '“',
        '«' => '»',
        '‘' => '’',
        '”' => '”',
        _ => return None,
    })
}

pub(crate) fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric() || matches!(c, '_' | '$' | '@' | '.' | '[' | ']' | '/' | '-')
}

/// `-?\d+(\.\d+)?` not followed by a letter, `_` or `$`. Returns the byte length.
fn number_at(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i = 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds {
        return None;
    }
    if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    match s[i..].chars().next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '$' => None,
        _ => Some(i),
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, ExpressionError> {
    let mut toks = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        let ch = rest.chars().next().unwrap_or(' ');
        if is_js_space(ch) {
            i += ch.len_utf8();
            continue;
        }
        if ch == '|' || ch == ':' || ch == ',' {
            toks.push(Tok::Punct(ch));
            i += 1;
            continue;
        }
        if let Some(close) = close_quote(ch) {
            let mut value = String::new();
            let mut chars = rest.char_indices().skip(1).peekable();
            let mut end = None;
            while let Some((j, c)) = chars.next() {
                if c == close || (close == '”' && c == '"') {
                    end = Some(j + c.len_utf8());
                    break;
                }
                if c == '\\' {
                    if let Some((_, n)) = chars.next() {
                        value.push(n);
                        continue;
                    }
                    // a trailing backslash is kept and the string stays unterminated
                    value.push(c);
                    break;
                }
                value.push(c);
            }
            let Some(end) = end else {
                return Err(ExpressionError(format!("Unterminated string in \"{src}\"")));
            };
            toks.push(Tok::Str(value));
            i += end;
            continue;
        }
        if let Some(n) = number_at(rest) {
            toks.push(Tok::Num(rest[..n].parse().unwrap_or(f64::NAN)));
            i += n;
            continue;
        }
        // A path word; brackets may contain quoted keys with spaces: a["some key"]
        let mut bracket = false;
        let mut quote: Option<char> = None;
        let mut j = 0;
        for (k, c) in rest.char_indices() {
            j = k;
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if bracket && (c == '"' || c == '\'') {
                quote = Some(c);
            } else if c == '[' {
                bracket = true;
            } else if c == ']' {
                bracket = false;
            } else if !bracket && !is_word_char(c) {
                break;
            }
            j = k + c.len_utf8();
        }
        if j == 0 {
            return Err(ExpressionError(format!("Unexpected character \"{ch}\" in \"{src}\"")));
        }
        toks.push(Tok::Word(rest[..j].to_string()));
        i += j;
    }
    Ok(toks)
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

fn invalid_path(word: &str) -> ExpressionError {
    ExpressionError(format!("Invalid path \"{word}\""))
}

/// Splits `a.b[0]["k"]` into segments (`([^.[\]]+)|\[\s*(-?\d+|"..."|'...')\s*\]|\.`).
fn parse_segments(word: &str, rest: &str) -> Result<Vec<Segment>, ExpressionError> {
    let mut segs = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'.' => i += 1,
            b'[' => {
                let mut k = i + 1;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                let seg;
                if k < b.len() && (b[k] == b'"' || b[k] == b'\'') {
                    let q = b[k] as char;
                    let end = rest[k + 1..].find(q).ok_or_else(|| invalid_path(word))? + k + 1;
                    seg = Segment::Key(rest[k + 1..end].to_string());
                    k = end + 1;
                } else {
                    let st = k;
                    if k < b.len() && b[k] == b'-' {
                        k += 1;
                    }
                    let ds = k;
                    while k < b.len() && b[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k == ds {
                        return Err(invalid_path(word));
                    }
                    seg = Segment::Index(rest[st..k].parse().map_err(|_| invalid_path(word))?);
                }
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                if k >= b.len() || b[k] != b']' {
                    return Err(invalid_path(word));
                }
                segs.push(seg);
                i = k + 1;
            }
            b']' => return Err(invalid_path(word)),
            _ => {
                let st = i;
                while i < b.len() && !matches!(b[i], b'.' | b'[' | b']') {
                    i += 1;
                }
                segs.push(Segment::Key(rest[st..i].to_string()));
            }
        }
    }
    Ok(segs)
}

fn parse_path(word: &str) -> Result<Operand, ExpressionError> {
    match word {
        "true" => return Ok(Operand::Literal(Value::Bool(true))),
        "false" => return Ok(Operand::Literal(Value::Bool(false))),
        "null" => return Ok(Operand::Literal(Value::Null)),
        "undefined" => return Ok(Operand::Literal(Value::Undefined)),
        _ => {}
    }
    let mut rest = word;
    let mut up = 0;
    while let Some(r) = rest.strip_prefix("../") {
        up += 1;
        rest = r;
    }
    let path = |up, self_, local, segments| Operand::Path(PathOp { up, self_, local, segments, source: word.to_string() });
    if rest == ".." {
        return Ok(path(up + 1, true, None, Vec::new()));
    }
    if rest == "." || rest == "this" {
        return Ok(path(up, true, None, Vec::new()));
    }
    let mut self_ = false;
    if let Some(r) = rest.strip_prefix("this.") {
        self_ = true;
        rest = r;
    }
    let mut segments = parse_segments(word, rest)?;
    if segments.is_empty() {
        return Err(invalid_path(word));
    }
    let mut local = None;
    if let Segment::Key(k) = &segments[0] {
        if let Some(name) = k.strip_prefix('@') {
            local = Some(name.to_string());
            segments.remove(0);
        }
    }
    Ok(path(up, self_, local, segments))
}

fn parse_operand(tok: Option<&Tok>, src: &str) -> Result<Operand, ExpressionError> {
    match tok {
        None => Err(ExpressionError(format!("Expected a value in \"{src}\""))),
        Some(Tok::Str(s)) => Ok(Operand::Literal(Value::str(s))),
        Some(Tok::Num(n)) => Ok(Operand::Literal(Value::num(*n))),
        Some(Tok::Word(w)) => parse_path(w),
        Some(Tok::Punct(p)) => Err(ExpressionError(format!("Unexpected \"{p}\" in \"{src}\""))),
    }
}

pub fn compile_expression(source: &str) -> Result<Expression, ExpressionError> {
    let toks = lex(source)?;
    if toks.is_empty() {
        return Err(ExpressionError("Empty expression".into()));
    }
    let mut i = 0;
    let operand = parse_operand(toks.get(i), source)?;
    i += 1;
    let mut calls = Vec::new();
    while i < toks.len() {
        if toks[i] != Tok::Punct('|') {
            return Err(ExpressionError(format!("Expected \"|\" in \"{source}\"")));
        }
        i += 1;
        let Some(Tok::Word(name)) = toks.get(i) else {
            return Err(ExpressionError(format!("Expected filter name after \"|\" in \"{source}\"")));
        };
        i += 1;
        let mut call = FilterCall { name: name.clone(), args: Vec::new() };
        if toks.get(i) == Some(&Tok::Punct(':')) {
            i += 1;
            call.args.push(parse_operand(toks.get(i), source)?);
            i += 1;
            while toks.get(i) == Some(&Tok::Punct(',')) {
                i += 1;
                call.args.push(parse_operand(toks.get(i), source)?);
                i += 1;
            }
        }
        calls.push(call);
    }
    let path_of = |op: &Operand| match op {
        Operand::Path(p) => p.source.clone(),
        Operand::Literal(_) => String::new(),
    };
    let paths = std::iter::once(&operand).chain(calls.iter().flat_map(|c| c.args.iter())).map(path_of).filter(|p| !p.is_empty()).collect();
    Ok(Expression {
        source: source.to_string(),
        path: path_of(&operand),
        paths,
        filters: calls.iter().map(|c| c.name.clone()).collect(),
        operand,
        calls,
    })
}

// ---------------------------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------------------------

fn utf_len(s: &str) -> usize {
    s.chars().count()
}

/// Property access: own properties only (no prototype), negative array indexes, `length`.
pub fn get(obj: &Value, key: &Segment) -> Value {
    match (obj, key) {
        (Value::Array(a), Segment::Index(i)) => {
            let idx = if *i < 0 { a.len() as i64 + i } else { *i };
            if idx < 0 {
                return Value::Undefined;
            }
            a.get(idx as usize).cloned().unwrap_or_default()
        }
        (Value::Array(a), Segment::Key(k)) => {
            if k == "length" {
                return Value::num(a.len() as f64);
            }
            match k.parse::<usize>() {
                Ok(i) if i.to_string() == *k => a.get(i).cloned().unwrap_or_default(),
                _ => Value::Undefined,
            }
        }
        (Value::String(s), Segment::Key(k)) if k == "length" => Value::num(utf_len(s) as f64),
        (Value::Object(o), Segment::Key(k)) => o.get(k).cloned().unwrap_or_default(),
        (Value::Object(o), Segment::Index(i)) => o.get(&i.to_string()).cloned().unwrap_or_default(),
        _ => Value::Undefined,
    }
}

fn has_own(obj: &Value, key: &Segment) -> bool {
    match (obj, key) {
        (Value::Object(o), Segment::Key(k)) => o.contains_key(k),
        (Value::Object(o), Segment::Index(i)) => o.contains_key(&i.to_string()),
        (Value::Array(a), Segment::Key(k)) => k == "length" || matches!(k.parse::<usize>(), Ok(i) if i < a.len() && i.to_string() == *k),
        (Value::Array(a), Segment::Index(i)) => *i >= 0 && (*i as usize) < a.len(),
        _ => false,
    }
}

pub fn resolve_operand(op: &Operand, scope: &Scope) -> Value {
    let p = match op {
        Operand::Literal(v) => return v.clone(),
        Operand::Path(p) => p,
    };
    let mut s = Some(scope);
    for _ in 0..p.up {
        s = s.and_then(|x| x.parent);
    }
    let Some(s) = s else { return Value::Undefined };

    if let Some(local) = &p.local {
        let mut cur = Some(s);
        while let Some(c) = cur {
            if let Some(v) = c.local(local) {
                let mut value = v.clone();
                for seg in &p.segments {
                    value = get(&value, seg);
                }
                return value;
            }
            cur = c.parent;
        }
        return Value::Undefined;
    }
    if p.self_ {
        let mut value = s.data.clone();
        for seg in &p.segments {
            value = get(&value, seg);
        }
        return value;
    }
    // Look the first segment up through the scope chain (like Mustache), then walk.
    let Some(first) = p.segments.first() else { return Value::Undefined };
    let mut cur = Some(s);
    while let Some(c) = cur {
        if has_own(&c.data, first) {
            let mut value = get(&c.data, first);
            for seg in &p.segments[1..] {
                value = get(&value, seg);
            }
            return value;
        }
        cur = c.parent;
    }
    Value::Undefined
}

impl Expression {
    pub fn literal_args(&self) -> impl Iterator<Item = &Rc<str>> {
        self.calls.iter().flat_map(|c| c.args.iter()).filter_map(|a| match a {
            Operand::Literal(Value::String(s)) => Some(s),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::parse_data;

    fn eval(src: &str, data: &str) -> Value {
        let e = compile_expression(src).unwrap();
        let scope = Scope::root(parse_data(data, &[]).unwrap());
        resolve_operand(&e.operand, &scope)
    }

    #[test]
    fn paths() {
        assert_eq!(eval("a.b.c", r#"{"a":{"b":{"c":1}}}"#).js_string(), "1");
        assert_eq!(eval("list[1]", r#"{"list":["x","y","z"]}"#).js_string(), "y");
        assert_eq!(eval("list[-1]", r#"{"list":["x","y","z"]}"#).js_string(), "z");
        assert_eq!(eval(r#"obj["key with space"]"#, r#"{"obj":{"key with space":"ok"}}"#).js_string(), "ok");
        assert_eq!(eval("s.length", r#"{"s":"abc"}"#).js_string(), "3");
        assert!(matches!(eval("constructor", "{}"), Value::Undefined));
        let e = compile_expression("x | default: “none” | join: «a», 'b', 2, true").unwrap();
        assert_eq!(e.filters, vec!["default", "join"]);
        assert_eq!(e.path, "x");
        assert!(compile_expression("a b").is_err());
        assert!(compile_expression("").is_err());
        assert!(compile_expression("x | ").is_err());
        assert!(compile_expression("\"open").is_err());
        assert!(compile_expression("a[").is_err());
    }

    #[test]
    fn scopes() {
        let root = Scope::root(parse_data(r#"{"title":"T","currency":"$"}"#, &[]).unwrap());
        let child = root.child(parse_data(r#"{"name":"a"}"#, &[]).unwrap(), vec![("index", Value::num(0.0))]);
        let ev = |src: &str| resolve_operand(&compile_expression(src).unwrap().operand, &child).js_string();
        assert_eq!(ev("name"), "a");
        assert_eq!(ev("currency"), "$");
        assert_eq!(ev("../title"), "T");
        assert_eq!(ev("@index"), "0");
        assert_eq!(ev("this.name"), "a");
        assert_eq!(ev(".."), "[object Object]");
    }
}
