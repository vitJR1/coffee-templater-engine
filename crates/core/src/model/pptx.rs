//! PPTX document model: slides, text shapes, tables, speaker notes.

use super::props::{json_f64, json_text, read_fields, write_props, Field, FieldMap};
use crate::error::{Error, Result};
use crate::formats::pptx::{presentation_path, slide_parts};
use crate::num::json_number;
use crate::opc::{rel, Package};
use crate::xml::dom::{parse_fragment, parse_xml, serialize_xml, XmlElement, XmlNode};
use crate::xml::escape::escape_xml;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as SJson};
use std::collections::HashSet;

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxDocument {
    /// Slides in presentation order. Reorder or remove items to reorder/remove slides.
    pub slides: Vec<PptxSlide>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxSlide {
    /// Package path of the slide, e.g. `ppt/slides/slide1.xml`.
    pub path: String,
    /// Shapes in z-order. Shapes missing from the list are removed on build.
    #[serde(default)]
    pub shapes: Vec<PptxShape>,
    /// Speaker notes as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxTextShape {
    /// Shape id (`p:cNvPr/@id`), used to match shapes on build.
    pub id: i64,
    #[serde(default)]
    pub name: String,
    /// Placeholder type when the shape is a layout placeholder: `title`, `body`, `subTitle`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub paragraphs: Vec<PptxParagraph>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxTableCell {
    #[serde(default)]
    pub paragraphs: Vec<PptxParagraph>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxTableRow {
    #[serde(default)]
    pub cells: Vec<PptxTableCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxTableShape {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rows: Vec<PptxTableRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxOtherShape {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    /// Raw XML. Only needed to insert a new shape; existing shapes are taken from the base file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xml: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PptxShape {
    Text(PptxTextShape),
    Table(PptxTableShape),
    Picture(PptxOtherShape),
    Group(PptxOtherShape),
    Connector(PptxOtherShape),
    Graphic(PptxOtherShape),
    Other(PptxOtherShape),
}

impl PptxShape {
    fn id(&self) -> i64 {
        match self {
            PptxShape::Text(s) => s.id,
            PptxShape::Table(s) => s.id,
            PptxShape::Picture(s) | PptxShape::Group(s) | PptxShape::Connector(s) | PptxShape::Graphic(s) | PptxShape::Other(s) => s.id,
        }
    }
    fn new_xml(&self) -> Option<&str> {
        match self {
            PptxShape::Picture(s) | PptxShape::Group(s) | PptxShape::Connector(s) | PptxShape::Graphic(s) | PptxShape::Other(s) => {
                s.xml.as_deref().filter(|x| !x.is_empty())
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParagraphFields {
    /// `left` | `center` | `right` | `justify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// Outline (bullet) level, 0-based.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::num::ser_opt_f64")]
    pub level: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PptxParagraph {
    #[serde(flatten)]
    pub fields: ParagraphFields,
    #[serde(default)]
    pub children: Vec<PptxInline>,
    /// Original `<a:pPr>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
    /// Original `<a:endParaRPr>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_props: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(rename = "PptxUnderline"))]
#[serde(untagged)]
pub enum Underline {
    On(bool),
    Kind(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<Underline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strike: Option<bool>,
    /// Font size in points.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::num::ser_opt_f64")]
    pub size: Option<f64>,
    /// Hex RGB color without `#`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxRun {
    /// Text; `\n` is a line break.
    #[serde(default)]
    pub text: String,
    #[serde(flatten)]
    pub style: RunStyle,
    /// Original `<a:rPr>` XML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<String>,
}

/// Fields (slide number, date) and other inline content, kept as XML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PptxRaw {
    pub xml: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PptxInline {
    Run(PptxRun),
    Raw(PptxRaw),
}

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

const RPR_ORDER: &[&str] = &[
    "a:ln",
    "a:noFill",
    "a:solidFill",
    "a:gradFill",
    "a:blipFill",
    "a:pattFill",
    "a:grpFill",
    "a:effectLst",
    "a:effectDag",
    "a:highlight",
    "a:uLnTx",
    "a:uLn",
    "a:uFillTx",
    "a:uFill",
    "a:latin",
    "a:ea",
    "a:cs",
    "a:sym",
    "a:hlinkClick",
    "a:hlinkMouseOver",
    "a:rtl",
    "a:extLst",
];

fn bool_read(v: &str) -> Option<SJson> {
    Some(SJson::Bool(v == "1" || v == "true"))
}
fn bool_write(v: &SJson) -> Option<String> {
    Some(if v.as_bool().unwrap_or(false) { "1".into() } else { "0".into() })
}

static RUN_FIELDS: FieldMap = &[
    ("bold", Field::Attr { attr: "b", read: bool_read, write: bool_write }),
    ("italic", Field::Attr { attr: "i", read: bool_read, write: bool_write }),
    (
        "underline",
        Field::Attr {
            attr: "u",
            read: |v| {
                Some(match v {
                    "none" => SJson::Bool(false),
                    "sng" => SJson::Bool(true),
                    o => SJson::String(o.into()),
                })
            },
            write: |v| {
                Some(match v {
                    SJson::Bool(true) => "sng".into(),
                    SJson::Bool(false) => "none".into(),
                    o => json_text(o),
                })
            },
        },
    ),
    (
        "strike",
        Field::Attr {
            attr: "strike",
            read: |v| Some(SJson::Bool(v != "noStrike")),
            write: |v| Some(if v.as_bool().unwrap_or(false) { "sngStrike".into() } else { "noStrike".into() }),
        },
    ),
    (
        "size",
        Field::Attr {
            attr: "sz",
            read: |v| Some(json_number(crate::num::js_string_to_number(v) / 100.0)),
            write: |v| Some(crate::num::js_to_string(crate::num::js_round(json_f64(v).unwrap_or(f64::NAN) * 100.0))),
        },
    ),
    (
        "color",
        Field::Child {
            names: &["a:solidFill", "a:noFill", "a:gradFill", "a:blipFill", "a:pattFill", "a:grpFill"],
            read: |c| c.first_child("a:srgbClr").and_then(|x| x.attr("val")).map(|v| SJson::String(v.into())),
            write: |v| {
                let hex = json_text(v).trim_start_matches('#').to_uppercase();
                vec![XmlElement::new("a:solidFill").child(XmlElement::with_attrs("a:srgbClr", &[("val", &hex)]))]
            },
        },
    ),
    (
        "font",
        Field::Child {
            names: &["a:latin"],
            read: |c| c.attr("typeface").map(|v| SJson::String(v.into())),
            write: |v| vec![XmlElement::with_attrs("a:latin", &[("typeface", &json_text(v))])],
        },
    ),
];

static PARA_FIELDS: FieldMap = &[
    (
        "align",
        Field::Attr {
            attr: "algn",
            read: |v| {
                let a = match v {
                    "l" => "left",
                    "ctr" => "center",
                    "r" => "right",
                    "just" | "dist" => "justify",
                    _ => return None,
                };
                Some(SJson::String(a.into()))
            },
            write: |v| {
                let s = json_text(v);
                Some(match s.as_str() {
                    "left" => "l".into(),
                    "center" => "ctr".into(),
                    "right" => "r".into(),
                    "justify" => "just".into(),
                    _ => s,
                })
            },
        },
    ),
    ("level", Field::Attr { attr: "lvl", read: |v| Some(json_number(crate::num::js_string_to_number(v))), write: |v| Some(json_text(v)) }),
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

const SHAPE_TAGS: [&str; 7] = ["p:sp", "p:pic", "p:graphicFrame", "p:grpSp", "p:cxnSp", "mc:AlternateContent", "p:contentPart"];

fn shape_children(tree: &XmlElement) -> impl Iterator<Item = &XmlElement> {
    tree.elements().filter(|c| SHAPE_TAGS.contains(&c.name.as_str()))
}

fn shape_id(node: &XmlElement) -> (i64, String) {
    let nv = node.find_descendant("p:cNvPr");
    let id = nv.and_then(|n| n.attr("id")).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    (id, nv.and_then(|n| n.attr("name")).unwrap_or("").to_string())
}

pub fn parse_pptx(pkg: &Package) -> Result<PptxDocument> {
    let mut slides = Vec::new();
    for s in slide_parts(pkg) {
        let xml = pkg.get_text(&s.path).unwrap_or_default();
        let root = parse_xml(&xml, pkg.max_xml_depth).map_err(|e| e.with_part(&s.path))?.root;
        let shapes = root.find_descendant("p:spTree").map(|t| shape_children(t).map(parse_shape).collect()).unwrap_or_default();
        slides.push(PptxSlide { notes: notes_text(pkg, &s.path), path: s.path, shapes });
    }
    Ok(PptxDocument { slides })
}

fn parse_shape(node: &XmlElement) -> PptxShape {
    let (id, name) = shape_id(node);
    let other = |id, name| PptxOtherShape { id, name, xml: None };
    match node.name.as_str() {
        "p:sp" => match node.first_child("p:txBody") {
            Some(body) => PptxShape::Text(PptxTextShape {
                id,
                name,
                placeholder: node.find_descendant("p:ph").map(|ph| ph.attr("type").unwrap_or("body").to_string()),
                paragraphs: body.children_named("a:p").map(parse_paragraph).collect(),
            }),
            None => PptxShape::Other(other(id, name)),
        },
        "p:graphicFrame" => match node.find_descendant("a:tbl") {
            Some(tbl) => PptxShape::Table(PptxTableShape {
                id,
                name,
                rows: tbl
                    .children_named("a:tr")
                    .map(|tr| PptxTableRow {
                        cells: tr
                            .children_named("a:tc")
                            .map(|tc| PptxTableCell {
                                paragraphs: tc
                                    .first_child("a:txBody")
                                    .map(|b| b.children_named("a:p").map(parse_paragraph).collect())
                                    .unwrap_or_default(),
                            })
                            .collect(),
                    })
                    .collect(),
            }),
            None => PptxShape::Graphic(other(id, name)),
        },
        "p:pic" => PptxShape::Picture(other(id, name)),
        "p:grpSp" => PptxShape::Group(other(id, name)),
        "p:cxnSp" => PptxShape::Connector(other(id, name)),
        _ => PptxShape::Other(other(id, name)),
    }
}

fn run_from(rpr: Option<&XmlElement>, text: String) -> PptxRun {
    PptxRun { text, style: from_map(read_fields(rpr, RUN_FIELDS)), props: rpr.map(XmlElement::serialize) }
}

fn parse_paragraph(p: &XmlElement) -> PptxParagraph {
    let ppr = p.first_child("a:pPr");
    let mut children: Vec<PptxInline> = Vec::new();
    for c in p.elements() {
        match c.name.as_str() {
            "a:r" => {
                let text = c
                    .first_child("a:t")
                    .map(|t| t.children.iter().map(|n| if let XmlNode::Text(s) = n { s.as_str() } else { "" }).collect())
                    .unwrap_or_default();
                children.push(PptxInline::Run(run_from(c.first_child("a:rPr"), text)));
            }
            "a:br" => match children.last_mut() {
                Some(PptxInline::Run(prev)) => prev.text.push('\n'),
                _ => children.push(PptxInline::Run(run_from(c.first_child("a:rPr"), "\n".into()))),
            },
            "a:pPr" | "a:endParaRPr" => {}
            _ => children.push(PptxInline::Raw(PptxRaw { xml: c.serialize() })),
        }
    }
    // merge runs with identical formatting
    let mut merged: Vec<PptxInline> = Vec::new();
    for item in children {
        if let (PptxInline::Run(cur), Some(PptxInline::Run(prev))) = (&item, merged.last_mut()) {
            if prev.style == cur.style && prev.props == cur.props {
                prev.text.push_str(&cur.text);
                continue;
            }
        }
        merged.push(item);
    }
    PptxParagraph {
        fields: from_map(read_fields(ppr, PARA_FIELDS)),
        children: merged,
        props: ppr.map(XmlElement::serialize),
        end_props: p.first_child("a:endParaRPr").map(XmlElement::serialize),
    }
}

fn notes_part(pkg: &Package, slide: &str) -> Option<String> {
    pkg.related_parts(slide, rel::NOTES_SLIDE).into_iter().next().map(|(_, p)| p).filter(|p| pkg.has(p))
}

fn notes_body_path(root: &XmlElement) -> Option<Vec<usize>> {
    // path of child indices to the body placeholder's txBody
    fn walk(el: &XmlElement, path: &mut Vec<usize>) -> bool {
        for (i, c) in el.children.iter().enumerate() {
            let XmlNode::Element(e) = c else { continue };
            path.push(i);
            let body_ph = e.name == "p:sp" && e.find_descendant("p:ph").is_some_and(|ph| ph.attr("type") == Some("body"));
            if let Some(j) = e.children.iter().position(|n| body_ph && matches!(n, XmlNode::Element(x) if x.name == "p:txBody")) {
                path.push(j);
                return true;
            }
            if walk(e, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    walk(root, &mut path).then_some(path)
}

fn element_at<'a>(root: &'a XmlElement, path: &[usize]) -> Option<&'a XmlElement> {
    let mut cur = root;
    for &i in path {
        match cur.children.get(i) {
            Some(XmlNode::Element(e)) => cur = e,
            _ => return None,
        }
    }
    Some(cur)
}

fn element_at_mut<'a>(root: &'a mut XmlElement, path: &[usize]) -> Option<&'a mut XmlElement> {
    let mut cur = root;
    for &i in path {
        match cur.children.get_mut(i) {
            Some(XmlNode::Element(e)) => cur = e,
            _ => return None,
        }
    }
    Some(cur)
}

fn notes_text(pkg: &Package, slide: &str) -> Option<String> {
    let path = notes_part(pkg, slide)?;
    let root = parse_xml(&pkg.get_text(&path)?, pkg.max_xml_depth).ok()?.root;
    let body = element_at(&root, &notes_body_path(&root)?)?;
    Some(body.children_named("a:p").map(|p| paragraph_text(&parse_paragraph(p))).collect::<Vec<_>>().join("\n"))
}

pub fn paragraph_text(p: &PptxParagraph) -> String {
    p.children.iter().map(|c| if let PptxInline::Run(r) = c { r.text.as_str() } else { "" }).collect()
}

// ---------------------------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------------------------

fn rels_of(path: &str) -> String {
    Package::rels_path_for(path)
}

pub fn build_pptx(model: &PptxDocument, pkg: &mut Package) -> Result<()> {
    let depth = pkg.max_xml_depth;
    let pres = presentation_path(pkg);
    let existing = slide_parts(pkg);
    let mut seen = HashSet::new();
    for s in &model.slides {
        if !existing.iter().any(|e| e.path == s.path) {
            return Err(Error::invalid_model(format!("Slide {} does not exist in the base presentation", s.path)));
        }
        if !seen.insert(s.path.clone()) {
            return Err(Error::invalid_model(format!("Slide {} is listed twice; duplicating slides is not supported", s.path)));
        }
    }

    // slide order / removal
    let mut pres_doc = parse_xml(&pkg.get_text(&pres).unwrap_or_default(), depth)?;
    if let Some(list) = pres_doc.root.find_descendant_mut("p:sldIdLst") {
        let old: Vec<XmlElement> = list.children_named("p:sldId").cloned().collect();
        list.children = model
            .slides
            .iter()
            .filter_map(|s| {
                let rid = &existing.iter().find(|e| e.path == s.path)?.r_id;
                old.iter().find(|o| o.attr("r:id") == Some(rid.as_str())).cloned().map(XmlNode::Element)
            })
            .collect();
        pkg.set_text(&pres, &serialize_xml(&pres_doc));
    }
    for e in &existing {
        if seen.contains(&e.path) {
            continue;
        }
        if let Some(notes) = notes_part(pkg, &e.path) {
            pkg.remove(&notes);
            pkg.remove(&rels_of(&notes));
            pkg.remove_override(&notes);
        }
        let rid = e.r_id.clone();
        pkg.remove_relationships(&pres, |r| r.id == rid);
        pkg.remove(&e.path);
        pkg.remove(&rels_of(&e.path));
        pkg.remove_override(&e.path);
    }

    for slide in &model.slides {
        let mut doc = parse_xml(&pkg.get_text(&slide.path).unwrap_or_default(), depth).map_err(|e| e.with_part(&slide.path))?;
        if let Some(tree) = doc.root.find_descendant_mut("p:spTree") {
            let by_id: Vec<(i64, XmlElement)> = shape_children(tree).map(|n| (shape_id(n).0, n.clone())).collect();
            let head: Vec<XmlNode> =
                tree.elements().filter(|c| c.name == "p:nvGrpSpPr" || c.name == "p:grpSpPr").cloned().map(XmlNode::Element).collect();
            let tail: Vec<XmlNode> = tree.elements().filter(|c| c.name == "p:extLst").cloned().map(XmlNode::Element).collect();
            let mut shapes: Vec<XmlNode> = Vec::new();
            for shape in &slide.shapes {
                if let Some(xml) = shape.new_xml() {
                    shapes.extend(
                        parse_fragment(xml, depth)
                            .map_err(|e| Error::invalid_model(format!("Invalid shape XML on {}: {}", slide.path, e.message)))?,
                    );
                    continue;
                }
                let mut node = by_id.iter().find(|(id, _)| *id == shape.id()).map(|(_, n)| n.clone()).ok_or_else(|| {
                    Error::invalid_model(format!("Shape #{} not found on {}; pass `xml` to insert a new shape", shape.id(), slide.path))
                })?;
                match shape {
                    PptxShape::Text(t) => write_text_shape(&mut node, t, depth)?,
                    PptxShape::Table(t) => write_table(&mut node, t, depth)?,
                    _ => {}
                }
                shapes.push(XmlNode::Element(node));
            }
            tree.children = head.into_iter().chain(shapes).chain(tail).collect();
        }
        pkg.set_text(&slide.path, &serialize_xml(&doc));

        if let Some(notes) = &slide.notes {
            if let Some(path) = notes_part(pkg, &slide.path) {
                if notes_text(pkg, &slide.path).as_deref() != Some(notes.as_str()) {
                    let mut ndoc = parse_xml(&pkg.get_text(&path).unwrap_or_default(), depth)?;
                    if let Some(bp) = notes_body_path(&ndoc.root) {
                        if let Some(body) = element_at_mut(&mut ndoc.root, &bp) {
                            let mut children: Vec<XmlNode> =
                                body.elements().filter(|c| c.name != "a:p").cloned().map(XmlNode::Element).collect();
                            for line in notes.split('\n') {
                                let mut p = XmlElement::new("a:p");
                                if !line.is_empty() {
                                    p = p.child(XmlElement::new("a:r").child(XmlElement::new("a:t").text_child(line)));
                                }
                                children.push(XmlNode::Element(p));
                            }
                            body.children = children;
                            pkg.set_text(&path, &serialize_xml(&ndoc));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn write_text_shape(node: &mut XmlElement, shape: &PptxTextShape, depth: u64) -> Result<()> {
    if node.first_child("p:txBody").is_none() {
        node.children
            .push(XmlNode::Element(XmlElement::new("p:txBody").child(XmlElement::new("a:bodyPr")).child(XmlElement::new("a:lstStyle"))));
    }
    if let Some(body) = node.first_child_mut("p:txBody") {
        set_paragraphs(body, &shape.paragraphs, depth)?;
    }
    Ok(())
}

fn set_paragraphs(body: &mut XmlElement, paragraphs: &[PptxParagraph], depth: u64) -> Result<()> {
    let keep: Vec<XmlNode> =
        body.elements().filter(|c| c.name == "a:bodyPr" || c.name == "a:lstStyle").cloned().map(XmlNode::Element).collect();
    let ps = if paragraphs.is_empty() {
        "<a:p/>".to_string()
    } else {
        let mut s = String::new();
        for p in paragraphs {
            s.push_str(&build_paragraph(p, depth)?);
        }
        s
    };
    let parsed = parse_fragment(&ps, depth).map_err(|e| Error::invalid_model(format!("Invalid paragraph XML: {}", e.message)))?;
    body.children = keep.into_iter().chain(parsed).collect();
    Ok(())
}

fn build_paragraph(p: &PptxParagraph, depth: u64) -> Result<String> {
    let ppr = write_props("a:pPr", p.props.as_deref(), &to_map(&p.fields), PARA_FIELDS, &[], depth)?;
    let mut inner = String::new();
    for c in &p.children {
        match c {
            PptxInline::Raw(r) => inner.push_str(&r.xml),
            PptxInline::Run(r) => {
                let rpr = write_props("a:rPr", r.props.as_deref(), &to_map(&r.style), RUN_FIELDS, RPR_ORDER, depth)?;
                let normalized = r.text.replace("\r\n", "\n").replace('\r', "\n");
                for (i, line) in normalized.split('\n').enumerate() {
                    if i > 0 {
                        inner.push_str(&if rpr.is_empty() { "<a:br/>".to_string() } else { format!("<a:br>{rpr}</a:br>") });
                    }
                    if !line.is_empty() {
                        inner.push_str(&format!("<a:r>{rpr}<a:t>{}</a:t></a:r>", escape_xml(line)));
                    }
                }
            }
        }
    }
    Ok(format!("<a:p>{ppr}{inner}{}</a:p>", p.end_props.as_deref().unwrap_or("")))
}

fn write_table(node: &mut XmlElement, shape: &PptxTableShape, depth: u64) -> Result<()> {
    let Some(tbl) = node.find_descendant_mut("a:tbl") else { return Ok(()) };
    let rows: Vec<XmlElement> = tbl.children_named("a:tr").cloned().collect();
    let cols = tbl.first_child("a:tblGrid").map(|g| g.children_named("a:gridCol").count()).unwrap_or(0);
    let Some(last) = rows.last().cloned() else { return Ok(()) };
    let mut new_rows = Vec::new();
    for (i, r) in shape.rows.iter().enumerate() {
        if cols > 0 && r.cells.len() != cols {
            return Err(Error::invalid_model(format!(
                "Table #{}: row {} has {} cells, the table has {} columns",
                shape.id,
                i + 1,
                r.cells.len(),
                cols
            )));
        }
        let mut tr = rows.get(i).cloned().unwrap_or_else(|| last.clone());
        let tc_idx: Vec<usize> =
            tr.children.iter().enumerate().filter(|(_, c)| matches!(c, XmlNode::Element(e) if e.name == "a:tc")).map(|(i, _)| i).collect();
        for (j, cell) in r.cells.iter().enumerate() {
            let Some(&ti) = tc_idx.get(j) else { continue };
            let XmlNode::Element(tc) = &mut tr.children[ti] else { continue };
            if tc.first_child("a:txBody").is_none() {
                tc.children.insert(
                    0,
                    XmlNode::Element(XmlElement::new("a:txBody").child(XmlElement::new("a:bodyPr")).child(XmlElement::new("a:lstStyle"))),
                );
            }
            if let Some(body) = tc.first_child_mut("a:txBody") {
                set_paragraphs(body, &cell.paragraphs, depth)?;
            }
        }
        new_rows.push(XmlNode::Element(tr));
    }
    let others: Vec<XmlNode> = tbl.elements().filter(|c| c.name != "a:tr").cloned().map(XmlNode::Element).collect();
    tbl.children = others.into_iter().chain(new_rows).collect();
    Ok(())
}

pub fn pptx_text(model: &PptxDocument) -> String {
    model
        .slides
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut lines = vec![format!("--- Slide {} ---", i + 1)];
            for sh in &s.shapes {
                match sh {
                    PptxShape::Text(t) => lines.push(t.paragraphs.iter().map(paragraph_text).collect::<Vec<_>>().join("\n")),
                    PptxShape::Table(t) => lines.push(
                        t.rows
                            .iter()
                            .map(|r| {
                                r.cells
                                    .iter()
                                    .map(|c| c.paragraphs.iter().map(paragraph_text).collect::<Vec<_>>().join(" "))
                                    .collect::<Vec<_>>()
                                    .join("\t")
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => {}
                }
            }
            if let Some(n) = s.notes.as_deref().filter(|n| !n.is_empty()) {
                lines.push(format!("Notes: {n}"));
            }
            lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
