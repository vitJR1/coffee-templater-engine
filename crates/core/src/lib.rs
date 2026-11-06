//! # coffee-core
//!
//! Shared core of coffee-templater: a templating engine and a JSON document model for
//! DOCX/XLSX/PPTX. The core is pure: no file system, network, clock or environment access.
//! Language wrappers talk to it through `coffee-abi` (frames over WebAssembly or a C ABI).
//!
//! ```no_run
//! use coffee_core::{Template, Options, NoHost, value::parse_data};
//! let bytes = std::fs::read("invoice.docx").unwrap();
//! let tpl = Template::compile(&bytes, Options::default()).unwrap();
//! let data = parse_data(r#"{"name": "Ann"}"#, &[]).unwrap();
//! let out = tpl.render(&data, &mut NoHost).unwrap();
//! ```

/// Compiles a regex once (`regex-lite`, lazily).
#[macro_export]
#[doc(hidden)]
macro_rules! re {
    ($s:expr) => {{
        static R: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
        R.get_or_init(|| regex_lite::Regex::new($s).expect("valid regex"))
    }};
}

pub mod date;
pub mod error;
pub mod formats;
pub mod host;
pub mod image;
pub mod json;
pub mod limits;
pub mod model;
pub mod num;
pub mod opc;
pub mod template;
pub mod value;
pub mod xml;
pub mod zip;

pub use error::{Error, ErrorCode, Errors, Result};
pub use formats::DocumentFormat;
pub use host::{FnHost, Host, HostError, NoHost, TagInfo};
pub use limits::Limits;
pub use model::{build, detect, extract_text, parse, OfficeDocument, MODEL_VERSION};
pub use template::options::Options;
pub use template::{InspectResult, Template};
pub use value::Value;

/// Semver of the core.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
