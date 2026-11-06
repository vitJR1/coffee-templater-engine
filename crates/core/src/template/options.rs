//! Compile options (spec §6.2). Defaults match the TS implementation.

use crate::error::{Error, ErrorCode, Result};
use crate::limits::Limits;
use serde_json::Value as SJson;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub struct Syntax {
    pub section: String,
    pub inverted: String,
    pub close: String,
    pub image: String,
    pub else_: String,
    pub if_: String,
    pub unless: String,
    pub each: String,
}

impl Default for Syntax {
    fn default() -> Self {
        Syntax {
            section: "#".into(),
            inverted: "^".into(),
            close: "/".into(),
            image: "%".into(),
            else_: "else".into(),
            if_: "if".into(),
            unless: "unless".into(),
            each: "each".into(),
        }
    }
}

/// What to print for `undefined` / `null`.
#[derive(Debug, Clone, PartialEq)]
pub enum NullValue {
    Empty,
    Text(String),
    /// Keep the original tag text.
    Keep,
    /// Ask the host (`nullValue` callback).
    Host,
    /// Same as `strict: true`.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StringifyMode {
    Default,
    /// Non-string values are formatted by the host (`stringify` callback).
    Host,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub start: String,
    pub end: String,
    pub escape_char: Option<String>,
    pub syntax: Syntax,
    pub strict: bool,
    pub linebreaks: bool,
    /// Filters implemented by the host; they override built-in filters with the same name.
    pub host_filters: BTreeSet<String>,
    pub null_value: NullValue,
    pub stringify: StringifyMode,
    /// `iso` or a `date` filter pattern (`DD.MM.YYYY`) used when printing dates.
    pub date_format: String,
    pub limits: Limits,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            start: "{{".into(),
            end: "}}".into(),
            escape_char: Some("\\".into()),
            syntax: Syntax::default(),
            strict: false,
            linebreaks: true,
            host_filters: BTreeSet::new(),
            null_value: NullValue::Empty,
            stringify: StringifyMode::Default,
            date_format: "iso".into(),
            limits: Limits::default(),
        }
    }
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidOptions, msg)
}

fn string_field(v: &SJson, what: &str) -> Result<String> {
    v.as_str().map(str::to_string).ok_or_else(|| invalid(format!("{what} must be a string")))
}

fn bool_field(v: &SJson, what: &str) -> Result<bool> {
    v.as_bool().ok_or_else(|| invalid(format!("{what} must be a boolean")))
}

impl Options {
    /// Parses the `options` object of a request. Unknown fields are ignored.
    pub fn from_json(v: Option<&SJson>) -> Result<Options> {
        let mut o = Options::default();
        let Some(v) = v else { return Ok(o) };
        if v.is_null() {
            return Ok(o);
        }
        let map = v.as_object().ok_or_else(|| invalid("options must be an object"))?;

        if let Some(d) = map.get("delimiters") {
            match d {
                SJson::Object(m) => {
                    o.start = string_field(m.get("start").unwrap_or(&SJson::Null), "delimiters.start")?;
                    o.end = string_field(m.get("end").unwrap_or(&SJson::Null), "delimiters.end")?;
                }
                SJson::Array(a) if a.len() == 2 => {
                    o.start = string_field(&a[0], "delimiters[0]")?;
                    o.end = string_field(&a[1], "delimiters[1]")?;
                }
                _ => return Err(invalid("delimiters must be {start, end} or [start, end]")),
            }
        }
        if let Some(e) = map.get("escapeChar") {
            o.escape_char = match e {
                SJson::Null | SJson::Bool(false) => None,
                SJson::String(s) => Some(s.clone()),
                _ => return Err(invalid("escapeChar must be a string or null")),
            };
        }
        if let Some(s) = map.get("syntax") {
            let m = s.as_object().ok_or_else(|| invalid("syntax must be an object"))?;
            for (k, val) in m {
                let val = string_field(val, &format!("syntax.{k}"))?;
                let slot = match k.as_str() {
                    "section" => &mut o.syntax.section,
                    "inverted" => &mut o.syntax.inverted,
                    "close" => &mut o.syntax.close,
                    "image" => &mut o.syntax.image,
                    "else" => &mut o.syntax.else_,
                    "if" => &mut o.syntax.if_,
                    "unless" => &mut o.syntax.unless,
                    "each" => &mut o.syntax.each,
                    _ => continue,
                };
                *slot = val;
            }
        }
        if let Some(s) = map.get("strict") {
            o.strict = bool_field(s, "strict")?;
        }
        if let Some(s) = map.get("linebreaks") {
            o.linebreaks = bool_field(s, "linebreaks")?;
        }
        if let Some(h) = map.get("hostFilters") {
            let a = h.as_array().ok_or_else(|| invalid("hostFilters must be an array of names"))?;
            for n in a {
                let n = string_field(n, "hostFilters[]")?;
                if n.is_empty() {
                    return Err(invalid("hostFilters names must be non-empty"));
                }
                o.host_filters.insert(n);
            }
        }
        if let Some(n) = map.get("nullValue") {
            let m = n.as_object().ok_or_else(|| invalid("nullValue must be an object"))?;
            let mode = m.get("mode").and_then(SJson::as_str).unwrap_or("empty");
            o.null_value = match mode {
                "empty" => NullValue::Empty,
                "text" => NullValue::Text(m.get("text").and_then(SJson::as_str).unwrap_or("").to_string()),
                "keep" => NullValue::Keep,
                "host" => NullValue::Host,
                "error" => NullValue::Error,
                other => return Err(invalid(format!("Unknown nullValue.mode \"{other}\""))),
            };
        }
        if let Some(s) = map.get("stringify") {
            let m = s.as_object().ok_or_else(|| invalid("stringify must be an object"))?;
            o.stringify = match m.get("mode").and_then(SJson::as_str).unwrap_or("default") {
                "default" => StringifyMode::Default,
                "host" => StringifyMode::Host,
                other => return Err(invalid(format!("Unknown stringify.mode \"{other}\""))),
            };
            if let Some(f) = m.get("dateFormat") {
                o.date_format = string_field(f, "stringify.dateFormat")?;
            }
        }
        if let Some(l) = map.get("limits") {
            o.limits = parse_limits(l)?;
        }
        o.validate()?;
        Ok(o)
    }

    pub fn validate(&self) -> Result<()> {
        if self.start.is_empty() || self.end.is_empty() {
            return Err(invalid("Delimiters must be non-empty strings"));
        }
        if self.start == self.end {
            return Err(invalid("Start and end delimiters must differ"));
        }
        if matches!(&self.escape_char, Some(e) if e.is_empty()) {
            return Err(invalid("escapeChar must be a non-empty string or null"));
        }
        let s = &self.syntax;
        for (k, v) in [
            ("section", &s.section),
            ("inverted", &s.inverted),
            ("close", &s.close),
            ("image", &s.image),
            ("else", &s.else_),
            ("if", &s.if_),
            ("unless", &s.unless),
            ("each", &s.each),
        ] {
            if v.is_empty() {
                return Err(invalid(format!("syntax.{k} must be non-empty")));
            }
        }
        Ok(())
    }

    pub fn is_host_filter(&self, name: &str) -> bool {
        self.host_filters.contains(name)
    }
}

pub fn parse_limits(v: &SJson) -> Result<Limits> {
    if v.is_null() {
        return Ok(Limits::default());
    }
    let m = v.as_object().ok_or_else(|| invalid("limits must be an object"))?;
    for (k, val) in m {
        if !val.is_u64() {
            return Err(invalid(format!("limits.{k} must be a non-negative integer")));
        }
    }
    serde_json::from_value(v.clone()).map_err(|e| invalid(format!("Invalid limits: {e}")))
}
