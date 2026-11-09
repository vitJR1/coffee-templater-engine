//! Glue between the generic template engine and concrete Office formats.

pub mod docx;
pub mod pptx;
pub mod xlsx;

use crate::error::{Error, ErrorCode, Result};
use crate::opc::{Package, CONTENT_TYPES};
use crate::template::engine::FormatHooks;
use crate::xml::tokenizer::{find_tags, raw_attr};
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

    /// Detects the format from the main document part: its content type (`<Override>` in
    /// `[Content_Types].xml`), then its path. `<Default>` entries are not used: a presentation
    /// with charts declares `Extension="xlsx"` as a spreadsheet, and one with an embedded Word
    /// document declares `docx`.
    pub fn detect(pkg: &Package) -> Result<DocumentFormat> {
        let ct = pkg.get_text(CONTENT_TYPES).unwrap_or_default();
        let overrides: Vec<(String, String)> = find_tags(&ct, "<Override")
            .filter_map(|(s, e)| {
                let tag = &ct[s..e];
                let part = raw_attr(tag, "PartName")?.trim_start_matches('/').to_string();
                Some((part, raw_attr(tag, "ContentType")?.to_string()))
            })
            .collect();
        let by_type = |t: &str| {
            let t = t.split(';').next().unwrap_or(t).trim();
            if t.starts_with("application/vnd.openxmlformats-officedocument.wordprocessingml.") || t.starts_with("application/vnd.ms-word.")
            {
                Some(DocumentFormat::Docx)
            } else if t.starts_with("application/vnd.openxmlformats-officedocument.spreadsheetml.")
                || t.starts_with("application/vnd.ms-excel.")
            {
                Some(DocumentFormat::Xlsx)
            } else if t.starts_with("application/vnd.openxmlformats-officedocument.presentationml.")
                || t.starts_with("application/vnd.ms-powerpoint.")
            {
                Some(DocumentFormat::Pptx)
            } else {
                None
            }
        };
        let main = pkg.main_document_path().unwrap_or_default();
        if !main.is_empty() {
            if let Some(f) = overrides.iter().find(|(p, _)| p.eq_ignore_ascii_case(&main)).and_then(|(_, t)| by_type(t)) {
                return Ok(f);
            }
            for (prefix, f) in [("word/", DocumentFormat::Docx), ("xl/", DocumentFormat::Xlsx), ("ppt/", DocumentFormat::Pptx)] {
                if main.starts_with(prefix) {
                    return Ok(f);
                }
            }
        }
        // no usable main part: the main document content types among the overrides
        for (suffix, f) in [
            ("wordprocessingml.document.main+xml", DocumentFormat::Docx),
            ("wordprocessingml.template.main+xml", DocumentFormat::Docx),
            ("spreadsheetml.sheet.main+xml", DocumentFormat::Xlsx),
            ("spreadsheetml.template.main+xml", DocumentFormat::Xlsx),
            ("presentationml.presentation.main+xml", DocumentFormat::Pptx),
            ("presentationml.template.main+xml", DocumentFormat::Pptx),
        ] {
            if overrides.iter().any(|(_, t)| t.ends_with(suffix)) {
                return Ok(f);
            }
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
