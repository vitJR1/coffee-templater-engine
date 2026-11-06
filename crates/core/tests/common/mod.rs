#![allow(dead_code)]

use coffee_core::host::{FnHost, NoHost};
use coffee_core::value::parse_data;
use coffee_core::{zip, Errors, Limits, Options, Template, Value};
use std::path::PathBuf;

pub fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/fixtures").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

pub fn read_part(file: &[u8], part: &str) -> String {
    let entries = zip::read(file, &Limits::default()).unwrap();
    let (_, data) = entries.into_iter().find(|(n, _)| n == part).unwrap_or_else(|| panic!("no part {part}"));
    String::from_utf8(data).unwrap()
}

pub fn part_names(file: &[u8]) -> Vec<String> {
    zip::read(file, &Limits::default()).unwrap().into_iter().map(|(n, _)| n).collect()
}

/// Plain text of every <w:t>/<a:t> in a part, paragraphs separated by \n.
pub fn text_of(xml: &str, para: &str, t: &str) -> String {
    let close = format!("</{para}>");
    let mut pieces: Vec<&str> = xml.split(&close).collect();
    pieces.pop();
    let re = regex_lite::Regex::new(&format!(r"<{t}(?:\s[^>]*)?>([^<]*)</{t}>")).unwrap();
    pieces
        .iter()
        .map(|p| re.captures_iter(p).map(|c| c[1].to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

/// Minimal docx with the given body XML.
pub fn docx(body: &str) -> Vec<u8> {
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc = format!(r#"<?xml version="1.0"?><w:document {W}><w:body>{body}</w:body></w:document>"#);
    zip::write(vec![("[Content_Types].xml", ct.as_bytes()), ("_rels/.rels", rels.as_bytes()), ("word/document.xml", doc.as_bytes())])
        .unwrap()
}

/// `p(&["Hello ", "{{na", "me}}"])` → paragraph with one run per argument.
pub fn p(runs: &[&str]) -> String {
    let inner: String = runs.iter().map(|r| format!(r#"<w:r><w:t xml:space="preserve">{r}</w:t></w:r>"#)).collect();
    format!("<w:p>{inner}</w:p>")
}

pub fn body(file: &[u8]) -> String {
    text_of(&read_part(file, "word/document.xml"), "w:p", "w:t")
}

pub fn data(json: &str) -> Value {
    parse_data(json, &[]).unwrap()
}

pub fn opts(json: &str) -> Options {
    Options::from_json(Some(&serde_json::from_str(json).unwrap())).unwrap()
}

pub fn compile(tpl: &[u8], o: Options) -> Result<Template, Errors> {
    Template::compile(tpl, o)
}

/// Compile + render with default options.
pub fn render(tpl: &[u8], json: &str) -> Vec<u8> {
    render_with(tpl, json, Options::default())
}

pub fn render_with(tpl: &[u8], json: &str, o: Options) -> Vec<u8> {
    Template::compile(tpl, o)
        .unwrap_or_else(|e| panic!("compile: {e}"))
        .render(&data(json), &mut NoHost)
        .unwrap_or_else(|e| panic!("render: {e}"))
}

pub fn render_host(tpl: &[u8], json: &str, o: Options, host: &mut FnHost) -> Result<Vec<u8>, coffee_core::Error> {
    Template::compile(tpl, o).map_err(|e| e.0[0].clone())?.render(&data(json), host)
}

pub const PNG_2X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGP4z8DwnwEIGBgAHvAD/c5k+8YAAAAASUVORK5CYII=";
