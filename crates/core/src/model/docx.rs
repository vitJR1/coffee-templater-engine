//! DOCX document model: paragraphs, runs, tables, hyperlinks; everything else kept as raw XML.

use super::props::{json_f64, read_fields, write_props, Field, FieldMap};
use crate::error::{Error, Result};
use crate::formats::docx::main_part;
use crate::num::json_number;
use crate::opc::{rel, Package};
use crate::xml::dom::{parse_fragment, parse_xml, serialize_xml, XmlElement, XmlNode};
use crate::xml::escape::{escape_attr, escape_xml};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as SJson};

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocxDocument {
    pub parts: Vec<DocxPart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    Body,
    Header,
    Footer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocxPart {
    /// Package path, e.g. `word/document.xml`, `word/header1.xml`.
    pub path: String,
    pub kind: PartKind,
    pub blocks: Vec<DocxBlock>,
    /// Final section properties of the body (page size, margins, header references).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_props: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DocxBlock {
    Paragraph(DocxParagraph),
    Table(DocxTable),
    Raw(DocxRaw),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ListRef {
    #[serde(serialize_with = "crate::num::ser_f64")]
    pub id: f64,
    #[serde(default, serialize_with = "crate::num::ser_f64")]
    pub level: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParagraphFields {
    /// Paragraph style id, e.g. `Heading1`, `ListParagraph`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// `left` | `center` | `right` | `justify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// Numbering (bullets / numbered lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<ListRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxParagraph {
    #[serde(flatten)]
    pub fields: ParagraphFields,
    #[serde(default)]
    pub children: Vec<DocxInline>,
    /// Original `<w:pPr>` XML. Fields above override it when changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "DocxUnderline"))]
#[serde(untagged)]
pub enum Underline {
    On(bool),
    Kind(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocxRunStyle {
    /// Character style id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// `true` = single underline, or an underline type (`double`, `wave`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<Underline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strike: Option<bool>,
    /// Hex color without `#`, e.g. `FF0000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Highlight color name, e.g. `yellow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<String>,
    /// Font size in points.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::num::ser_opt_f64")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// `superscript` | `subscript` | `baseline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vert_align: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxRun {
    /// Text; `\n` is a line break and `\t` a tab.
    #[serde(default)]
    pub text: String,
    #[serde(flatten)]
    pub style: DocxRunStyle,
    /// Original `<w:rPr>` XML. Fields override it when changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum BreakKind {
    Page,
    Column,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxBreak {
    pub kind: BreakKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxHyperlink {
    /// External URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Internal bookmark name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(default)]
    pub children: Vec<DocxInline>,
}

/// Anything not modelled (images, fields, bookmarks, content controls...), kept as XML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxRaw {
    pub xml: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DocxInline {
    Run(DocxRun),
    Break(DocxBreak),
    Hyperlink(DocxHyperlink),
    Raw(DocxRaw),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxTable {
    pub rows: Vec<DocxTableRow>,
    /// Original `<w:tblPr>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
    /// Original `<w:tblGrid>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxTableRow {
    pub cells: Vec<DocxTableCell>,
    /// Original `<w:trPr>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CellFields {
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::num::ser_opt_f64")]
    pub col_span: Option<f64>,
    /// `restart` | `continue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v_merge: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocxTableCell {
    #[serde(default)]
    pub blocks: Vec<DocxBlock>,
    #[serde(flatten)]
    pub fields: CellFields,
    /// Original `<w:tcPr>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Schema (element order matters to Word)
// ---------------------------------------------------------------------------------------------

const RPR_ORDER: &[&str] = &[
    "w:rStyle",
    "w:rFonts",
    "w:b",
    "w:bCs",
    "w:i",
    "w:iCs",
    "w:caps",
    "w:smallCaps",
    "w:strike",
    "w:dstrike",
    "w:outline",
    "w:shadow",
    "w:emboss",
    "w:imprint",
    "w:noProof",
    "w:snapToGrid",
    "w:vanish",
    "w:webHidden",
    "w:color",
    "w:spacing",
    "w:w",
    "w:kern",
    "w:position",
    "w:sz",
    "w:szCs",
    "w:highlight",
    "w:u",
    "w:effect",
    "w:bdr",
    "w:shd",
    "w:fitText",
    "w:vertAlign",
    "w:rtl",
    "w:cs",
    "w:em",
    "w:lang",
    "w:eastAsianLayout",
    "w:specVanish",
    "w:oMath",
    "w:rPrChange",
];
const PPR_ORDER: &[&str] = &[
    "w:pStyle",
    "w:keepNext",
    "w:keepLines",
    "w:pageBreakBefore",
    "w:framePr",
    "w:widowControl",
    "w:numPr",
    "w:suppressLineNumbers",
    "w:pBdr",
    "w:shd",
    "w:tabs",
    "w:suppressAutoHyphens",
    "w:kinsoku",
    "w:wordWrap",
    "w:overflowPunct",
    "w:topLinePunct",
    "w:autoSpaceDE",
    "w:autoSpaceDN",
    "w:bidi",
    "w:adjustRightInd",
    "w:snapToGrid",
    "w:spacing",
    "w:ind",
    "w:contextualSpacing",
    "w:mirrorIndents",
    "w:suppressOverlap",
    "w:jc",
    "w:textDirection",
    "w:textAlignment",
    "w:textboxTightWrap",
    "w:outlineLvl",
    "w:divId",
    "w:cnfStyle",
    "w:rPr",
    "w:sectPr",
    "w:pPrChange",
];
const TCPR_ORDER: &[&str] = &[
    "w:cnfStyle",
    "w:tcW",
    "w:gridSpan",
    "w:hMerge",
    "w:vMerge",
    "w:tcBorders",
    "w:shd",
    "w:noWrap",
    "w:tcMar",
    "w:textDirection",
    "w:tcFitText",
    "w:vAlign",
    "w:hideMark",
];

fn el(name: &str, attrs: &[(&str, &str)]) -> XmlElement {
    XmlElement::with_attrs(name, attrs)
}

static RUN_FIELDS: FieldMap = &[
    ("style", Field::Val { name: "w:rStyle" }),
    ("bold", Field::OnOff { name: "w:b" }),
    ("italic", Field::OnOff { name: "w:i" }),
    ("strike", Field::OnOff { name: "w:strike" }),
    (
        "underline",
        Field::Child {
            names: &["w:u"],
            read: |c| {
                let v = c.attr("w:val").unwrap_or("single");
                Some(match v {
                    "none" => SJson::Bool(false),
                    "single" => SJson::Bool(true),
                    other => SJson::String(other.into()),
                })
            },
            write: |v| {
                let val = match v {
                    SJson::Bool(true) => "single".to_string(),
                    SJson::Bool(false) => "none".to_string(),
                    other => super::props::json_text(other),
                };
                vec![el("w:u", &[("w:val", &val)])]
            },
        },
    ),
    ("color", Field::Val { name: "w:color" }),
    ("highlight", Field::Val { name: "w:highlight" }),
    (
        "size",
        Field::Child {
            names: &["w:sz", "w:szCs"],
            read: |c| c.attr("w:val").filter(|v| !v.is_empty()).map(|v| json_number(crate::num::js_string_to_number(v) / 2.0)),
            write: |v| {
                let half = crate::num::js_round(json_f64(v).unwrap_or(f64::NAN) * 2.0);
                let s = crate::num::js_to_string(half);
                vec![el("w:sz", &[("w:val", &s)]), el("w:szCs", &[("w:val", &s)])]
            },
        },
    ),
    (
        "font",
        Field::Child {
            names: &["w:rFonts"],
            read: |c| c.attr("w:ascii").or_else(|| c.attr("w:hAnsi")).map(|s| SJson::String(s.into())),
            write: |v| {
                let f = super::props::json_text(v);
                vec![el("w:rFonts", &[("w:ascii", &f), ("w:hAnsi", &f), ("w:cs", &f), ("w:eastAsia", &f)])]
            },
        },
    ),
    ("vertAlign", Field::Val { name: "w:vertAlign" }),
];

fn align_in(v: &str) -> Option<&'static str> {
    Some(match v {
        "left" | "start" => "left",
        "center" => "center",
        "right" | "end" => "right",
        "both" | "distribute" => "justify",
        _ => return None,
    })
}

static PARA_FIELDS: FieldMap = &[
    ("style", Field::Val { name: "w:pStyle" }),
    (
        "align",
        Field::Child {
            names: &["w:jc"],
            read: |c| c.attr("w:val").and_then(align_in).map(|s| SJson::String(s.into())),
            write: |v| {
                let s = super::props::json_text(v);
                let out = match s.as_str() {
                    "justify" => "both".to_string(),
                    other => other.to_string(),
                };
                vec![el("w:jc", &[("w:val", &out)])]
            },
        },
    ),
    (
        "list",
        Field::Child {
            names: &["w:numPr"],
            read: |c| {
                let id = c.first_child("w:numId")?.attr("w:val")?;
                let level = c.first_child("w:ilvl").and_then(|l| l.attr("w:val")).unwrap_or("0");
                let mut m = Map::new();
                m.insert("id".into(), json_number(crate::num::js_string_to_number(id)));
                m.insert("level".into(), json_number(crate::num::js_string_to_number(level)));
                Some(SJson::Object(m))
            },
            write: |v| {
                let get = |k: &str| v.get(k).and_then(json_f64);
                let level = crate::num::js_to_string(get("level").unwrap_or(0.0));
                let id = v.get("id").map(super::props::json_text).unwrap_or_default();
                vec![el("w:numPr", &[]).child(el("w:ilvl", &[("w:val", &level)])).child(el("w:numId", &[("w:val", &id)]))]
            },
        },
    ),
];

static CELL_FIELDS: FieldMap = &[
    (
        "colSpan",
        Field::Child {
            names: &["w:gridSpan"],
            read: |c| Some(json_number(crate::num::js_string_to_number(c.attr("w:val").unwrap_or("x")))),
            write: |v| match json_f64(v) {
                Some(n) if n > 1.0 => vec![el("w:gridSpan", &[("w:val", &crate::num::js_to_string(n))])],
                _ => Vec::new(),
            },
        },
    ),
    (
        "vMerge",
        Field::Child {
            names: &["w:vMerge"],
            read: |c| Some(SJson::String(if c.attr("w:val") == Some("restart") { "restart".into() } else { "continue".into() })),
            write: |v| {
                if v.as_str() == Some("restart") {
                    vec![el("w:vMerge", &[("w:val", "restart")])]
                } else {
                    vec![el("w:vMerge", &[])]
                }
            },
        },
    ),
];

fn to_map<T: Serialize>(v: &T) -> Map<String, SJson> {
    match serde_json::to_value(v) {
        Ok(SJson::Object(m)) => m,
        _ => Map::new(),
    }
}

fn from_map<T: for<'de> Deserialize<'de> + Default>(m: Map<String, SJson>) -> T {
    serde_json::from_value(SJson::Object(m)).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------------------------

struct ParseCtx {
    rels: Vec<(String, String)>,
}

pub fn parse_docx(pkg: &Package) -> Result<DocxDocument> {
    let main = main_part(pkg);
    let mut parts = Vec::new();
    let mut add = |path: &str, kind: PartKind| -> Result<()> {
        let Some(xml) = pkg.get_text(path) else { return Ok(()) };
        let ctx = ParseCtx {
            rels: pkg.relationships(path).into_iter().filter(|r| r.rel_type == rel::HYPERLINK).map(|r| (r.id, r.target)).collect(),
        };
        let doc = parse_xml(&xml, pkg.max_xml_depth).map_err(|e| e.with_part(path))?;
        let container = if kind == PartKind::Body { doc.root.first_child("w:body") } else { Some(&doc.root) };
        let Some(container) = container else { return Ok(()) };
        let mut part = DocxPart { path: path.to_string(), kind, blocks: Vec::new(), section_props: None };
        for child in container.elements() {
            if kind == PartKind::Body && child.name == "w:sectPr" {
                part.section_props = Some(child.serialize());
            } else {
                part.blocks.push(parse_block(child, &ctx));
            }
        }
        parts.push(part);
        Ok(())
    };
    add(&main, PartKind::Body)?;
    for (_, p) in pkg.related_parts(&main, rel::HEADER) {
        add(&p, PartKind::Header)?;
    }
    for (_, p) in pkg.related_parts(&main, rel::FOOTER) {
        add(&p, PartKind::Footer)?;
    }
    Ok(DocxDocument { parts })
}

fn parse_block(node: &XmlElement, ctx: &ParseCtx) -> DocxBlock {
    match node.name.as_str() {
        "w:p" => DocxBlock::Paragraph(parse_paragraph(node, ctx)),
        "w:tbl" => DocxBlock::Table(parse_table(node, ctx)),
        _ => DocxBlock::Raw(DocxRaw { xml: node.serialize() }),
    }
}

fn parse_paragraph(p: &XmlElement, ctx: &ParseCtx) -> DocxParagraph {
    let ppr = p.first_child("w:pPr");
    DocxParagraph {
        fields: from_map(read_fields(ppr, PARA_FIELDS)),
        children: parse_inlines(&p.children, ctx),
        props: ppr.map(XmlElement::serialize),
    }
}

const SKIP: [&str; 3] = ["w:pPr", "w:proofErr", "w:lastRenderedPageBreak"];

fn parse_inlines(nodes: &[XmlNode], ctx: &ParseCtx) -> Vec<DocxInline> {
    let mut out = Vec::new();
    for child in nodes {
        let XmlNode::Element(child) = child else { continue };
        if SKIP.contains(&child.name.as_str()) {
            continue;
        }
        match child.name.as_str() {
            "w:r" => out.extend(parse_run(child)),
            "w:hyperlink" => {
                let url = child.attr("r:id").and_then(|id| ctx.rels.iter().find(|(r, _)| r == id)).map(|(_, t)| t.clone());
                out.push(DocxInline::Hyperlink(DocxHyperlink {
                    url,
                    anchor: child.attr("w:anchor").filter(|a| !a.is_empty()).map(str::to_string),
                    children: parse_inlines(&child.children, ctx),
                }));
            }
            _ => out.push(DocxInline::Raw(DocxRaw { xml: child.serialize() })),
        }
    }
    merge_runs(out)
}

fn parse_run(r: &XmlElement) -> Vec<DocxInline> {
    let rpr = r.first_child("w:rPr");
    let style: DocxRunStyle = from_map(read_fields(rpr, RUN_FIELDS));
    let props = rpr.map(XmlElement::serialize);
    let mut out = Vec::new();
    let mut text: Option<String> = None;
    let flush = |text: &mut Option<String>, out: &mut Vec<DocxInline>| {
        if let Some(t) = text.take() {
            out.push(DocxInline::Run(DocxRun { text: t, style: style.clone(), props: props.clone() }));
        }
    };
    for c in r.elements() {
        if c.name == "w:rPr" || c.name == "w:lastRenderedPageBreak" {
            continue;
        }
        let append = |text: &mut Option<String>, s: &str| text.get_or_insert_with(String::new).push_str(s);
        match c.name.as_str() {
            "w:t" => {
                let t: String = c.children.iter().map(|n| if let XmlNode::Text(t) = n { t.as_str() } else { "" }).collect();
                append(&mut text, &t);
            }
            "w:tab" => append(&mut text, "\t"),
            "w:cr" => append(&mut text, "\n"),
            "w:noBreakHyphen" => append(&mut text, "\u{2011}"),
            "w:softHyphen" => append(&mut text, "\u{ad}"),
            "w:br" => match c.attr("w:type") {
                Some(k @ ("page" | "column")) => {
                    flush(&mut text, &mut out);
                    out.push(DocxInline::Break(DocxBreak { kind: if k == "page" { BreakKind::Page } else { BreakKind::Column } }));
                }
                _ => append(&mut text, "\n"),
            },
            _ => {
                flush(&mut text, &mut out);
                out.push(DocxInline::Raw(DocxRaw { xml: format!("<w:r>{}{}</w:r>", props.as_deref().unwrap_or(""), c.serialize()) }));
            }
        }
    }
    flush(&mut text, &mut out);
    out
}

/// rsid attributes differ between otherwise identical runs; ignore them when comparing.
fn normalize_run_props(xml: Option<&str>) -> String {
    crate::re!(r#"\sw:rsid\w*="[^"]*""#).replace_all(xml.unwrap_or(""), "").into_owned()
}

/// Merges neighbouring runs with identical formatting (Word splits them for spell-check/rsid noise).
fn merge_runs(list: Vec<DocxInline>) -> Vec<DocxInline> {
    let mut out: Vec<DocxInline> = Vec::new();
    for item in list {
        if let (DocxInline::Run(cur), Some(DocxInline::Run(prev))) = (&item, out.last_mut()) {
            if prev.style == cur.style && normalize_run_props(prev.props.as_deref()) == normalize_run_props(cur.props.as_deref()) {
                prev.text.push_str(&cur.text);
                continue;
            }
        }
        out.push(item);
    }
    out
}

fn parse_table(tbl: &XmlElement, ctx: &ParseCtx) -> DocxTable {
    let mut rows = Vec::new();
    for tr in tbl.children_named("w:tr") {
        let mut cells = Vec::new();
        for tc in tr.children_named("w:tc") {
            let tcpr = tc.first_child("w:tcPr");
            cells.push(DocxTableCell {
                blocks: tc.elements().filter(|b| b.name != "w:tcPr").map(|b| parse_block(b, ctx)).collect(),
                fields: from_map(read_fields(tcpr, CELL_FIELDS)),
                props: tcpr.map(XmlElement::serialize),
            });
        }
        rows.push(DocxTableRow { cells, props: tr.first_child("w:trPr").map(XmlElement::serialize) });
    }
    DocxTable {
        rows,
        props: tbl.first_child("w:tblPr").map(XmlElement::serialize),
        grid: tbl.first_child("w:tblGrid").map(XmlElement::serialize),
    }
}

// ---------------------------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------------------------

struct BuildCtx<'a> {
    pkg: &'a mut Package,
    part: String,
    depth: u64,
}

pub fn build_docx(model: &DocxDocument, pkg: &mut Package) -> Result<()> {
    let depth = pkg.max_xml_depth;
    for part in &model.parts {
        let xml = pkg
            .get_text(&part.path)
            .ok_or_else(|| Error::invalid_model(format!("Part {} does not exist in the base document", part.path)))?;
        let mut doc = parse_xml(&xml, depth).map_err(|e| e.with_part(&part.path))?;
        let mut ctx = BuildCtx { pkg: &mut *pkg, part: part.path.clone(), depth };
        let mut inner = String::new();
        for b in &part.blocks {
            inner.push_str(&build_block(b, &mut ctx)?);
        }
        let container = if part.kind == PartKind::Body { doc.root.first_child_mut("w:body") } else { Some(&mut doc.root) };
        let container = container.ok_or_else(|| Error::invalid_model(format!("Part {} has no body", part.path)))?;
        if part.kind == PartKind::Body {
            let sect = part.section_props.clone().or_else(|| container.first_child("w:sectPr").map(XmlElement::serialize));
            if let Some(s) = sect {
                inner.push_str(&s);
            }
        }
        container.children =
            parse_fragment(&inner, depth).map_err(|e| Error::invalid_model(format!("Invalid XML in {}: {}", part.path, e.message)))?;
        if inner.contains(" r:id=") && doc.root.attr("xmlns:r").is_none() {
            doc.root.set_attr("xmlns:r", "http://schemas.openxmlformats.org/officeDocument/2006/relationships");
        }
        pkg.set_text(&part.path, &serialize_xml(&doc));
    }
    Ok(())
}

fn build_block(b: &DocxBlock, ctx: &mut BuildCtx) -> Result<String> {
    match b {
        DocxBlock::Raw(r) => Ok(r.xml.clone()),
        DocxBlock::Table(t) => build_table(t, ctx),
        DocxBlock::Paragraph(p) => build_paragraph(p, ctx),
    }
}

fn build_paragraph(p: &DocxParagraph, ctx: &mut BuildCtx) -> Result<String> {
    let ppr = write_props("w:pPr", p.props.as_deref(), &to_map(&p.fields), PARA_FIELDS, PPR_ORDER, ctx.depth)?;
    let mut out = format!("<w:p>{ppr}");
    for c in &p.children {
        out.push_str(&build_inline(c, ctx)?);
    }
    out.push_str("</w:p>");
    Ok(out)
}

fn build_inline(i: &DocxInline, ctx: &mut BuildCtx) -> Result<String> {
    Ok(match i {
        DocxInline::Raw(r) => r.xml.clone(),
        DocxInline::Break(b) => format!("<w:r><w:br w:type=\"{}\"/></w:r>", if b.kind == BreakKind::Page { "page" } else { "column" }),
        DocxInline::Hyperlink(h) => {
            let mut attrs = Vec::new();
            if let Some(url) = h.url.as_deref().filter(|u| !u.is_empty()) {
                let existing = ctx.pkg.relationships(&ctx.part).into_iter().find(|r| r.rel_type == rel::HYPERLINK && r.target == url);
                let id = match existing {
                    Some(r) => r.id,
                    None => {
                        let part = ctx.part.clone();
                        ctx.pkg.add_relationship(&part, rel::HYPERLINK, url, true)
                    }
                };
                attrs.push(format!("r:id=\"{id}\""));
            }
            if let Some(a) = h.anchor.as_deref().filter(|a| !a.is_empty()) {
                attrs.push(format!("w:anchor=\"{}\"", escape_attr(a)));
            }
            attrs.push("w:history=\"1\"".into());
            let mut out = format!("<w:hyperlink {}>", attrs.join(" "));
            for c in &h.children {
                out.push_str(&build_inline(c, ctx)?);
            }
            out.push_str("</w:hyperlink>");
            out
        }
        DocxInline::Run(r) => {
            let mut m = to_map(&r.style);
            m.remove("text");
            let rpr = write_props("w:rPr", r.props.as_deref(), &m, RUN_FIELDS, RPR_ORDER, ctx.depth)?;
            let mut content = String::new();
            let mut buf = String::new();
            let flush = |buf: &mut String, content: &mut String| {
                if !buf.is_empty() {
                    content.push_str(&format!("<w:t xml:space=\"preserve\">{}</w:t>", escape_xml(buf)));
                    buf.clear();
                }
            };
            let mut chars = r.text.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '\t' => {
                        flush(&mut buf, &mut content);
                        content.push_str("<w:tab/>");
                    }
                    '\u{2011}' => {
                        flush(&mut buf, &mut content);
                        content.push_str("<w:noBreakHyphen/>");
                    }
                    '\u{ad}' => {
                        flush(&mut buf, &mut content);
                        content.push_str("<w:softHyphen/>");
                    }
                    '\r' | '\n' => {
                        flush(&mut buf, &mut content);
                        if c == '\r' && chars.peek() == Some(&'\n') {
                            chars.next();
                        }
                        content.push_str("<w:br/>");
                    }
                    c => buf.push(c),
                }
            }
            flush(&mut buf, &mut content);
            format!("<w:r>{rpr}{content}</w:r>")
        }
    })
}

fn build_table(t: &DocxTable, ctx: &mut BuildCtx) -> Result<String> {
    let cols = t
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.fields.col_span.unwrap_or(1.0).max(1.0) as usize).sum::<usize>())
        .max()
        .unwrap_or(1)
        .max(1);
    let tbl_pr =
        t.props.clone().unwrap_or_else(|| "<w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>".into());
    let grid = t.grid.clone().unwrap_or_else(|| format!("<w:tblGrid>{}</w:tblGrid>", "<w:gridCol/>".repeat(cols.min(1000))));
    let mut rows = String::new();
    for r in &t.rows {
        let mut cells = String::new();
        for c in &r.cells {
            let tcpr = write_props("w:tcPr", c.props.as_deref(), &to_map(&c.fields), CELL_FIELDS, TCPR_ORDER, ctx.depth)?;
            let mut blocks = String::new();
            for b in &c.blocks {
                blocks.push_str(&build_block(b, ctx)?);
            }
            // A cell must end with a paragraph.
            if !matches!(c.blocks.last(), Some(DocxBlock::Paragraph(_))) {
                blocks.push_str("<w:p/>");
            }
            cells.push_str(&format!("<w:tc>{tcpr}{blocks}</w:tc>"));
        }
        rows.push_str(&format!("<w:tr>{}{cells}</w:tr>", r.props.as_deref().unwrap_or("")));
    }
    Ok(format!("<w:tbl>{tbl_pr}{grid}{rows}</w:tbl>"))
}

// ---------------------------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------------------------

pub fn inline_text(children: &[DocxInline]) -> String {
    children
        .iter()
        .map(|c| match c {
            DocxInline::Run(r) => r.text.clone(),
            DocxInline::Hyperlink(h) => inline_text(&h.children),
            DocxInline::Break(_) => "\n".into(),
            DocxInline::Raw(_) => String::new(),
        })
        .collect()
}

pub fn blocks_text(blocks: &[DocxBlock]) -> String {
    blocks
        .iter()
        .map(|b| match b {
            DocxBlock::Paragraph(p) => inline_text(&p.children),
            DocxBlock::Table(t) => t
                .rows
                .iter()
                .map(|r| r.cells.iter().map(|c| blocks_text(&c.blocks).replace('\n', " ")).collect::<Vec<_>>().join("\t"))
                .collect::<Vec<_>>()
                .join("\n"),
            DocxBlock::Raw(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn docx_text(model: &DocxDocument) -> String {
    model.parts.iter().find(|p| p.kind == PartKind::Body).map(|p| blocks_text(&p.blocks)).unwrap_or_default()
}
