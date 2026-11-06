//! Minimal empty documents used by `build` when no base file is given.

use crate::opc::Package;
use indexmap::IndexMap;
use std::rc::Rc;

const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn pkg_from(files: &[(&str, String)]) -> Package {
    let mut map = IndexMap::new();
    for (k, v) in files {
        map.insert(k.to_string(), Rc::from(format!("{XML_HEAD}{v}").into_bytes()));
    }
    Package::new(map)
}

fn style(id: &str, name: &str, extra: &str) -> String {
    format!("<w:style w:type=\"paragraph\" w:styleId=\"{id}\"><w:name w:val=\"{name}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>{extra}</w:style>")
}

/// A minimal empty Word document with common styles (Normal, Title, Heading1-3, ListParagraph, TableGrid).
pub fn blank_docx() -> Package {
    let borders: String = ["top", "left", "bottom", "right", "insideH", "insideV"]
        .iter()
        .map(|b| format!("<w:{b} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>"))
        .collect();
    let styles = format!(
        "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:cs=\"Calibri\" w:eastAsia=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
{}{}{}{}{}\
<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style>\
<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>\
<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:basedOn w:val=\"TableNormal\"/><w:tblPr><w:tblBorders>{borders}</w:tblBorders></w:tblPr></w:style>\
</w:styles>",
        style("Title", "Title", "<w:rPr><w:sz w:val=\"56\"/></w:rPr>"),
        style("Heading1", "heading 1", "<w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"32\"/></w:rPr>"),
        style("Heading2", "heading 2", "<w:pPr><w:keepNext/><w:spacing w:before=\"200\" w:after=\"100\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"28\"/></w:rPr>"),
        style("Heading3", "heading 3", "<w:pPr><w:keepNext/><w:spacing w:before=\"160\" w:after=\"80\"/><w:outlineLvl w:val=\"2\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"24\"/></w:rPr>"),
        style("ListParagraph", "List Paragraph", "<w:pPr><w:ind w:left=\"720\"/></w:pPr>"),
    );
    pkg_from(&[
        (
            "[Content_Types].xml",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
</Types>"
                .into(),
        ),
        ("_rels/.rels", format!("<Relationships xmlns=\"{RELS_NS}\"><Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"word/document.xml\"/></Relationships>")),
        ("word/_rels/document.xml.rels", format!("<Relationships xmlns=\"{RELS_NS}\"><Relationship Id=\"rId1\" Type=\"{R}/styles\" Target=\"styles.xml\"/></Relationships>")),
        (
            "word/document.xml",
            format!(
                "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"{R}\" \
xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<w:body><w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1134\" w:right=\"850\" w:bottom=\"1134\" w:left=\"1701\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"
            ),
        ),
        ("word/styles.xml", styles),
    ])
}

/// A minimal empty workbook (no sheets; they are created from the model).
pub fn blank_xlsx() -> Package {
    pkg_from(&[
        (
            "[Content_Types].xml",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>\
</Types>"
                .into(),
        ),
        ("_rels/.rels", format!("<Relationships xmlns=\"{RELS_NS}\"><Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>")),
        ("xl/_rels/workbook.xml.rels", format!("<Relationships xmlns=\"{RELS_NS}\"><Relationship Id=\"rId1\" Type=\"{R}/styles\" Target=\"styles.xml\"/></Relationships>")),
        ("xl/workbook.xml", format!("<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"{R}\"><sheets/></workbook>")),
        (
            "xl/styles.xml",
            "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<fonts count=\"1\"><font><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font></fonts>\
<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>\
<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
<cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"14\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/></cellXfs>\
<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>\
</styleSheet>"
                .into(),
        ),
    ])
}
