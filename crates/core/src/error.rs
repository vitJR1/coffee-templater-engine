//! Structured errors (spec §8).

use serde::Serialize;
use serde_json::{Map, Value as Json};
use std::fmt;

/// Error codes. Template codes match the TS implementation; the rest are core-level codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // template
    UnclosedTag,
    UnopenedTag,
    UnclosedSection,
    UnmatchedClose,
    InvalidExpression,
    UnknownFilter,
    FilterFailed,
    MissingValue,
    InvalidSection,
    InvalidImage,
    Unsupported,
    // core
    InvalidRequest,
    InvalidOptions,
    InvalidDocument,
    UnsupportedFormat,
    InvalidModel,
    UnknownTemplate,
    LimitExceeded,
    HostError,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnclosedTag => "unclosed_tag",
            ErrorCode::UnopenedTag => "unopened_tag",
            ErrorCode::UnclosedSection => "unclosed_section",
            ErrorCode::UnmatchedClose => "unmatched_close",
            ErrorCode::InvalidExpression => "invalid_expression",
            ErrorCode::UnknownFilter => "unknown_filter",
            ErrorCode::FilterFailed => "filter_failed",
            ErrorCode::MissingValue => "missing_value",
            ErrorCode::InvalidSection => "invalid_section",
            ErrorCode::InvalidImage => "invalid_image",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::InvalidOptions => "invalid_options",
            ErrorCode::InvalidDocument => "invalid_document",
            ErrorCode::UnsupportedFormat => "unsupported_format",
            ErrorCode::InvalidModel => "invalid_model",
            ErrorCode::UnknownTemplate => "unknown_template",
            ErrorCode::LimitExceeded => "limit_exceeded",
            ErrorCode::HostError => "host_error",
            ErrorCode::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an error happened inside a part. Filled when known.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Location {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paragraph: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Box<Location>>,
    #[serde(skip_serializing_if = "no_details")]
    pub details: Box<Map<String, Json>>,
}

fn no_details(d: &Map<String, Json>) -> bool {
    d.is_empty()
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Error { code, message: message.into(), part: None, tag: None, location: None, details: Box::default() }
    }

    pub fn template(code: ErrorCode, message: impl Into<String>, part: &str, tag: Option<&str>) -> Self {
        let mut e = Error::new(code, message);
        if !part.is_empty() {
            e.part = Some(part.to_string());
        }
        e.tag = tag.map(str::to_string);
        e
    }

    pub fn with_part(mut self, part: &str) -> Self {
        if self.part.is_none() && !part.is_empty() {
            self.part = Some(part.to_string());
        }
        self
    }

    pub fn with_tag(mut self, tag: &str) -> Self {
        if self.tag.is_none() {
            self.tag = Some(tag.to_string());
        }
        self
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<Json>) -> Self {
        self.details.insert(key.to_string(), value.into());
        self
    }

    pub fn invalid_document(message: impl Into<String>) -> Self {
        Error::new(ErrorCode::InvalidDocument, message)
    }

    pub fn invalid_model(message: impl Into<String>) -> Self {
        Error::new(ErrorCode::InvalidModel, message)
    }

    pub fn limit(name: &str, limit: u64) -> Self {
        Error::new(ErrorCode::LimitExceeded, format!("Limit {name} exceeded ({limit})"))
            .with_detail("limit", name)
            .with_detail("value", limit)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        let mut where_ = Vec::new();
        if let Some(t) = &self.tag {
            where_.push(format!("tag {t}"));
        }
        if let Some(p) = &self.part {
            where_.push(format!("in {p}"));
        }
        if !where_.is_empty() {
            write!(f, " ({})", where_.join(" "))?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

/// A non-empty list of errors (compile collects all template problems, spec §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Errors(pub Vec<Error>);

impl From<Error> for Errors {
    fn from(e: Error) -> Self {
        Errors(vec![e])
    }
}

impl fmt::Display for Errors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Errors {}

impl Errors {
    pub fn first(&self) -> &Error {
        &self.0[0]
    }
    pub fn codes(&self) -> Vec<ErrorCode> {
        self.0.iter().map(|e| e.code).collect()
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
