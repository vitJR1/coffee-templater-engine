//! PPTX adapter.

use crate::error::Result;
use crate::image::{ResolvedImage, EMU_PER_PX};
use crate::opc::{rel, Package};
use crate::template::engine::{FormatHooks, FormatSpec, ImageMode, RenderCtx, Tok, TokenContext, XmlKind};
use crate::xml::dom::parse_xml;
use crate::xml::escape::escape_attr;
use crate::xml::tokenizer::raw_attr;

pub static SPEC: FormatSpec = FormatSpec {
    text_tags: &["a:t"],
    group_tags: &["a:p"],
    paragraph_tag: Some("a:p"),
    row_tags: &["a:tr"],
    content_tags: &["a:br", "a:fld", "a14:m", "mc:AlternateContent"],
    preserve_space: false,
    image_mode: ImageMode::Shape,
    shape_tag: Some("p:sp"),
};

pub fn presentation_path(pkg: &Package) -> String {
    pkg.main_document_path().unwrap_or_else(|| "ppt/presentation.xml".into())
}

pub struct SlideRef {
    pub path: String,
    pub r_id: String,
    pub id: String,
}

/// Slides in presentation order.
pub fn slide_parts(pkg: &Package) -> Vec<SlideRef> {
    let pres = presentation_path(pkg);
    let Some(xml) = pkg.get_text(&pres) else { return Vec::new() };
    let Ok(doc) = parse_xml(&xml, pkg.max_xml_depth) else { return Vec::new() };
    let rels: Vec<(String, String)> = pkg.related_parts(&pres, rel::SLIDE).into_iter().map(|(r, p)| (r.id, p)).collect();
    let Some(list) = doc.root.first_child("p:sldIdLst") else { return Vec::new() };
    list.children_named("p:sldId")
        .filter_map(|s| {
            let r_id = s.attr("r:id").unwrap_or_default().to_string();
            let path = rels.iter().find(|(id, _)| *id == r_id).map(|(_, p)| p.clone())?;
            if !pkg.has(&path) {
                return None;
            }
            Some(SlideRef { path, r_id, id: s.attr("id").unwrap_or_default().to_string() })
        })
        .collect()
}

pub fn template_parts(pkg: &Package) -> Vec<String> {
    let mut parts = Vec::new();
    for s in slide_parts(pkg) {
        parts.push(s.path.clone());
        for (_, notes) in pkg.related_parts(&s.path, rel::NOTES_SLIDE) {
            if pkg.has(&notes) {
                parts.push(notes);
            }
        }
    }
    parts
}

fn run_props(tokens: &[Tok], run_idx: usize, ctx: &TokenContext) -> String {
    let Some(Tok::Xml { raw, name, kind }) = tokens.get(run_idx + 1) else { return String::new() };
    if name != "a:rPr" {
        return String::new();
    }
    if *kind == XmlKind::SelfClosing {
        return raw.clone();
    }
    let Some(&end) = ctx.close_of.get(&(run_idx + 1)) else { return String::new() };
    tokens[run_idx + 1..=end]
        .iter()
        .map(|t| match t {
            Tok::Xml { raw, .. } => raw.as_str(),
            _ => "",
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Box_ {
    x: i64,
    y: i64,
    cx: i64,
    cy: i64,
}

/// First `<tag ...>` (open or self-closing) in `xml`, as raw text.
fn first_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(p) = xml[from..].find(tag).map(|p| p + from) {
        from = p + tag.len();
        let next = xml.as_bytes().get(p + tag.len()).copied();
        if !matches!(next, Some(c) if c.is_ascii_whitespace() || c == b'>' || c == b'/') {
            continue;
        }
        let end = xml[p..].find('>')? + p + 1;
        return Some(&xml[p..end]);
    }
    None
}

/// `<p:spPr .../>` or `<p:spPr ...>...</p:spPr>`.
fn sp_pr(xml: &str) -> &str {
    let Some(open) = first_tag(xml, "<p:spPr") else { return "" };
    let start = xml.find(open).unwrap_or(0);
    if open.ends_with("/>") {
        return open;
    }
    match xml[start..].find("</p:spPr>") {
        Some(e) => &xml[start..start + e + "</p:spPr>".len()],
        None => "",
    }
}

fn int_attr(tag: Option<&str>, name: &str) -> Option<i64> {
    raw_attr(tag?, name)?.parse().ok()
}

fn xfrm_of(xml: &str) -> Option<Box_> {
    let sp = sp_pr(xml);
    let off = first_tag(sp, "<a:off");
    let ext = first_tag(sp, "<a:ext");
    Some(Box_ { x: int_attr(off, "x")?, y: int_attr(off, "y")?, cx: int_attr(ext, "cx")?.max(0), cy: int_attr(ext, "cy")?.max(0) })
}

/// Position of a placeholder shape inherited from the slide layout.
fn layout_box(pkg: &Package, slide: &str, shape_xml: &str) -> Option<Box_> {
    let ph = first_tag(shape_xml, "<p:ph")?;
    let ph_type = raw_attr(ph, "type");
    let ph_idx = raw_attr(ph, "idx");
    let layout = pkg.related_parts(slide, rel::SLIDE_LAYOUT).into_iter().next()?.1;
    let xml = pkg.get_text(&layout)?;
    let mut from = 0;
    while let Some(st) = xml[from..].find("<p:sp").map(|p| p + from) {
        let next = xml.as_bytes().get(st + 5).copied();
        from = st + 5;
        if !matches!(next, Some(c) if c.is_ascii_whitespace() || c == b'>') {
            continue;
        }
        let Some(end) = xml[st..].find("</p:sp>").map(|e| st + e + 7) else { break };
        let sp = &xml[st..end];
        from = end;
        let Some(lph) = first_tag(sp, "<p:ph") else { continue };
        let (lt, li) = (raw_attr(lph, "type"), raw_attr(lph, "idx"));
        let matches = match ph_idx {
            Some(i) => li == Some(i),
            None => ph_type.is_some() && lt == ph_type,
        };
        if matches {
            return xfrm_of(sp);
        }
    }
    None
}

pub struct Pptx;

impl FormatHooks for Pptx {
    fn spec(&self) -> &'static FormatSpec {
        &SPEC
    }

    fn breaks(&self, ctx: &TokenContext) -> (Option<String>, Option<String>) {
        if ctx.stack.len() < 2 {
            return (None, None);
        }
        let run = ctx.stack[ctx.stack.len() - 2];
        if ctx.tokens[run].name() != "a:r" {
            return (None, None);
        }
        let rpr = run_props(ctx.tokens, run, ctx);
        let br = if rpr.is_empty() { "<a:br/>".to_string() } else { format!("<a:br>{rpr}</a:br>") };
        (Some(format!("</a:t></a:r>{br}<a:r>{rpr}<a:t>")), None)
    }

    fn render_image(&self, img: &ResolvedImage, ctx: &mut RenderCtx, shape: Option<&str>) -> Result<String> {
        let shape = shape.unwrap_or("");
        let cnv = first_tag(shape, "<p:cNvPr");
        let id = match cnv.and_then(|t| raw_attr(t, "id")).filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())) {
            Some(v) => v.to_string(),
            None => {
                if ctx.shape_id == 0 {
                    ctx.shape_id = 0x7000;
                }
                ctx.shape_id += 1;
                ctx.shape_id.to_string()
            }
        };
        let name = cnv.and_then(|t| raw_attr(t, "name")).map(str::to_string).unwrap_or_else(|| format!("Picture {id}"));
        let part = ctx.part.clone();
        let bx = xfrm_of(shape).or_else(|| layout_box(ctx.pkg, &part, shape));
        let mut cx = (img.width * EMU_PER_PX).round();
        let mut cy = (img.height * EMU_PER_PX).round();
        let (mut x, mut y) = (0i64, 0i64);
        if let Some(b) = bx {
            // fit ("contain") the picture into the text box, centered
            let scale = (b.cx as f64 / cx).min(b.cy as f64 / cy);
            if scale.is_finite() {
                cx = (cx * scale).round();
                cy = (cy * scale).round();
            }
            x = b.x + ((b.cx as f64 - cx) / 2.0).round() as i64;
            y = b.y + ((b.cy as f64 - cy) / 2.0).round() as i64;
        }
        let r_id = ctx.pkg.add_image(&part, "ppt/media", img.data.clone(), &img.extension);
        let (cx, cy) = (cx as i64, cy as i64);
        Ok(format!(
            "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"{name}\" descr=\"{}\"/>\
<p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>\
<p:blipFill><a:blip r:embed=\"{r_id}\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>\
<p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>",
            escape_attr(&img.alt)
        ))
    }
}
