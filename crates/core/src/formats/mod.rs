//! Glue between the generic template engine and concrete Office formats.

pub mod docx;
pub mod pptx;
pub mod xlsx;

use crate::error::{Error, ErrorCode, Result};
use crate::opc::{Package, CONTENT_TYPES};
use crate::template::engine::FormatHooks;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum DocumentFormat {
    Docx,
    Xlsx,
    Pptx,
}

impl DocumentFormat {
    /// Local name of the elements whose `id` must be unique (drawings, shapes).
    pub fn drawing_id_element(self) -> &'static str {
        match self {
            DocumentFormat::Pptx => "cNvPr",
            _ => "docPr",
        }
    }

    pub const ALL: [DocumentFormat; 3] = [DocumentFormat::Docx, DocumentFormat::Xlsx, DocumentFormat::Pptx];

    pub fn as_str(self) -> &'static str {
        match self {
            DocumentFormat::Docx => "docx",
            DocumentFormat::Xlsx => "xlsx",
            DocumentFormat::Pptx => "pptx",
        }
    }

    pub fn detect(pkg: &Package) -> Result<DocumentFormat> {
        let main = pkg.main_document_path().unwrap_or_default();
        let ct = pkg.get_text(CONTENT_TYPES).unwrap_or_default();
        if main.starts_with("word/") || ct.contains("wordprocessingml.document") {
            return Ok(DocumentFormat::Docx);
        }
        if main.starts_with("xl/") || ct.contains("spreadsheetml.sheet") {
            return Ok(DocumentFormat::Xlsx);
        }
        if main.starts_with("ppt/") || ct.contains("presentationml.presentation") {
            return Ok(DocumentFormat::Pptx);
        }
        Err(Error::new(ErrorCode::UnsupportedFormat, "Unsupported document: expected a .docx, .xlsx or .pptx file"))
    }

    pub fn hooks(self) -> &'static dyn FormatHooks {
        match self {
            DocumentFormat::Docx => &docx::Docx,
            DocumentFormat::Xlsx => &xlsx::Xlsx,
            DocumentFormat::Pptx => &pptx::Pptx,
        }
    }

    /// Parts (XML files) that contain templatable text, in document order.
    pub fn template_parts(self, pkg: &Package) -> Vec<String> {
        match self {
            DocumentFormat::Docx => docx::template_parts(pkg),
            DocumentFormat::Xlsx => xlsx::template_parts(pkg),
            DocumentFormat::Pptx => pptx::template_parts(pkg),
        }
    }

    /// Whether image tags are supported.
    pub fn supports_images(self) -> bool {
        !matches!(self, DocumentFormat::Xlsx)
    }
}

impl std::fmt::Display for DocumentFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
