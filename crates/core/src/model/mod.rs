//! Document model (parse → JSON → build), spec §6.7–6.9.

pub mod blank;
pub mod docx;
pub mod pptx;
pub mod props;
pub mod xlsx;

use crate::error::{Error, Result};
use crate::formats::DocumentFormat;
use crate::limits::Limits;
use crate::opc::Package;
use serde::{Deserialize, Serialize};

pub use docx::DocxDocument;
pub use pptx::PptxDocument;
pub use xlsx::XlsxDocument;

/// Version of the JSON model (spec §13). `build` accepts this and the previous version.
pub const MODEL_VERSION: u32 = 1;
const OLDEST_MODEL_VERSION: u32 = 1;

fn model_version() -> u32 {
    MODEL_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum OfficeDocument {
    Docx(Versioned<DocxDocument>),
    Xlsx(Versioned<XlsxDocument>),
    Pptx(Versioned<PptxDocument>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "Versioned{T}"))]
pub struct Versioned<T> {
    #[serde(rename = "modelVersion", default = "model_version")]
    pub model_version: u32,
    #[serde(flatten)]
    pub doc: T,
}

impl<T> Versioned<T> {
    pub fn new(doc: T) -> Self {
        Versioned { model_version: MODEL_VERSION, doc }
    }
}

impl OfficeDocument {
    pub fn format(&self) -> DocumentFormat {
        match self {
            OfficeDocument::Docx(_) => DocumentFormat::Docx,
            OfficeDocument::Xlsx(_) => DocumentFormat::Xlsx,
            OfficeDocument::Pptx(_) => DocumentFormat::Pptx,
        }
    }

    pub fn model_version(&self) -> u32 {
        match self {
            OfficeDocument::Docx(v) => v.model_version,
            OfficeDocument::Xlsx(v) => v.model_version,
            OfficeDocument::Pptx(v) => v.model_version,
        }
    }

    pub fn from_json(v: serde_json::Value) -> Result<OfficeDocument> {
        let doc: OfficeDocument = serde_json::from_value(v).map_err(|e| Error::invalid_model(format!("Invalid model: {e}")))?;
        let ver = doc.model_version();
        if !(OLDEST_MODEL_VERSION..=MODEL_VERSION).contains(&ver) {
            return Err(Error::invalid_model(format!(
                "Unsupported modelVersion {ver} (supported: {OLDEST_MODEL_VERSION}..{MODEL_VERSION})"
            )));
        }
        Ok(doc)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// Parses a .docx/.xlsx/.pptx file into a JSON-serializable model.
pub fn parse(input: &[u8], limits: &Limits) -> Result<OfficeDocument> {
    let pkg = Package::load(input, limits)?;
    Ok(match DocumentFormat::detect(&pkg)? {
        DocumentFormat::Docx => OfficeDocument::Docx(Versioned::new(docx::parse_docx(&pkg)?)),
        DocumentFormat::Xlsx => OfficeDocument::Xlsx(Versioned::new(xlsx::parse_xlsx(&pkg)?)),
        DocumentFormat::Pptx => OfficeDocument::Pptx(Versioned::new(pptx::parse_pptx(&pkg)?)),
    })
}

/// Builds a document from a model. Styles, media and everything else not in the model are taken
/// from `base`. `base` is optional for docx/xlsx (a blank document is used) and required for pptx.
pub fn build(model: &OfficeDocument, base: Option<&[u8]>, limits: &Limits) -> Result<Vec<u8>> {
    let mut pkg = match base {
        Some(b) => {
            let pkg = Package::load(b, limits)?;
            let f = DocumentFormat::detect(&pkg)?;
            if f != model.format() {
                return Err(Error::invalid_model(format!("Base document is {f}, model is {}", model.format())));
            }
            pkg
        }
        None => match model {
            OfficeDocument::Docx(_) => blank::blank_docx(),
            OfficeDocument::Xlsx(_) => blank::blank_xlsx(),
            OfficeDocument::Pptx(_) => return Err(Error::invalid_model("Building a pptx requires `base` (the original presentation)")),
        },
    };
    pkg.max_xml_depth = limits.max_xml_depth;
    match model {
        OfficeDocument::Docx(v) => {
            if base.is_none() {
                // blank document: only the body exists
                let mut doc = v.doc.clone();
                doc.parts.retain(|p| p.kind == docx::PartKind::Body);
                for p in &mut doc.parts {
                    p.path = "word/document.xml".into();
                }
                docx::build_docx(&doc, &mut pkg)?;
            } else {
                docx::build_docx(&v.doc, &mut pkg)?;
            }
        }
        OfficeDocument::Xlsx(v) => {
            if base.is_none() {
                let mut doc = v.doc.clone();
                for s in &mut doc.sheets {
                    s.path = None;
                }
                xlsx::build_xlsx(&doc, &mut pkg)?;
            } else {
                xlsx::build_xlsx(&v.doc, &mut pkg)?;
            }
        }
        OfficeDocument::Pptx(v) => pptx::build_pptx(&v.doc, &mut pkg)?,
    }
    pkg.to_bytes(limits)
}

/// Plain text of a model: paragraphs (docx), tab-separated cells (xlsx), slide texts (pptx).
pub fn model_text(doc: &OfficeDocument) -> String {
    match doc {
        OfficeDocument::Docx(v) => docx::docx_text(&v.doc),
        OfficeDocument::Xlsx(v) => xlsx::xlsx_text(&v.doc),
        OfficeDocument::Pptx(v) => pptx::pptx_text(&v.doc),
    }
}

pub fn extract_text(input: &[u8], limits: &Limits) -> Result<String> {
    Ok(model_text(&parse(input, limits)?))
}

/// Detects the format of a document.
pub fn detect(input: &[u8], limits: &Limits) -> Result<DocumentFormat> {
    DocumentFormat::detect(&Package::load(input, limits)?)
}
