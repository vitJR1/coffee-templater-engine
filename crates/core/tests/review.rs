//! Regression tests for CODE_REVIEW.md.

mod common;
use coffee_core::{ErrorCode, NoHost, Options};
use common::*;

fn png_logo(extra: &str) -> String {
    format!(r#"{{"logo":{{"data":{{"$base64":"{PNG_2X1}"}}{extra}}}}}"#)
}

// 1, 2: the image extension goes into zip paths and XML attributes
#[test]
fn image_extension_is_whitelisted() {
    let tpl = compile(&docx(&p(&["{{%logo}}"])), Options::default()).unwrap();
    for bad in [
        r#","extension":"png/../../../../../tmp/evil""#,
        r#","extension":"png\"/><Override PartName=\"/word/document.xml\" ContentType=\"text/plain""#,
        r#","extension":"averyveryverylongext""#,
    ] {
        let e = tpl.render(&data(&png_logo(bad)), &mut NoHost).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidImage, "{bad}");
    }
    let out = tpl.render(&data(&png_logo(r#","extension":".PNG""#)), &mut NoHost).unwrap();
    assert!(part_names(&out).contains(&"word/media/image_ct1.png".to_string()));
}

#[test]
fn zip_writer_rejects_unsafe_names() {
    assert!(coffee_core::zip::write(vec![("word/media/x.png/../../evil", b"x".as_slice())]).is_err());
    assert!(coffee_core::zip::write(vec![("/abs", b"x".as_slice())]).is_err());
}

// 3: deep section nesting must not overflow the stack
#[test]
fn deep_sections_are_rejected() {
    let deep = |n: usize| docx(&p(&[&format!("{}x{}", "{{#a}}".repeat(n), "{{/a}}".repeat(n))]));
    let e = compile(&deep(20_000), Options::default()).unwrap_err();
    assert_eq!(e.0[0].code, ErrorCode::LimitExceeded);
    assert_eq!(e.0[0].details["limit"], "sectionDepth");
    // the maximum depth still compiles, renders and inspects
    let tpl = compile(&deep(128), Options::default()).unwrap();
    tpl.inspect();
    assert!(body(&tpl.render(&data(r#"{"a":true}"#), &mut NoHost).unwrap()).contains('x'));
}

#[test]
fn deep_xml_is_rejected_even_without_a_limit() {
    let deep = docx(&format!("{}{}", "<w:sdt>".repeat(5000), "</w:sdt>".repeat(5000)));
    let e = coffee_core::parse(&deep, &coffee_core::Limits { max_xml_depth: 0, ..Default::default() }).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded);
}

// 5: the same image rendered many times is stored once
#[test]
fn images_are_deduplicated() {
    let tpl = compile(&docx(&[p(&["{{#items}}"]), p(&["{{%logo}}"]), p(&["{{/items}}"])].concat()), Options::default()).unwrap();
    let items = vec![format!(r#"{{"logo":{{"data":{{"$base64":"{PNG_2X1}"}}}}}}"#); 50].join(",");
    let out = tpl.render(&data(&format!(r#"{{"items":[{items}]}}"#)), &mut NoHost).unwrap();
    assert_eq!(part_names(&out).iter().filter(|n| n.contains("media/")).count(), 1);
    let doc = read_part(&out, "word/document.xml");
    assert_eq!(doc.matches("<w:drawing>").count(), 50);
    assert_eq!(read_part(&out, "word/_rels/document.xml.rels").matches("media/image_ct1.png").count(), 1);
    // drawing ids stay unique
    let ids: std::collections::BTreeSet<&str> = doc.match_indices("<wp:docPr id=\"").map(|(i, _)| &doc[i + 15..i + 20]).collect();
    assert_eq!(ids.len(), 50);
}

fn sheet_xml(out: &[u8], n: usize) -> String {
    read_part(out, &format!("xl/worksheets/sheet{n}.xml"))
}

fn render_xlsx(tpl: &[u8], json: &str) -> Vec<u8> {
    render(tpl, json)
}

// 6: copies of a looped row are contiguous even when a blank row precedes the loop
#[test]
fn xlsx_loop_copies_are_contiguous() {
    let loop_row = cell("A3", "{{#items}}{{.}}") + &cell("B3", "{{/items}}");
    let sheet = format!(
        r#"<sheetData><row r="1">{}</row><row r="3">{}</row><row r="5">{}<c r="B5"><f>SUM(A3:A3)</f></c></row></sheetData><mergeCells count="1"><mergeCell ref="A5:C5"/></mergeCells>"#,
        cell("A1", "Head"),
        loop_row,
        cell("A5", "Total")
    );
    let out = render_xlsx(&xlsx(&[("S", &sheet)], "", &[]), r#"{"items":[1,2,3]}"#);
    let xml = sheet_xml(&out, 1);
    for r in ["<row r=\"3\"", "<row r=\"4\"", "<row r=\"5\"", "<row r=\"7\""] {
        assert!(xml.contains(r), "{r} in {xml}");
    }
    assert!(xml.contains("<f>SUM(A3:A5)</f>"), "{xml}");
    assert!(xml.contains(r#"<mergeCell ref="A7:C7"/>"#), "{xml}");
}

// 7: references to rows removed by an empty loop become #REF!
#[test]
fn xlsx_references_to_removed_rows_become_ref_errors() {
    let sheet = format!(
        r#"<sheetData><row r="2">{}</row><row r="3">{}</row><row r="4">{}</row><row r="6"><c r="A6"><f>B3+B4</f></c><c r="B6"><f>SUM(B2:B4)</f></c><c r="C6"><f>SUM(B3:B3)</f></c></row></sheetData>"#,
        cell("A2", "head"),
        cell("A3", "{{#items}}{{.}}"),
        cell("A4", "x{{/items}}")
    );
    let out = render_xlsx(&xlsx(&[("S", &sheet)], "", &[]), r#"{"items":[]}"#);
    let xml = sheet_xml(&out, 1);
    assert!(xml.contains("<f>#REF!+#REF!</f>"), "{xml}");
    assert!(xml.contains("<f>SUM(B2:B2)</f>"), "range is clamped to the surviving rows: {xml}");
    assert!(xml.contains("<f>SUM(#REF!)</f>"), "{xml}");
    assert!(xml.contains(r#"<row r="4""#), "row 6 moved up by 2: {xml}");
}

// 8: tables, auto filters, defined names and other sheets follow the moved rows
#[test]
fn xlsx_updates_tables_names_and_cross_sheet_references() {
    let loop_row = cell("A3", "{{#items}}{{.}}") + &cell("B3", "{{/items}}");
    let sheet = format!(
        r#"<sheetData><row r="2">{}</row><row r="3">{}</row><row r="5">{}</row></sheetData><autoFilter ref="A2:B3"/><hyperlinks><hyperlink ref="A5" r:id="rId9"/></hyperlinks><tableParts count="1"><tablePart r:id="rId1"/></tableParts>"#,
        cell("A2", "Name"),
        loop_row,
        cell("A5", "end")
    );
    let other = r#"<sheetData><row r="1"><c r="A1"><f>Data!A5&amp;'Data'!A3&amp;SUM(Data!A3:A3)&amp;Other!A5</f></c></row></sheetData>"#;
    let names = r#"<definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">Data!$A$2:$B$5</definedName><definedName name="Last">'Data'!$A$5</definedName></definedNames>"#;
    let table_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../tables/table1.xml"/></Relationships>"#;
    let table = r#"<?xml version="1.0"?><table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" id="1" name="T" displayName="T" ref="A2:B3"><autoFilter ref="A2:B3"/><tableColumns count="2"><tableColumn id="1" name="Name"/><tableColumn id="2" name="X"/></tableColumns></table>"#;
    let tpl = xlsx(
        &[("Data", &sheet), ("Other", other)],
        names,
        &[("xl/worksheets/_rels/sheet1.xml.rels", "", table_rels), ("xl/tables/table1.xml", "", table)],
    );
    let out = render_xlsx(&tpl, r#"{"items":[1,2,3,4,5,6,7,8,9,10]}"#);
    let xml = sheet_xml(&out, 1);
    assert!(xml.contains(r#"<autoFilter ref="A2:B12"/>"#), "{xml}");
    assert!(xml.contains(r#"<hyperlink ref="A14""#), "{xml}");
    let t = read_part(&out, "xl/tables/table1.xml");
    assert!(t.contains(r#"ref="A2:B12""#) && t.contains(r#"<autoFilter ref="A2:B12"/>"#), "{t}");
    let wb = read_part(&out, "xl/workbook.xml");
    assert!(wb.contains(">Data!$A$2:$B$14</definedName>"), "{wb}");
    assert!(wb.contains(">'Data'!$A$14</definedName>"), "{wb}");
    let o = sheet_xml(&out, 2);
    assert!(o.contains("<f>Data!A14&amp;'Data'!A3&amp;SUM(Data!A3:A12)&amp;Other!A5</f>"), "{o}");
}

// 9: a formula in a cell that receives a scalar value is kept
#[test]
fn xlsx_keeps_formula_of_scalar_cell() {
    let sheet = r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><f>B3*2</f><is><t>{{qty}}</t></is></c></row></sheetData>"#;
    let out = render_xlsx(&xlsx(&[("S", sheet)], "", &[]), r#"{"qty":5}"#);
    assert!(sheet_xml(&out, 1).contains(r#"<c r="A1"><f>B3*2</f><v>5</v></c>"#), "{}", sheet_xml(&out, 1));
}

// 10: drawing ids continue after the ids already present in the template
#[test]
fn drawing_ids_do_not_collide_with_the_template() {
    let first = render(&docx(&p(&["{{%logo}}"])), &png_logo(""));
    let first_doc = read_part(&first, "word/document.xml");
    assert!(first_doc.contains(r#"<wp:docPr id="28673""#));
    // use the generated document (with an image) as a new template
    let mut entries = coffee_core::zip::read(&first, &Default::default()).unwrap();
    for (name, data) in entries.iter_mut() {
        if name == "word/document.xml" {
            *data = String::from_utf8(data.clone()).unwrap().replace("</w:body>", &format!("{}</w:body>", p(&["{{%logo}}"]))).into_bytes();
        }
    }
    let tpl = coffee_core::zip::write(entries.iter().map(|(n, d)| (n.as_str(), d.as_slice()))).unwrap();
    let doc = read_part(&render(&tpl, &png_logo("")), "word/document.xml");
    assert!(doc.contains(r#"<wp:docPr id="28673""#) && doc.contains(r#"<wp:docPr id="28674""#), "{doc}");
}
