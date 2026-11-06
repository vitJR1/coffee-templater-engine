//! Port of `test/xlsx-pptx-template.test.ts` from the TS implementation.

mod common;
use coffee_core::model::pptx::{PptxInline, PptxShape};
use coffee_core::model::xlsx::XlsxDocument;
use coffee_core::{parse, Limits, OfficeDocument, Options};
use common::*;
use serde_json::{json, Value as SJson};
use std::collections::HashMap;

const ITEMS: &str = r#"[{"name":"Coffee","qty":2,"price":10.5},{"name":"Tea","qty":1,"price":4},{"name":"Milk","qty":3,"price":1.25}]"#;

fn xlsx_model(file: &[u8]) -> XlsxDocument {
    match parse(file, &Limits::default()).unwrap() {
        OfficeDocument::Xlsx(v) => v.doc,
        _ => panic!("not xlsx"),
    }
}

fn cells(doc: &XlsxDocument, sheet: usize) -> HashMap<String, SJson> {
    let mut out = HashMap::new();
    for r in &doc.sheets[sheet].rows {
        for c in &r.cells {
            let v = match &c.formula {
                Some(f) => SJson::String(format!("={f}")),
                None => c.value.clone(),
            };
            out.insert(c.reference.clone().unwrap_or_default(), v);
        }
    }
    out
}

#[test]
fn xlsx_repeats_rows_keeps_types_and_shifts_rows_below() {
    let out = render(&fixture("report.xlsx"), &format!(r#"{{"title":"Q1","items":{ITEMS},"author":"me"}}"#));
    let c = cells(&xlsx_model(&out), 0);
    assert_eq!(c["A1"], "Report: Q1");
    assert_eq!([&c["A3"], &c["B3"], &c["C3"]], [&json!("Coffee"), &json!(2), &json!(10.5)]);
    assert_eq!([&c["A5"], &c["B5"], &c["C5"]], [&json!("Milk"), &json!(3), &json!(1.25)]);
    assert_eq!(c["D4"], "=B4*C4");
    assert_eq!(c["A7"], "Total");
    assert_eq!(c["D7"], "=SUM(D3:D5)");
    assert_eq!(c["A8"], "Escaped {{x}}");
}

#[test]
fn xlsx_removes_loop_rows_for_empty_arrays_and_shifts_rows_up() {
    let out = render(&fixture("report.xlsx"), r#"{"title":"Q1","items":[],"author":"me"}"#);
    let c = cells(&xlsx_model(&out), 0);
    assert_eq!(c["A2"], "Name");
    assert!(!c.contains_key("A3"));
    assert_eq!(c["A4"], "Total");
}

#[test]
fn xlsx_keeps_merged_cells_and_renders_other_sheets() {
    let out = render(&fixture("report.xlsx"), &format!(r#"{{"title":"Q1","items":{ITEMS},"author":"me"}}"#));
    let doc = xlsx_model(&out);
    assert_eq!(doc.sheets[0].merges.as_deref(), Some(&["A1:D1".to_string()][..]));
    assert_eq!(cells(&doc, 1)["B1"], "me");
}

#[test]
fn xlsx_drops_calc_chain_and_forces_recalculation() {
    let out = render(&fixture("report.xlsx"), &format!(r#"{{"title":"Q1","items":{ITEMS},"author":"me"}}"#));
    assert!(read_part(&out, "xl/workbook.xml").contains(r#"fullCalcOnLoad="1""#));
    assert!(!part_names(&out).iter().any(|n| n == "xl/calcChain.xml"));
}

#[test]
fn xlsx_keeps_numbers_as_text_when_mixed_with_other_text() {
    let out = render(&fixture("report.xlsx"), r#"{"title":2024,"items":[],"author":true}"#);
    let doc = xlsx_model(&out);
    assert_eq!(cells(&doc, 0)["A1"], "Report: 2024");
    assert_eq!(cells(&doc, 1)["B1"], true);
}

#[test]
fn xlsx_writes_decimals_as_numbers() {
    let out = render(&fixture("report.xlsx"), r#"{"title":"Q1","items":[{"name":"x","qty":{"$decimal":"2.50"},"price":1}],"author":"me"}"#);
    assert_eq!(cells(&xlsx_model(&out), 0)["B3"], json!(2.5));
}

#[test]
fn xlsx_inspects_variables() {
    let info = compile(&fixture("report.xlsx"), Options::default()).unwrap().inspect();
    assert_eq!(info.variables, vec!["title", "items[].name", "items[].qty", "items[].price", "author"]);
}

#[test]
fn xlsx_values_are_data_not_template_syntax() {
    let tpl = fixture("report.xlsx");
    let out = render(&tpl, r#"{"title":"{{%logo}}","items":[],"author":"x"}"#);
    assert_eq!(cells(&xlsx_model(&out), 0)["A1"], "Report: {{%logo}}");
}

// ------------------------------------------------------------------------------- pptx

fn deck_data(logo: &str) -> String {
    format!(
        r#"{{"title":"Hello","subtitle":"line 1\nline 2","points":["a","b","c"],"logo":{logo},"rows":[{{"name":"x","value":1}},{{"name":"y","value":2}}],"speaker":"Bob"}}"#
    )
}

fn shape_text(sh: &PptxShape) -> String {
    match sh {
        PptxShape::Text(t) => t
            .paragraphs
            .iter()
            .map(|p| p.children.iter().map(|c| if let PptxInline::Run(r) = c { r.text.as_str() } else { "" }).collect::<String>())
            .collect::<Vec<_>>()
            .join("|"),
        _ => String::new(),
    }
}

#[test]
fn pptx_renders_text_paragraph_loops_tables_and_notes() {
    let out = render(&fixture("deck.pptx"), &deck_data(&format!(r#"{{"data":{{"$base64":"{PNG_2X1}"}}}}"#)));
    let OfficeDocument::Pptx(doc) = parse(&out, &Limits::default()).unwrap() else { panic!() };
    let (s1, s2) = (&doc.doc.slides[0], &doc.doc.slides[1]);
    assert_eq!(shape_text(&s1.shapes[0]), "Hello");
    assert_eq!(shape_text(&s1.shapes[1]), "line 1\nline 2");
    assert_eq!(shape_text(&s2.shapes[0]), "• a|• b|• c");
    let table = s2.shapes.iter().find_map(|s| if let PptxShape::Table(t) = s { Some(t) } else { None }).unwrap();
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    c.paragraphs[0]
                        .children
                        .iter()
                        .map(|x| if let PptxInline::Run(r) = x { r.text.clone() } else { String::new() })
                        .collect()
                })
                .collect()
        })
        .collect();
    assert_eq!(rows, vec![vec!["Name", "Value"], vec!["x", "1"], vec!["y", "2"]]);
    assert_eq!(s2.notes.as_deref(), Some("Speaker: Bob"));
}

#[test]
fn pptx_uses_a_br_with_run_properties_for_line_breaks() {
    let xml = read_part(&render(&fixture("deck.pptx"), &deck_data("null")), "ppt/slides/slide1.xml");
    assert!(regex_lite::Regex::new(r"line 1</a:t></a:r><a:br\b").unwrap().is_match(&xml), "{xml}");
}

#[test]
fn pptx_replaces_the_image_text_box_with_a_picture_at_the_same_place() {
    let out = render(&fixture("deck.pptx"), &deck_data(&format!(r#"{{"data":{{"$base64":"{PNG_2X1}"}}}}"#)));
    let xml = read_part(&out, "ppt/slides/slide2.xml");
    assert!(xml.contains("<p:pic>"));
    assert!(!xml.contains("{{%logo}}"));
    // text box is 3in x 2in at (6in, 1in); a 2:1 image fits as 3in x 1.5in, vertically centered
    let inch = 914400;
    assert!(
        xml.contains(&format!(r#"<a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/>"#, 6 * inch, inch + inch / 4, 3 * inch, 3 * inch / 2)),
        "{xml}"
    );
    assert!(part_names(&out).iter().any(|n| n.starts_with("ppt/media/image_ct")));
}

#[test]
fn pptx_removes_the_image_shape_when_the_value_is_empty() {
    let xml = read_part(&render(&fixture("deck.pptx"), &deck_data("null")), "ppt/slides/slide2.xml");
    assert!(!xml.contains("<p:pic>"));
    assert!(!xml.contains("{{%logo}}"));
}
