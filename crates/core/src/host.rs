//! Host callbacks (spec §7): user filters, `nullValue: host` and `stringify: host` run on the
//! host side. The ABI layer implements [`Host`] by exchanging frames; native callers may implement
//! it directly.

use crate::value::Value;

/// Information about the tag being rendered.
#[derive(Debug, Clone, Copy)]
pub struct TagInfo<'a> {
    /// Full tag text including delimiters, e.g. `{{client.name}}`.
    pub raw: &'a str,
    /// Expression inside the tag, e.g. `client.name | upper`.
    pub expression: &'a str,
    /// Package part, e.g. `word/document.xml`.
    pub part: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// The host does not support callbacks (the import returned 0).
    Unsupported,
    /// The callback returned `ok: false`.
    Failed(String),
    /// The host returned a malformed answer.
    Invalid(String),
}

pub trait Host {
    fn filter(&mut self, name: &str, value: &Value, args: &[Value], tag: &TagInfo) -> Result<Value, HostError>;
    fn null_value(&mut self, tag: &TagInfo) -> Result<Value, HostError>;
    fn stringify(&mut self, value: &Value, tag: &TagInfo) -> Result<Value, HostError>;
}

/// A host without callbacks.
pub struct NoHost;

impl Host for NoHost {
    fn filter(&mut self, _: &str, _: &Value, _: &[Value], _: &TagInfo) -> Result<Value, HostError> {
        Err(HostError::Unsupported)
    }
    fn null_value(&mut self, _: &TagInfo) -> Result<Value, HostError> {
        Err(HostError::Unsupported)
    }
    fn stringify(&mut self, _: &Value, _: &TagInfo) -> Result<Value, HostError> {
        Err(HostError::Unsupported)
    }
}

/// A host built from closures (handy for native callers and tests).
#[derive(Default)]
pub struct FnHost<'a> {
    #[allow(clippy::type_complexity)]
    pub filters: Vec<(String, Box<dyn FnMut(&Value, &[Value]) -> Result<Value, String> + 'a>)>,
    #[allow(clippy::type_complexity)]
    pub null_value: Option<Box<dyn FnMut(&TagInfo) -> Value + 'a>>,
    #[allow(clippy::type_complexity)]
    pub stringify: Option<Box<dyn FnMut(&Value) -> String + 'a>>,
}

impl<'a> FnHost<'a> {
    pub fn new() -> Self {
        FnHost::default()
    }

    pub fn filter(mut self, name: &str, f: impl FnMut(&Value, &[Value]) -> Result<Value, String> + 'a) -> Self {
        self.filters.push((name.to_string(), Box::new(f)));
        self
    }

    pub fn on_null(mut self, f: impl FnMut(&TagInfo) -> Value + 'a) -> Self {
        self.null_value = Some(Box::new(f));
        self
    }

    pub fn on_stringify(mut self, f: impl FnMut(&Value) -> String + 'a) -> Self {
        self.stringify = Some(Box::new(f));
        self
    }

    pub fn filter_names(&self) -> Vec<String> {
        self.filters.iter().map(|(n, _)| n.clone()).collect()
    }
}

impl Host for FnHost<'_> {
    fn filter(&mut self, name: &str, value: &Value, args: &[Value], _: &TagInfo) -> Result<Value, HostError> {
        match self.filters.iter_mut().find(|(n, _)| n == name) {
            Some((_, f)) => f(value, args).map_err(HostError::Failed),
            None => Err(HostError::Invalid(format!("no host filter \"{name}\""))),
        }
    }
    fn null_value(&mut self, tag: &TagInfo) -> Result<Value, HostError> {
        match &mut self.null_value {
            Some(f) => Ok(f(tag)),
            None => Err(HostError::Unsupported),
        }
    }
    fn stringify(&mut self, value: &Value, _: &TagInfo) -> Result<Value, HostError> {
        match &mut self.stringify {
            Some(f) => Ok(Value::string(f(value))),
            None => Err(HostError::Unsupported),
        }
    }
}
