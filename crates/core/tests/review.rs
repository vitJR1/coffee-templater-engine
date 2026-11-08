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

// 3: deep section nesting must not overflow the stack
#[test]
fn deep_sections_are_rejected() {
    let deep = |n: usize| docx(&p(&[&format!("{}x{}", "{{#a}}".repeat(n), "{{/a}}".repeat(n))]));
    let e = compile(&deep(20_000), Options::default()).unwrap_err();
    assert_eq!(e.0[0].code, ErrorCode::LimitExceeded);
    assert_eq!(e.0[0].details["limit"], "sectionDepth");
    // the maximum depth still compiles, renders and inspects
    let max = coffee_core::limits::MAX_SECTION_DEPTH;
    assert!(compile(&deep(max + 1), Options::default()).is_err());
    let tpl = compile(&deep(max), Options::default()).unwrap();
    assert!(json_depth(&tpl.inspect().to_json()) < 120);
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

use common::render as render_xlsx;

fn json_depth(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        serde_json::Value::Object(o) => 1 + o.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
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
        &[("xl/worksheets/_rels/sheet1.xml.rels", table_rels), ("xl/tables/table1.xml", table)],
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

// ------------------------------------------------------------------------------------------
// Second review (items 14–40)
// ------------------------------------------------------------------------------------------

/// A loop over `items` in row `r` (cells A and B).
fn loop_row(r: i64) -> String {
    format!(r#"<row r="{r}">{}{}</row>"#, cell(&format!("A{r}"), "{{#items}}{{.}}"), cell(&format!("B{r}"), "{{/items}}"))
}

fn items(n: usize) -> String {
    format!(r#"{{"items":[{}]}}"#, (1..=n).map(|i| i.to_string()).collect::<Vec<_>>().join(","))
}

/// Renders one sheet `S` and returns its XML (checked to be well-formed).
fn render_sheet(sheet: &str, n: usize) -> String {
    let out = render_xlsx(&xlsx(&[("S", sheet)], "", &[]), &items(n));
    let xml = sheet_xml(&out, 1);
    coffee_core::xml::dom::parse_xml(&xml, 256).unwrap_or_else(|e| panic!("{e}: {xml}"));
    xml
}

// 14: a <dimension> with an end tag is removed whole
#[test]
fn xlsx_dimension_with_end_tag() {
    let sheet = format!(r#"<dimension ref="A1:B3"></dimension><sheetData>{}</sheetData>"#, loop_row(1));
    let xml = render_sheet(&sheet, 3);
    assert!(!xml.contains("dimension"), "{xml}");
}

// 15: hyperlinks and validations on removed rows leave valid XML and no empty wrappers
#[test]
fn xlsx_removed_hyperlinks_and_validations() {
    let sheet = format!(
        r#"<sheetData>{}</sheetData><dataValidations count="1"><dataValidation type="list" sqref="B2"><formula1>"a,b"</formula1></dataValidation></dataValidations><hyperlinks><hyperlink ref="A2" r:id="rId9"></hyperlink></hyperlinks>"#,
        loop_row(2)
    );
    let xml = render_sheet(&sheet, 0);
    assert!(!xml.contains("dataValidation") && !xml.contains("hyperlink"), "{xml}");
    let sheet = format!(
        r#"<sheetData>{}</sheetData><dataValidations count="2"><dataValidation sqref="B2"/><dataValidation sqref="C9"/></dataValidations>"#,
        loop_row(2)
    );
    let xml = render_sheet(&sheet, 0);
    assert!(xml.contains(r#"<dataValidations count="1"><dataValidation sqref="C8"/></dataValidations>"#), "{xml}");
}

// 16: blank rows around a removed loop row behave as in Excel
#[test]
fn xlsx_blank_rows_next_to_a_removed_loop() {
    let sheet = format!(
        r#"<sheetData><row r="1">{}</row>{}<row r="4">{}<c r="B4"><f>SUM(B2:B3)</f></c></row></sheetData><mergeCells count="1"><mergeCell ref="A3:B3"/></mergeCells>"#,
        cell("A1", "Head"),
        loop_row(2),
        cell("A4", "Total")
    );
    let xml = render_sheet(&sheet, 0);
    assert!(xml.contains(r#"<row r="3""#) && xml.contains(r#"<c r="B3"><f>SUM(B2:B2)</f></c>"#), "{xml}");
    assert!(xml.contains(r#"<mergeCell ref="A2:B2"/>"#), "{xml}");
    // the blank row before a removed row is kept
    let sheet =
        format!(r#"<sheetData><row r="1">{}</row>{}<row r="5">{}</row></sheetData>"#, cell("A1", "Head"), loop_row(3), cell("A5", "Total"));
    assert!(render_sheet(&sheet, 0).contains(r#"<row r="4""#));
}

// 17, 19, 20: sheet names, structured references, names and other workbooks are not references
#[test]
fn xlsx_formula_tokens_that_are_not_references() {
    let f = "SUM('FY5 Plan'!A:A)+VLOOKUP(A5,'Q4 Data'!A:C,2,FALSE)+SUM(Sales[Q4])+Sales[[#This Row],[FY2024]]+Q4!A1+Q4.Total+[1]Data!A5+SUM(5:5)";
    let sheet = format!(r#"<sheetData>{}<row r="5"><c r="A5"><f>{f}</f></c></row></sheetData>"#, loop_row(3));
    let xml = render_sheet(&sheet, 3);
    let want = f.replace("VLOOKUP(A5", "VLOOKUP(A7").replace("SUM(5:5)", "SUM(7:7)");
    assert!(xml.contains(&format!("<f>{want}</f>")), "{xml}");
}

#[test]
fn xlsx_qualified_names_and_external_references() {
    let data = format!(r#"<sheetData>{}<row r="5">{}</row></sheetData>"#, loop_row(3), cell("A5", "x"));
    let other = r#"<sheetData><row r="1"><c r="A1"><f>[1]Data!A5+Data!A5+Data!FY2024_Total*2+SUM([1]Data!A3:A3)+S1:S3!A5+Data!$5:$5</f></c></row></sheetData>"#;
    let names = r#"<definedNames><definedName name="Ext">[1]Data!$A$5</definedName><definedName name="Titles">Data!$3:$3</definedName></definedNames>"#;
    let out = render_xlsx(&xlsx(&[("Data", &data), ("Other", other)], names, &[]), &items(3));
    let o = sheet_xml(&out, 2);
    assert!(o.contains("<f>[1]Data!A5+Data!A7+Data!FY2024_Total*2+SUM([1]Data!A3:A3)+S1:S3!A5+Data!$7:$7</f>"), "{o}");
    let wb = read_part(&out, "xl/workbook.xml");
    assert!(wb.contains(">[1]Data!$A$5<") && wb.contains(">Data!$3:$5<"), "{wb}");
}

// 18: blocks of several rows: merges are duplicated, references stay inside their copy
#[test]
fn xlsx_multi_row_loop_blocks() {
    let sheet = format!(
        r#"<sheetData><row r="2">{}<c r="C2"><f>B3*2</f></c></row><row r="3">{}</row></sheetData><mergeCells count="1"><mergeCell ref="A2:A3"/></mergeCells>"#,
        cell("A2", "{{#items}}{{.}}"),
        cell("B3", "{{/items}}")
    );
    let xml = render_sheet(&sheet, 3);
    for m in ["A2:A3", "A4:A5", "A6:A7"] {
        assert!(xml.contains(&format!(r#"<mergeCell ref="{m}"/>"#)), "{m}: {xml}");
    }
    for f in ["<c r=\"C2\"><f>B3*2</f>", "<c r=\"C4\"><f>B5*2</f>", "<c r=\"C6\"><f>B7*2</f>"] {
        assert!(xml.contains(f), "{f}: {xml}");
    }
}

// 21, 22, 39: conditional formatting, validation, page breaks and protected ranges move too
#[test]
fn xlsx_sheet_ranges_follow_rows() {
    let sheet = format!(
        r#"<sheetData>{}<row r="5">{}</row></sheetData><conditionalFormatting sqref="A3"><cfRule type="expression" priority="1"><formula>$B$5&gt;100</formula></cfRule></conditionalFormatting><dataValidations count="1"><dataValidation type="list" sqref="B3"><formula1>$A$10:$A$11</formula1></dataValidation></dataValidations><protectedRanges><protectedRange name="in" sqref="B3:B3"/></protectedRanges><rowBreaks count="1" manualBreakCount="1"><brk id="5" max="16383" man="1"/></rowBreaks>"#,
        loop_row(3),
        cell("A5", "Total")
    );
    let xml = render_sheet(&sheet, 3);
    for want in [
        r#"<conditionalFormatting sqref="A3:A5">"#,
        "<formula>$B$7&gt;100</formula>",
        r#"<dataValidation type="list" sqref="B3:B5"><formula1>$A$12:$A$13</formula1>"#,
        r#"<protectedRange name="in" sqref="B3:B5"/>"#,
        r#"<brk id="7" max="16383" man="1"/>"#,
    ] {
        assert!(xml.contains(want), "{want}: {xml}");
    }
}

#[test]
fn xlsx_comments_and_drawings_follow_rows() {
    let sheet = format!(
        r#"<sheetData>{}<row r="5">{}</row></sheetData><drawing r:id="rId2"/><legacyDrawing r:id="rId3"/>"#,
        loop_row(3),
        cell("A5", "Total")
    );
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing" Target="../drawings/vmlDrawing1.vml"/></Relationships>"#;
    let comments = r#"<?xml version="1.0"?><comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>a</author></authors><commentList><comment ref="A5" authorId="0"><text><t>note</t></text></comment></commentList></comments>"#;
    let vml = r#"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel"><v:shape id="_x0000_s1025"><x:ClientData ObjectType="Note"><x:Anchor>1, 15, 3, 10, 3, 15, 7, 4</x:Anchor><x:Row>4</x:Row><x:Column>0</x:Column></x:ClientData></v:shape></xml>"#;
    let drawing = r#"<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>5</xdr:row></xdr:from><xdr:to><xdr:col>3</xdr:col><xdr:row>9</xdr:row></xdr:to></xdr:twoCellAnchor></xdr:wsDr>"#;
    let tpl = xlsx(
        &[("S", &sheet)],
        "",
        &[
            ("xl/worksheets/_rels/sheet1.xml.rels", rels),
            ("xl/comments1.xml", comments),
            ("xl/drawings/vmlDrawing1.vml", vml),
            ("xl/drawings/drawing1.xml", drawing),
        ],
    );
    let out = render_xlsx(&tpl, &items(3));
    assert!(read_part(&out, "xl/comments1.xml").contains(r#"<comment ref="A7""#));
    let v = read_part(&out, "xl/drawings/vmlDrawing1.vml");
    assert!(v.contains("<x:Row>6</x:Row>") && v.contains("<x:Anchor>1, 15, 5, 10, 3, 15, 9, 4</x:Anchor>"), "{v}");
    let d = read_part(&out, "xl/drawings/drawing1.xml");
    assert!(d.contains("<xdr:row>7</xdr:row>") && d.contains("<xdr:row>11</xdr:row>"), "{d}");
}

// 23: tables left without data rows keep one; tables whose rows are all gone are removed
#[test]
fn xlsx_tables_after_empty_loops() {
    let table_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../tables/table1.xml"/></Relationships>"#;
    let table = |r: &str| {
        format!(
            r#"<?xml version="1.0"?><table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" id="1" name="T" displayName="T" ref="{r}"><autoFilter ref="{r}"><sortState ref="{r}"><sortCondition ref="A2:A2"/></sortState></autoFilter></table>"#
        )
    };
    let body = |extra: &str| {
        format!(
            r#"<sheetData><row r="1">{}</row>{}<row r="4">{}</row></sheetData>{extra}<tableParts count="1"><tablePart r:id="rId1"/></tableParts>"#,
            cell("A1", "Name"),
            loop_row(2),
            cell("A4", "end")
        )
    };
    // header row 1, data row 2 (the loop)
    let tpl =
        xlsx(&[("S", &body(""))], "", &[("xl/worksheets/_rels/sheet1.xml.rels", table_rels), ("xl/tables/table1.xml", &table("A1:B2"))]);
    let t = read_part(&render_xlsx(&tpl, &items(0)), "xl/tables/table1.xml");
    assert!(t.contains(r#"ref="A1:B2""#) && t.contains(r#"<autoFilter ref="A1:B2">"#), "{t}");
    let t = read_part(&render_xlsx(&tpl, &items(3)), "xl/tables/table1.xml");
    assert!(t.contains(r#"<sortCondition ref="A2:A4"/>"#), "{t}");
    // the whole table inside the removed rows
    let tpl = xlsx(
        &[("S", &body(r#"<autoFilter ref="A2:B2"/>"#))],
        "",
        &[("xl/worksheets/_rels/sheet1.xml.rels", table_rels), ("xl/tables/table1.xml", &table("A2:B2"))],
    );
    let out = render_xlsx(&tpl, &items(0));
    assert!(!part_names(&out).contains(&"xl/tables/table1.xml".to_string()));
    let xml = sheet_xml(&out, 1);
    assert!(!xml.contains("tablePart") && !xml.contains("autoFilter"), "{xml}");
    assert!(!read_part(&out, "[Content_Types].xml").contains("table1"));
}

// 24: long paths do not make the sample shape (and the inspect response) arbitrarily deep
#[test]
fn inspect_depth_is_bounded() {
    let path = vec!["a"; 5000].join(".");
    let tpl = compile(&docx(&p(&[&format!("{{{{{path}}}}}")])), Options::default()).unwrap();
    let r = tpl.inspect();
    assert_eq!(r.variables, vec![path]);
    assert!(json_depth(&r.to_json()) < 100);
}

// 25: references to the sheet's own name inside a loop row stay in their copy
#[test]
fn xlsx_self_qualified_references_in_loop_rows() {
    let row = format!(
        r#"<row r="2">{}{}<c r="C2"><f>Data!A2*2+SUM(Data!A2:A2)</f></c></row>"#,
        cell("A2", "{{#items}}{{.}}"),
        cell("B2", "{{/items}}")
    );
    let out = render_xlsx(&xlsx(&[("Data", &format!("<sheetData>{row}</sheetData>"))], "", &[]), &items(3));
    let xml = sheet_xml(&out, 1);
    // like unqualified references (`A2*2`): each copy refers to itself
    for f in ["Data!A2*2+SUM(Data!A2:A2)", "Data!A3*2+SUM(Data!A3:A3)", "Data!A4*2+SUM(Data!A4:A4)"] {
        assert!(xml.contains(&format!("<f>{f}</f>")), "{f}: {xml}");
    }
}

// 26: cell text that looks like the old gap attribute is not touched
#[test]
fn xlsx_text_is_not_stripped() {
    let sheet = format!("<sheetData><row r=\"1\">{}</row></sheetData>", cell("A1", "{{v}}"));
    for v in [r#"keep data-ct-gap=\"x\" this"#, r#"a data-ct-gap=\"b"#] {
        let out = render_xlsx(&xlsx(&[("S", &sheet)], "", &[]), &format!(r#"{{"v":"{v}"}}"#));
        assert!(sheet_xml(&out, 1).contains(&v.replace("\\\"", "\"")), "{}", sheet_xml(&out, 1));
    }
}

// 27, 28: quotes in attributes
#[test]
fn xlsx_quoted_attributes() {
    let sheet = r#"<sheetData><row r="1"><c r="A1" t="inlineStr"><f a="</f>">B3</f><is><t>{{n}}</t></is></c></row></sheetData>"#;
    let out = render_xlsx(&xlsx(&[("S", sheet)], "", &[]), r#"{"n":5}"#);
    assert!(sheet_xml(&out, 1).contains("<v>5</v>"));
    let data = format!(r#"<sheetData>{}<row r="4">{}</row></sheetData>"#, loop_row(2), cell("A4", "x"));
    let names = r#"<definedNames><definedName name="Last" comment="a>b">S!$A$4</definedName></definedNames>"#;
    let out = render_xlsx(&xlsx(&[("S", &data)], names, &[]), &items(3));
    assert!(read_part(&out, "xl/workbook.xml").contains(r#"<definedName name="Last" comment="a>b">S!$A$6</definedName>"#));
}

// 29: drawings repeated by a loop get distinct ids
#[test]
fn docx_loop_copies_of_drawings_get_unique_ids() {
    let drawing = r#"<w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><wp:docPr id="1" name="Picture 1"/></wp:inline></w:drawing></w:r></w:p>"#;
    let tpl = docx(&[p(&["{{#items}}"]), drawing.to_string(), p(&["{{/items}}"])].concat());
    let doc = read_part(&render(&tpl, &items(3)), "word/document.xml");
    let ids: Vec<&str> = doc.match_indices("<wp:docPr id=\"").map(|(i, _)| doc[i + 14..].split('"').next().unwrap()).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 3, "{ids:?}");
    assert_eq!(ids[0], "1");
}

// 31: only image formats are accepted as extensions
#[test]
fn image_extension_must_be_an_image_format() {
    let tpl = compile(&docx(&p(&["{{%logo}}"])), Options::default()).unwrap();
    for bad in ["rels", "xml", "bin", "vml"] {
        let e = tpl.render(&data(&png_logo(&format!(r#","extension":"{bad}""#))), &mut NoHost).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidImage, "{bad}");
    }
}

// 32, 33: ids near u32::MAX and ids written in other forms
#[test]
fn drawing_ids_overflow_and_other_spellings() {
    for docpr in
        [r#"<wp:docPr id="4294967295" name="x"/>"#, r#"<wp:docPr id='28673' name="x"/>"#, r#"<wp14:docPr  id = "28673" name="x"/>"#]
    {
        let existing = format!(
            r#"<w:p><w:r><w:drawing><wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:wp14="x">{docpr}</wp:inline></w:drawing></w:r></w:p>"#
        );
        let tpl = docx(&[existing, p(&["{{%logo}}"]), p(&["{{%logo}}"])].concat());
        let doc = read_part(&render(&tpl, &png_logo("")), "word/document.xml");
        let ids: Vec<String> = coffee_core::xml::tokenizer::find_local_tags(&doc, "docPr")
            .map(|(s, e)| coffee_core::xml::tokenizer::raw_attr(&doc[s..e], "id").unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 3, "{doc}");
        assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 3, "{docpr}: {ids:?}");
    }
}

// 32, 36: row numbers are bounded by the sheet
#[test]
fn xlsx_ranges_to_the_last_row() {
    let sheet = format!(
        r#"<sheetData>{}<row r="5"><c r="A5"><f>SUM(A2:A1048576)+A9223372036854775807</f></c></row></sheetData><conditionalFormatting sqref="D1:D1048576"><cfRule type="expression" priority="1"><formula>1</formula></cfRule></conditionalFormatting>"#,
        loop_row(3)
    );
    let xml = render_sheet(&sheet, 5);
    assert!(xml.contains("<f>SUM(A2:A1048576)+A9223372036854775807</f>"), "{xml}");
    assert!(xml.contains(r#"sqref="D1:D1048576""#), "{xml}");
}

// 37: shared formulas in loop rows become ordinary formulas
#[test]
fn xlsx_shared_formulas_in_loop_rows() {
    let row = format!(
        r#"<row r="3">{}{}<c r="D3"><f t="shared" ref="D3:F3" si="0">$A3*2+B5</f></c><c r="E3"><f t="shared" si="0"/></c><c r="F3"><f t="shared" si="0"/></c></row>"#,
        cell("A3", "{{#items}}{{.}}"),
        cell("B3", "{{/items}}")
    );
    let xml = render_sheet(&format!(r#"<sheetData>{row}<row r="5">{}</row></sheetData>"#, cell("A5", "t")), 3);
    assert!(!xml.contains("shared"), "{xml}");
    for f in ["<c r=\"D3\"><f>$A3*2+B7</f>", "<c r=\"E4\"><f>$A4*2+C7</f>", "<c r=\"F5\"><f>$A5*2+D7</f>"] {
        assert!(xml.contains(f), "{f}: {xml}");
    }
}

// 38: a closing tag in a comment before the merges
#[test]
fn xlsx_merge_cells_after_a_comment() {
    let sheet = format!(
        r#"<sheetData>{}</sheetData><!-- </mergeCells> --><mergeCells count="1"><mergeCell ref="A5:B5"/></mergeCells>"#,
        loop_row(3)
    );
    assert!(render_sheet(&sheet, 3).contains(r#"<mergeCell ref="A7:B7"/>"#));
}

// 40: the section depth limit keeps the errors found before it
#[test]
fn section_depth_error_keeps_other_errors() {
    let n = coffee_core::limits::MAX_SECTION_DEPTH + 1;
    let tpl = docx(&[p(&["{{/oops}}"]), p(&["{{else}}"]), p(&[&format!("{}x{}", "{{#a}}".repeat(n), "{{/a}}".repeat(n))])].concat());
    let e = compile(&tpl, Options::default()).unwrap_err();
    let codes: Vec<ErrorCode> = e.0.iter().map(|e| e.code).collect();
    assert!(
        codes.contains(&ErrorCode::LimitExceeded) && codes.iter().filter(|c| **c == ErrorCode::UnmatchedClose).count() == 2,
        "{codes:?}"
    );
}
