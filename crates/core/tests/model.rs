//! Port of `test/model.test.ts` from the TS implementation.

mod common;
use coffee_core::model::docx::{DocxBlock, DocxInline, PartKind};
use coffee_core::model::pptx::PptxShape;
use coffee_core::{build, extract_text, parse, ErrorCode, Limits, OfficeDocument};
use common::*;
use serde_json::{json, Value as SJson};

fn lim() -> Limits {
    Limits::default()
}

/// JSON round trip of the model, like `JSON.parse(JSON.stringify(doc))`.
fn json_of(doc: &OfficeDocument) -> SJson {
    serde_json::from_str(&serde_json::to_string(doc).unwrap()).unwrap()
}

fn from_json(v: SJson) -> OfficeDocument {
    OfficeDocument::from_json(v).unwrap()
}

// ------------------------------------------------------------------------------- docx

#[test]
fn docx_parses_paragraphs_styles_runs_and_tables() {
    let doc = json_of(&parse(&fixture("invoice.docx"), &lim()).unwrap());
    assert_eq!(doc["format"], "docx");
    assert_eq!(doc["modelVersion"], 1);
    let body = &doc["parts"][0];
    assert_eq!(body["kind"], "body");
    let blocks = &body["blocks"];
    assert_eq!(blocks[0]["type"], "paragraph");
    assert_eq!(blocks[0]["style"], "Heading1");
    assert_eq!(blocks[0]["children"][0]["text"], "Invoice {{number}}");
    let client = &blocks[1]["children"];
    assert_eq!(client[0]["text"], "Client: ");
    assert_eq!(client[1]["text"], "{{cli");
    assert_eq!(client[1]["bold"], true);
    assert_eq!(client[2]["text"], "ent.name}}");
    let table = blocks.as_array().unwrap().iter().find(|b| b["type"] == "table").unwrap();
    let heads: Vec<&SJson> = table["rows"][0]["cells"].as_array().unwrap().iter().map(|c| &c["blocks"][0]["children"][0]["text"]).collect();
    assert_eq!(heads, vec!["Name", "Qty", "Price"]);
    assert!(body["sectionProps"].as_str().unwrap().contains("<w:pgSz"));
    assert!(doc["parts"].as_array().unwrap().iter().any(|p| p["kind"] == "header"));
}

#[test]
fn docx_is_json_serializable_and_round_trips_without_changes() {
    let doc = parse(&fixture("invoice.docx"), &lim()).unwrap();
    let rebuilt = build(&from_json(json_of(&doc)), Some(&fixture("invoice.docx")), &lim()).unwrap();
    assert_eq!(json_of(&parse(&rebuilt, &lim()).unwrap()), json_of(&doc));
}

#[test]
fn docx_applies_edits_and_keeps_original_formatting() {
    let mut doc = json_of(&parse(&fixture("invoice.docx"), &lim()).unwrap());
    let blocks = doc["parts"][0]["blocks"].as_array_mut().unwrap();
    let total = blocks
        .iter_mut()
        .find(|b| {
            b["type"] == "paragraph"
                && b["children"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|r| r["type"] == "run" && r["text"].as_str().unwrap_or("").starts_with("Total")))
        })
        .unwrap();
    let run = &mut total["children"][0];
    assert_eq!(run["size"], 14);
    run["text"] = json!("Total: 100");
    run["bold"] = json!(true);
    run["color"] = json!("FF0000");
    blocks.push(json!({ "type": "paragraph", "align": "center", "children": [{ "type": "run", "text": "Line 1\nLine 2\tTabbed", "italic": true }] }));
    blocks.push(json!({ "type": "paragraph", "children": [{ "type": "hyperlink", "url": "https://example.com", "children": [{ "type": "run", "text": "link", "underline": true }] }] }));

    let out = build(&from_json(doc), Some(&fixture("invoice.docx")), &lim()).unwrap();
    let xml = read_part(&out, "word/document.xml");
    // fields are written in schema order, the original size is kept
    assert!(
        xml.contains(r#"<w:rPr><w:b/><w:color w:val="FF0000"/><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">Total: 100</w:t>"#),
        "{xml}"
    );
    assert!(xml.contains(r#"<w:pPr><w:jc w:val="center"/></w:pPr>"#));
    assert!(xml.contains(r#"<w:t xml:space="preserve">Line 1</w:t><w:br/><w:t xml:space="preserve">Line 2</w:t><w:tab/>"#));
    assert!(regex_lite::Regex::new(r#"Target="https://example\.com" TargetMode="External""#)
        .unwrap()
        .is_match(&read_part(&out, "word/_rels/document.xml.rels")));

    let reparsed = json_of(&parse(&out, &lim()).unwrap());
    let blocks = reparsed["parts"][0]["blocks"].as_array().unwrap();
    let last = &blocks[blocks.len() - 1]["children"][0];
    assert_eq!(last["type"], "hyperlink");
    assert_eq!(last["url"], "https://example.com");
    assert_eq!(last["children"][0]["text"], "link");
    assert_eq!(last["children"][0]["underline"], true);
}

#[test]
fn docx_removes_a_field_when_it_is_deleted_from_the_model() {
    let mut doc = json_of(&parse(&fixture("invoice.docx"), &lim()).unwrap());
    doc["parts"][0]["blocks"][1]["children"][1].as_object_mut().unwrap().remove("bold");
    let out = build(&from_json(doc), Some(&fixture("invoice.docx")), &lim()).unwrap();
    let again = json_of(&parse(&out, &lim()).unwrap());
    assert_eq!(again["parts"][0]["blocks"][1]["children"], json!([{ "type": "run", "text": "Client: {{client.name}}" }]));
}

#[test]
fn docx_builds_a_new_document_without_a_base_file() {
    let model = from_json(json!({
        "format": "docx",
        "parts": [{
            "path": "word/document.xml", "kind": "body",
            "blocks": [
                { "type": "paragraph", "style": "Heading1", "children": [{ "type": "run", "text": "Report" }] },
                { "type": "table", "rows": [{ "cells": [{ "blocks": [{ "type": "paragraph", "children": [{ "type": "run", "text": "a", "bold": true }] }] }, { "blocks": [] }] }] }
            ]
        }]
    }));
    let out = build(&model, None, &lim()).unwrap();
    assert_eq!(extract_text(&out, &lim()).unwrap(), "Report\na\t");
    assert!(read_part(&out, "word/styles.xml").contains(r#"w:styleId="Heading1""#));
}

#[test]
fn docx_extracts_plain_text() {
    let text = extract_text(&fixture("invoice.docx"), &lim()).unwrap();
    assert!(text.contains("Invoice {{number}}"));
    assert!(text.contains("Name\tQty\tPrice"));
}

#[test]
fn docx_typed_model_access() {
    let OfficeDocument::Docx(v) = parse(&fixture("invoice.docx"), &lim()).unwrap() else { panic!() };
    let body = v.doc.parts.iter().find(|p| p.kind == PartKind::Body).unwrap();
    let DocxBlock::Paragraph(p) = &body.blocks[1] else { panic!() };
    let DocxInline::Run(r) = &p.children[1] else { panic!() };
    assert_eq!(r.style.bold, Some(true));
}

// ------------------------------------------------------------------------------- xlsx

#[test]
fn xlsx_parses_sheets_typed_cells_formulas_and_merges() {
    let doc = json_of(&parse(&fixture("report.xlsx"), &lim()).unwrap());
    let names: Vec<&SJson> = doc["sheets"].as_array().unwrap().iter().map(|s| &s["name"]).collect();
    assert_eq!(names, vec!["Report", "Info"]);
    let sheet = &doc["sheets"][0];
    assert_eq!(sheet["merges"], json!(["A1:D1"]));
    let row5 = sheet["rows"].as_array().unwrap().iter().find(|r| r["index"] == 5).unwrap();
    let d5 = row5["cells"].as_array().unwrap().iter().find(|c| c["ref"] == "D5").unwrap();
    assert_eq!(d5["formula"], "SUM(D3:D3)");
    let OfficeDocument::Xlsx(x) = parse(&fixture("report.xlsx"), &lim()).unwrap() else { panic!() };
    let arrays = coffee_core::model::xlsx::sheet_to_arrays(&x.doc.sheets[0]);
    assert_eq!(arrays[1], vec![json!("Name"), json!("Qty"), json!("Price"), json!("Sum"), SJson::Null]);
}

#[test]
fn xlsx_round_trips_without_changes() {
    let doc = parse(&fixture("report.xlsx"), &lim()).unwrap();
    let out = build(&from_json(json_of(&doc)), Some(&fixture("report.xlsx")), &lim()).unwrap();
    assert_eq!(json_of(&parse(&out, &lim()).unwrap()), json_of(&doc));
}

#[test]
fn xlsx_edits_values_renames_and_removes_sheets() {
    let mut doc = json_of(&parse(&fixture("report.xlsx"), &lim()).unwrap());
    doc["sheets"][0]["name"] = json!("Отчёт");
    doc["sheets"][0]["rows"].as_array_mut().unwrap().push(json!({ "index": 10, "cells": [{ "ref": "A10", "value": 42 }, { "ref": "B10", "value": true }, { "ref": "C10", "value": "text" }] }));
    doc["sheets"].as_array_mut().unwrap().pop();
    let out = json_of(&parse(&build(&from_json(doc), Some(&fixture("report.xlsx")), &lim()).unwrap(), &lim()).unwrap());
    let names: Vec<&SJson> = out["sheets"].as_array().unwrap().iter().map(|s| &s["name"]).collect();
    assert_eq!(names, vec!["Отчёт"]);
    let row = out["sheets"][0]["rows"].as_array().unwrap().iter().find(|r| r["index"] == 10).unwrap();
    let values: Vec<&SJson> = row["cells"].as_array().unwrap().iter().map(|c| &c["value"]).collect();
    assert_eq!(values, vec![&json!(42), &json!(true), &json!("text")]);
}

#[test]
fn xlsx_creates_a_workbook_from_scratch_with_dates() {
    let model = from_json(json!({
        "format": "xlsx",
        "sheets": [
            { "name": "Data", "rows": [
                { "index": 1, "cells": [{ "ref": "A1", "value": "Name" }, { "ref": "B1", "value": "Born" }, { "ref": "C1", "value": "Score" }] },
                { "index": 2, "cells": [{ "ref": "A2", "value": "Ann" }, { "ref": "B2", "value": "1990-05-17", "type": "date" }, { "ref": "C2", "value": 9.5 }] },
                { "index": 3, "cells": [{ "ref": "A3", "value": "Bob" }, { "ref": "B3", "value": "2001-01-01", "type": "date" }, { "ref": "C3", "value": 7 }] }
            ] },
            { "name": "Empty", "rows": [] }
        ]
    }));
    let out = build(&model, None, &lim()).unwrap();
    let parsed = json_of(&parse(&out, &lim()).unwrap());
    let names: Vec<&SJson> = parsed["sheets"].as_array().unwrap().iter().map(|s| &s["name"]).collect();
    assert_eq!(names, vec!["Data", "Empty"]);
    let born = &parsed["sheets"][0]["rows"][1]["cells"][1];
    assert_eq!(born["type"], "date");
    assert_eq!(born["value"], "1990-05-17");
    assert_eq!(parsed["sheets"][0]["rows"][2]["cells"][2]["value"], 7);
    assert_eq!(extract_text(&out, &lim()).unwrap(), "Data\nName\tBorn\tScore\nAnn\t1990-05-17\t9.5\nBob\t2001-01-01\t7\n\nEmpty\n");
}

// ------------------------------------------------------------------------------- pptx

#[test]
fn pptx_parses_slides_shapes_tables_and_notes() {
    let OfficeDocument::Pptx(v) = parse(&fixture("deck.pptx"), &lim()).unwrap() else { panic!() };
    let doc = v.doc;
    assert_eq!(doc.slides.len(), 2);
    let ph: Vec<Option<&str>> =
        doc.slides[0].shapes.iter().map(|s| if let PptxShape::Text(t) = s { t.placeholder.as_deref() } else { None }).collect();
    assert_eq!(ph, vec![Some("ctrTitle"), Some("subTitle")]);
    let table = doc.slides[1].shapes.iter().find_map(|s| if let PptxShape::Table(t) = s { Some(t) } else { None }).unwrap();
    assert_eq!(table.rows.len(), 2);
    assert_eq!(doc.slides[1].notes.as_deref(), Some("Speaker: {{speaker}}"));
}

#[test]
fn pptx_round_trips_without_changes() {
    let doc = parse(&fixture("deck.pptx"), &lim()).unwrap();
    let out = build(&from_json(json_of(&doc)), Some(&fixture("deck.pptx")), &lim()).unwrap();
    assert_eq!(json_of(&parse(&out, &lim()).unwrap()), json_of(&doc));
}

#[test]
fn pptx_edits_text_table_rows_and_notes_reorders_and_removes_slides() {
    let mut doc = json_of(&parse(&fixture("deck.pptx"), &lim()).unwrap());
    doc["slides"][0]["shapes"][0]["paragraphs"] =
        json!([{ "align": "center", "children": [{ "type": "run", "text": "New title", "bold": true, "size": 40, "color": "#00ff00" }] }]);
    let shapes = doc["slides"][1]["shapes"].as_array_mut().unwrap();
    let table = shapes.iter_mut().find(|s| s["type"] == "table").unwrap();
    table["rows"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "cells": [{ "paragraphs": [{ "children": [{ "type": "run", "text": "r3" }] }] }, { "paragraphs": [] }] }));
    doc["slides"][1]["notes"] = json!("New notes");
    doc["slides"].as_array_mut().unwrap().reverse();

    let out = build(&from_json(doc.clone()), Some(&fixture("deck.pptx")), &lim()).unwrap();
    let again = json_of(&parse(&out, &lim()).unwrap());
    let paths: Vec<&SJson> = again["slides"].as_array().unwrap().iter().map(|s| &s["path"]).collect();
    assert_eq!(paths, vec!["ppt/slides/slide2.xml", "ppt/slides/slide1.xml"]);
    let p = &again["slides"][1]["shapes"][0]["paragraphs"][0];
    assert_eq!(p["align"], "center");
    assert_eq!(p["children"][0]["text"], "New title");
    assert_eq!(p["children"][0]["bold"], true);
    assert_eq!(p["children"][0]["size"], 40);
    assert_eq!(p["children"][0]["color"], "00FF00");
    let tb = again["slides"][0]["shapes"].as_array().unwrap().iter().find(|s| s["type"] == "table").unwrap();
    assert_eq!(tb["rows"].as_array().unwrap().len(), 3);
    assert_eq!(again["slides"][0]["notes"], "New notes");

    doc["slides"].as_array_mut().unwrap().truncate(1);
    let removed = build(&from_json(doc), Some(&fixture("deck.pptx")), &lim()).unwrap();
    let OfficeDocument::Pptx(r) = parse(&removed, &lim()).unwrap() else { panic!() };
    assert_eq!(r.doc.slides.len(), 1);
}

#[test]
fn pptx_requires_a_base_file() {
    let doc = parse(&fixture("deck.pptx"), &lim()).unwrap();
    let e = build(&doc, None, &lim()).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidModel);
    assert!(e.message.contains("requires `base`"));
}

#[test]
fn rejects_unknown_model_versions() {
    let e = OfficeDocument::from_json(json!({ "format": "docx", "modelVersion": 99, "parts": [] })).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidModel);
}

#[test]
fn detects_formats_and_rejects_garbage() {
    assert_eq!(coffee_core::detect(&fixture("deck.pptx"), &lim()).unwrap(), coffee_core::DocumentFormat::Pptx);
    assert_eq!(parse(b"hello", &lim()).unwrap_err().code, ErrorCode::InvalidDocument);
    let not_office = coffee_core::zip::write(vec![("[Content_Types].xml", b"<Types/>".as_slice())]).unwrap();
    assert_eq!(parse(&not_office, &lim()).unwrap_err().code, ErrorCode::UnsupportedFormat);
}
