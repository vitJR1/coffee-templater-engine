//! DOCX adapter.

use crate::error::Result;
use crate::image::{ResolvedImage, EMU_PER_PX};
use crate::opc::{rel, Package};
use crate::template::engine::{FormatHooks, FormatSpec, ImageMode, RenderCtx, TokenContext};
use crate::xml::escape::escape_attr;

const PRESERVE_T: &str = "<w:t xml:space=\"preserve\">";

pub static SPEC: FormatSpec = FormatSpec {
    text_tags: &["w:t"],
    group_tags: &["w:p"],
    paragraph_tag: Some("w:p"),
    row_tags: &["w:tr"],
    content_tags: &["w:drawing", "w:pict", "w:object", "w:tab", "w:br", "w:sym", "w:fldChar", "w:fldSimple", "m:oMath", "m:oMathPara"],
    preserve_space: true,
    image_mode: ImageMode::Inline,
    shape_tag: None,
};

pub fn main_part(pkg: &Package) -> String {
    pkg.main_document_path().unwrap_or_else(|| "word/document.xml".into())
}

pub fn template_parts(pkg: &Package) -> Vec<String> {
    let main = main_part(pkg);
    let mut parts = vec![main.clone()];
    for t in [rel::HEADER, rel::FOOTER, rel::FOOTNOTES, rel::ENDNOTES] {
        for (_, path) in pkg.related_parts(&main, t) {
            if pkg.has(&path) && !parts.contains(&path) {
                parts.push(path);
            }
        }
    }
    parts
}

pub struct Docx;

impl FormatHooks for Docx {
    fn spec(&self) -> &'static FormatSpec {
        &SPEC
    }

    fn breaks(&self, _ctx: &TokenContext) -> (Option<String>, Option<String>) {
        (Some(format!("</w:t><w:br/>{PRESERVE_T}")), Some(format!("</w:t><w:tab/>{PRESERVE_T}")))
    }

    fn render_image(&self, img: &ResolvedImage, ctx: &mut RenderCtx, _shape: Option<&str>) -> Result<String> {
        let part = ctx.part.clone();
        let r_id = ctx.pkg.add_image(&part, "word/media", img.data.clone(), &img.extension)?;
        ctx.doc_pr = ctx.doc_pr.checked_add(1).unwrap_or(1); // repeats are renumbered after rendering
        let id = ctx.doc_pr;
        let cx = (img.width * EMU_PER_PX).round() as i64;
        let cy = (img.height * EMU_PER_PX).round() as i64;
        let alt = escape_attr(&img.alt);
        let a = "http://schemas.openxmlformats.org/drawingml/2006/main";
        let pic = "http://schemas.openxmlformats.org/drawingml/2006/picture";
        let ext = escape_attr(&img.extension);
        Ok(format!(
            "</w:t><w:drawing>\
<wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
<wp:docPr id=\"{id}\" name=\"Picture {id}\" descr=\"{alt}\"/>\
<wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"{a}\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>\
<a:graphic xmlns:a=\"{a}\"><a:graphicData uri=\"{pic}\"><pic:pic xmlns:pic=\"{pic}\">\
<pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"image{id}.{ext}\" descr=\"{alt}\"/><pic:cNvPicPr/></pic:nvPicPr>\
<pic:blipFill><a:blip r:embed=\"{r_id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>{PRESERVE_T}"
        ))
    }
}
