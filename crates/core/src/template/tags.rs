//! Tag classification: variable, image, section open/close, else.

use super::expression::{compile_expression, Expression};
use super::filters::is_builtin;
use super::options::Options;
use crate::error::{Error, ErrorCode};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionMode {
    Section,
    Inverted,
    If,
    Unless,
    Each,
}

#[derive(Debug, Clone)]
pub struct ExprTag {
    pub raw: String,
    pub expr: Expression,
}

#[derive(Debug, Clone)]
pub struct OpenTag {
    pub raw: String,
    pub mode: SectionMode,
    pub expr: Expression,
    pub close_name: String,
}

#[derive(Debug, Clone)]
pub enum ParsedTag {
    Var(ExprTag),
    Image(ExprTag),
    Open(OpenTag),
    Close { raw: String, name: String },
    Else { raw: String },
}

impl ParsedTag {
    pub fn raw(&self) -> &str {
        match self {
            ParsedTag::Var(t) | ParsedTag::Image(t) => &t.raw,
            ParsedTag::Open(t) => &t.raw,
            ParsedTag::Close { raw, .. } | ParsedTag::Else { raw } => raw,
        }
    }
}

fn trim(s: &str) -> &str {
    s.trim_matches(super::expression::is_js_space)
}

pub fn parse_tag(inner: &str, raw: &str, opts: &Options, part: &str) -> Result<ParsedTag, Error> {
    let s = &opts.syntax;
    let body = trim(inner);
    let compile = |src: &str| -> Result<Expression, Error> {
        let expr = compile_expression(src)
            .map_err(|e| Error::template(ErrorCode::InvalidExpression, format!("Invalid expression: {}", e.0), part, Some(raw)))?;
        for f in &expr.filters {
            if !is_builtin(f) && !opts.is_host_filter(f) {
                return Err(Error::template(ErrorCode::UnknownFilter, format!("Unknown filter \"{f}\""), part, Some(raw))
                    .with_detail("filter", f.as_str()));
            }
        }
        Ok(expr)
    };

    if body == s.else_ {
        return Ok(ParsedTag::Else { raw: raw.to_string() });
    }
    if let Some(rest) = body.strip_prefix(s.close.as_str()) {
        return Ok(ParsedTag::Close { raw: raw.to_string(), name: trim(rest).to_string() });
    }
    if let Some(rest) = body.strip_prefix(s.image.as_str()) {
        return Ok(ParsedTag::Image(ExprTag { raw: raw.to_string(), expr: compile(rest)? }));
    }
    if let Some(rest) = body.strip_prefix(s.inverted.as_str()) {
        let src = trim(rest);
        return Ok(ParsedTag::Open(OpenTag {
            raw: raw.to_string(),
            mode: SectionMode::Inverted,
            expr: compile(src)?,
            close_name: close_name_of(src),
        }));
    }
    if let Some(rest) = body.strip_prefix(s.section.as_str()) {
        let src = trim(rest);
        for (mode, kw) in [(SectionMode::If, &s.if_), (SectionMode::Unless, &s.unless), (SectionMode::Each, &s.each)] {
            if src == kw.as_str() || src.starts_with(&format!("{kw} ")) {
                let rest = trim(&src[kw.len()..]);
                if rest.is_empty() {
                    return Err(Error::template(ErrorCode::InvalidExpression, format!("\"{kw}\" needs an expression"), part, Some(raw)));
                }
                return Ok(ParsedTag::Open(OpenTag { raw: raw.to_string(), mode, expr: compile(rest)?, close_name: kw.clone() }));
            }
        }
        return Ok(ParsedTag::Open(OpenTag {
            raw: raw.to_string(),
            mode: SectionMode::Section,
            expr: compile(src)?,
            close_name: close_name_of(src),
        }));
    }
    if body.is_empty() {
        return Err(Error::template(ErrorCode::InvalidExpression, "Empty tag", part, Some(raw)));
    }
    Ok(ParsedTag::Var(ExprTag { raw: raw.to_string(), expr: compile(body)? }))
}

/// `items | sort: "x"` closes with `{{/items}}`.
fn close_name_of(src: &str) -> String {
    trim(src.split('|').next().unwrap_or("")).to_string()
}

pub fn closes(open: &OpenTag, close_name: &str) -> bool {
    close_name.is_empty() || close_name == open.close_name
}
