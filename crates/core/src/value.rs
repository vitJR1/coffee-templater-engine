//! Template data values (spec §5) and the JavaScript-like semantics the TS engine relies on
//! (truthiness, `String(v)`, `Number(v)`, `===`, `<`).

use crate::date::DateTime;
use crate::error::{Error, ErrorCode, Result};
use crate::json::{self, Json};
use crate::num;
use base64::Engine as _;
use indexmap::IndexMap;
use std::cmp::Ordering;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct Number {
    pub value: f64,
    /// Source text when the number came from JSON input.
    pub lexeme: Option<Rc<str>>,
}

pub type Object = IndexMap<String, Value>;

#[derive(Debug, Clone, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(Number),
    String(Rc<str>),
    Array(Rc<Vec<Value>>),
    Object(Rc<Object>),
    Date(DateTime),
    /// Exact decimal, kept as text (`"1234.50"`).
    Decimal(Rc<str>),
    Bytes(Rc<[u8]>),
}

const TAGS: [&str; 5] = ["$date", "$decimal", "$blob", "$base64", "$literal"];

impl Value {
    pub fn num(v: f64) -> Value {
        Value::Number(Number { value: v, lexeme: None })
    }
    pub fn str(s: &str) -> Value {
        Value::String(Rc::from(s))
    }
    pub fn string(s: String) -> Value {
        Value::String(Rc::from(s))
    }
    pub fn array(v: Vec<Value>) -> Value {
        Value::Array(Rc::new(v))
    }
    pub fn object(o: Object) -> Value {
        Value::Object(Rc::new(o))
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    // -----------------------------------------------------------------------------------------
    // JSON <-> Value
    // -----------------------------------------------------------------------------------------

    /// Converts request JSON into a value, resolving tagged values and blob references.
    pub fn from_json(j: &Json, blobs: &[Rc<[u8]>]) -> Result<Value> {
        Ok(match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(lex) => Value::Number(Number { value: lex.parse().unwrap_or(f64::NAN), lexeme: Some(Rc::from(lex.as_str())) }),
            Json::Str(s) => Value::str(s),
            Json::Arr(a) => Value::array(a.iter().map(|x| Value::from_json(x, blobs)).collect::<Result<_>>()?),
            Json::Obj(items) => {
                if items.len() == 1 && TAGS.contains(&items[0].0.as_str()) {
                    return tagged(&items[0].0, &items[0].1, blobs);
                }
                let mut o = Object::with_capacity(items.len());
                for (k, v) in items {
                    o.insert(k.clone(), Value::from_json(v, blobs)?);
                }
                Value::object(o)
            }
        })
    }

    /// Plain JSON → value, without interpreting tags (`$literal`).
    pub fn from_json_literal(j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(lex) => Value::Number(Number { value: lex.parse().unwrap_or(f64::NAN), lexeme: Some(Rc::from(lex.as_str())) }),
            Json::Str(s) => Value::str(s),
            Json::Arr(a) => Value::array(a.iter().map(Value::from_json_literal).collect()),
            Json::Obj(items) => Value::object(items.iter().map(|(k, v)| (k.clone(), Value::from_json_literal(v))).collect()),
        }
    }

    /// Encodes a value in the tagged notation of spec §5 (for host callbacks). Bytes are appended
    /// to `blobs` and referenced by index.
    pub fn to_tagged_json(&self, blobs: &mut Vec<Rc<[u8]>>) -> Json {
        match self {
            Value::Undefined | Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Number(n) => number_json(n),
            Value::String(s) => Json::Str(s.to_string()),
            Value::Array(a) => Json::Arr(a.iter().map(|x| x.to_tagged_json(blobs)).collect()),
            Value::Object(o) => {
                let items: Vec<(String, Json)> =
                    o.iter().filter(|(_, v)| !matches!(v, Value::Undefined)).map(|(k, v)| (k.clone(), v.to_tagged_json(blobs))).collect();
                if items.len() == 1 && TAGS.contains(&items[0].0.as_str()) {
                    Json::Obj(vec![("$literal".into(), Json::Obj(items))])
                } else {
                    Json::Obj(items)
                }
            }
            Value::Date(d) => Json::Obj(vec![("$date".into(), Json::Str(d.to_iso()))]),
            Value::Decimal(d) => Json::Obj(vec![("$decimal".into(), Json::Str(d.to_string()))]),
            Value::Bytes(b) => {
                blobs.push(b.clone());
                Json::Obj(vec![("$blob".into(), Json::Num((blobs.len() - 1).to_string()))])
            }
        }
    }

    /// `JSON.stringify(value, null, indent)`. Returns `None` for `undefined`.
    pub fn to_json_string(&self, indent: &str) -> Option<String> {
        let mut out = String::new();
        if !write_js_json(&mut out, self, indent, 0) {
            return None;
        }
        Some(out)
    }

    // -----------------------------------------------------------------------------------------
    // JS semantics
    // -----------------------------------------------------------------------------------------

    /// `Boolean(v)`.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.value != 0.0 && !n.value.is_nan(),
            Value::Decimal(d) => num::decimal_plain(d).map(|p| p != "0").unwrap_or(false),
            Value::String(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// Engine truthiness: like `Boolean(v)`, but empty arrays are false.
    pub fn section_truthy(&self) -> bool {
        match self {
            Value::Array(a) => !a.is_empty(),
            v => v.truthy(),
        }
    }

    /// `undefined`, `null`, `''` or `[]`.
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => true,
            Value::String(s) => s.is_empty(),
            Value::Array(a) => a.is_empty(),
            _ => false,
        }
    }

    /// `String(v)`.
    pub fn js_string(&self) -> String {
        match self {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => number_text(n),
            Value::String(s) => s.to_string(),
            Value::Array(a) => a.iter().map(|x| if x.is_nullish() { String::new() } else { x.js_string() }).collect::<Vec<_>>().join(","),
            Value::Object(_) => "[object Object]".into(),
            Value::Date(d) => d.to_iso(),
            Value::Decimal(d) => d.to_string(),
            Value::Bytes(_) => String::new(),
        }
    }

    /// `Number(v)`.
    pub fn js_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::Number(n) => n.value,
            Value::String(s) => num::js_string_to_number(s),
            Value::Date(d) => d.ms as f64,
            Value::Decimal(d) => d.parse().unwrap_or(f64::NAN),
            Value::Array(a) => match a.len() {
                0 => 0.0,
                1 => Value::string(a[0].js_string()).js_number(),
                _ => f64::NAN,
            },
            Value::Object(_) | Value::Bytes(_) => f64::NAN,
        }
    }

    /// The `toNumber` helper of the TS filters: strings accept a decimal comma.
    pub fn to_number(&self) -> f64 {
        match self {
            Value::String(s) => num::js_parse_float(&s.replacen(',', ".", 1)),
            v => v.js_number(),
        }
    }

    /// `===` (objects and arrays compare by identity).
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Number(_) | Value::Decimal(_), Value::Number(_) | Value::Decimal(_)) => self.js_number() == other.js_number(),
            (Value::Date(a), Value::Date(b)) => a.ms == b.ms,
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Bytes(a), Value::Bytes(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// SameValueZero (`Array.prototype.includes`).
    pub fn same_value_zero(&self, other: &Value) -> bool {
        if let (Value::Number(a), Value::Number(b)) = (self, other) {
            if a.value.is_nan() && b.value.is_nan() {
                return true;
            }
        }
        self.strict_eq(other)
    }

    /// Abstract relational comparison (`a < b`, `a > b`). `None` when incomparable (NaN, undefined).
    pub fn js_compare(&self, other: &Value) -> Option<Ordering> {
        let a = self.to_primitive();
        let b = other.to_primitive();
        match (&a, &b) {
            (Prim::Str(x), Prim::Str(y)) => Some(cmp_utf16(x, y)),
            _ => {
                let x = a.to_number();
                let y = b.to_number();
                x.partial_cmp(&y)
            }
        }
    }

    fn to_primitive(&self) -> Prim {
        match self {
            Value::Undefined => Prim::Num(f64::NAN),
            Value::Null => Prim::Num(0.0),
            Value::Bool(b) => Prim::Num(*b as u8 as f64),
            Value::Number(n) => Prim::Num(n.value),
            Value::Decimal(_) | Value::Date(_) => Prim::Num(self.js_number()),
            Value::String(s) => Prim::Str(s.to_string()),
            v => Prim::Str(v.js_string()),
        }
    }
}

enum Prim {
    Num(f64),
    Str(String),
}

impl Prim {
    fn to_number(&self) -> f64 {
        match self {
            Prim::Num(n) => *n,
            Prim::Str(s) => num::js_string_to_number(s),
        }
    }
}

/// JS compares strings by UTF-16 code units.
fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Text of a number: its JSON lexeme when known, `String(n)` otherwise.
pub fn number_text(n: &Number) -> String {
    match &n.lexeme {
        Some(l) => l.to_string(),
        None => num::js_to_string(n.value),
    }
}

fn number_json(n: &Number) -> Json {
    match &n.lexeme {
        Some(l) => Json::Num(l.to_string()),
        None if n.value.is_finite() => Json::Num(num::js_to_string(n.value)),
        None => Json::Null,
    }
}

fn tagged(tag: &str, v: &Json, blobs: &[Rc<[u8]>]) -> Result<Value> {
    let bad = |msg: &str| Error::new(ErrorCode::InvalidRequest, format!("Invalid {tag} value: {msg}"));
    match tag {
        "$literal" => Ok(Value::from_json_literal(v)),
        "$date" => {
            let s = v.as_str().ok_or_else(|| bad("expected an ISO 8601 string"))?;
            DateTime::parse(s).map(Value::Date).ok_or_else(|| bad(&format!("\"{s}\" is not an ISO 8601 date")))
        }
        "$decimal" => {
            let s = match v {
                Json::Str(s) => s.clone(),
                Json::Num(n) => n.clone(),
                _ => return Err(bad("expected a decimal string")),
            };
            if num::decimal_plain(&s).is_none() {
                return Err(bad(&format!("\"{s}\" is not a decimal number")));
            }
            Ok(Value::Decimal(Rc::from(s.trim())))
        }
        "$blob" => {
            let idx = match v {
                Json::Num(n) => n.parse::<usize>().map_err(|_| bad("expected a blob index"))?,
                _ => return Err(bad("expected a blob index")),
            };
            blobs.get(idx).cloned().map(Value::Bytes).ok_or_else(|| bad(&format!("blob {idx} is not in the frame")))
        }
        "$base64" => {
            let s = v.as_str().ok_or_else(|| bad("expected a string"))?;
            decode_base64(s).map(|b| Value::Bytes(Rc::from(b))).ok_or_else(|| bad("not valid base64"))
        }
        _ => unreachable!(),
    }
}

/// Decodes base64 or a `data:<mime>;base64,` URL, ignoring whitespace.
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let s = if s.starts_with("data:") {
        let comma = s.find(',')?;
        if !s[..comma].ends_with(";base64") {
            return None;
        }
        &s[comma + 1..]
    } else {
        s
    };
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(clean.trim_end_matches('=').as_bytes()))
        .ok()
}

/// `JSON.stringify` writer. Returns false when the value is `undefined` (nothing written).
fn write_js_json(out: &mut String, v: &Value, indent: &str, depth: usize) -> bool {
    let nl = |out: &mut String, d: usize| {
        if !indent.is_empty() {
            out.push('\n');
            for _ in 0..d {
                out.push_str(indent);
            }
        }
    };
    match v {
        Value::Undefined => return false,
        Value::Null | Value::Bytes(_) => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match number_json(n) {
            Json::Num(s) => out.push_str(&s),
            _ => out.push_str("null"),
        },
        Value::Decimal(d) => out.push_str(&num::decimal_plain(d).unwrap_or_else(|| "null".into())),
        Value::String(s) => json::write_str(out, s),
        Value::Date(d) => json::write_str(out, &d.to_iso()),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return true;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, depth + 1);
                if !write_js_json(out, x, indent, depth + 1) {
                    out.push_str("null");
                }
            }
            nl(out, depth);
            out.push(']');
        }
        Value::Object(o) => {
            let mut first = true;
            out.push('{');
            for (k, x) in o.iter() {
                if matches!(x, Value::Undefined) {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                nl(out, depth + 1);
                json::write_str(out, k);
                out.push(':');
                if !indent.is_empty() {
                    out.push(' ');
                }
                write_js_json(out, x, indent, depth + 1);
            }
            if !first {
                nl(out, depth);
            }
            out.push('}');
        }
    }
    true
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::str(s)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::num(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

/// Parses data JSON text (convenience for tests and native callers).
pub fn parse_data(text: &str, blobs: &[Rc<[u8]>]) -> Result<Value> {
    Value::from_json(&json::parse(text)?, blobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_values() {
        let blobs: Vec<Rc<[u8]>> = vec![Rc::from(vec![1u8, 2, 3])];
        let v = parse_data(
            r#"{"d":{"$date":"2024-05-17"},"m":{"$decimal":"1234.50"},"b":{"$blob":0},"x":{"$base64":"AQID"},"l":{"$literal":{"$date":"не дата"}},"o":{"$date":"x","y":1}}"#,
            &blobs,
        )
        .unwrap();
        let Value::Object(o) = v else { panic!() };
        assert!(matches!(o["d"], Value::Date(_)));
        assert!(matches!(&o["m"], Value::Decimal(s) if &**s == "1234.50"));
        assert!(matches!(&o["b"], Value::Bytes(b) if b[..] == [1, 2, 3]));
        assert!(matches!(&o["x"], Value::Bytes(b) if b[..] == [1, 2, 3]));
        assert!(matches!(&o["l"], Value::Object(l) if l.contains_key("$date")));
        assert!(matches!(&o["o"], Value::Object(_)));
        assert!(parse_data(r#"{"$date":"не дата"}"#, &[]).is_err());
        assert!(parse_data(r#"{"$blob":3}"#, &[]).is_err());
    }

    #[test]
    fn js_semantics() {
        assert_eq!(parse_data("[1,null,\"a\"]", &[]).unwrap().js_string(), "1,,a");
        assert_eq!(Value::str("1,5").to_number(), 1.5);
        assert!(!Value::array(vec![]).section_truthy());
        assert!(Value::array(vec![]).truthy());
        assert_eq!(Value::str("10").js_compare(&Value::num(9.0)), Some(Ordering::Greater));
        assert_eq!(Value::str("10").js_compare(&Value::str("9")), Some(Ordering::Less));
        let v = parse_data(r#"{"a":[1,{"b":"x"}],"u":null}"#, &[]).unwrap();
        assert_eq!(v.to_json_string("").unwrap(), r#"{"a":[1,{"b":"x"}],"u":null}"#);
        assert_eq!(v.to_json_string("  ").unwrap(), "{\n  \"a\": [\n    1,\n    {\n      \"b\": \"x\"\n    }\n  ],\n  \"u\": null\n}");
    }
}
