//! Frame-level tests of the dispatcher and host callbacks.

use coffee_abi::frame::Frame;
use coffee_abi::{request, Instance};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/fixtures").join(name)).unwrap()
}

fn call(inst: &mut Instance, json: Value, blobs: Vec<Vec<u8>>) -> (Value, Vec<Vec<u8>>) {
    let resp = Frame::decode(&inst.call(&request(&json.to_string(), blobs))).unwrap();
    (serde_json::from_str(&resp.json).unwrap(), resp.blobs)
}

fn text(inst: &mut Instance, doc: Vec<u8>) -> String {
    let (r, _) = call(inst, json!({"op": "extractText", "document": {"$blob": 0}}), vec![doc]);
    r["result"]["text"].as_str().unwrap().to_string()
}

#[test]
fn info_reports_versions() {
    let mut inst = Instance::default();
    let (r, _) = call(&mut inst, json!({"op": "info", "id": 7}), vec![]);
    assert_eq!(r["ok"], true);
    assert_eq!(r["id"], 7);
    assert_eq!(r["result"]["abiVersion"], 1);
    assert_eq!(r["result"]["modelVersion"], 1);
    assert_eq!(r["result"]["formats"], json!(["docx", "xlsx", "pptx"]));
    assert_eq!(r["result"]["features"]["images"], json!(["docx", "pptx"]));
    assert_eq!(r["warnings"], json!([]));
}

#[test]
fn response_keys_are_in_fixed_order() {
    let mut inst = Instance::default();
    let resp = Frame::decode(&inst.call(&request(r#"{"op":"dispose","template":1,"id":"x"}"#, vec![]))).unwrap();
    assert_eq!(resp.json, r#"{"ok":true,"id":"x","result":{},"warnings":[]}"#);
}

#[test]
fn compile_render_inspect_dispose() {
    let mut inst = Instance::default();
    let (r, _) = call(&mut inst, json!({"op": "compile", "template": {"$blob": 0}}), vec![fixture("invoice.docx")]);
    assert_eq!(r["ok"], true, "{r}");
    let handle = r["result"]["template"].clone();
    assert_eq!(r["result"]["format"], "docx");
    assert_eq!(r["result"]["parts"][0], "word/document.xml");

    let png = coffee_core::value::decode_base64(
        "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGP4z8DwnwEIGBgAHvAD/c5k+8YAAAAASUVORK5CYII=",
    )
    .unwrap();
    let data = json!({"number": 42, "client": {"name": "Ann"}, "items": [], "logo": {"data": {"$blob": 0}, "width": 50}});
    let (r, blobs) = call(&mut inst, json!({"op": "render", "template": handle, "data": data}), vec![png]);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["document"], json!({"$blob": 0}));
    let doc = blobs.into_iter().next().unwrap();
    assert!(text(&mut inst, doc.clone()).contains("Invoice 42"));
    let entries = coffee_core::zip::read(&doc, &Default::default()).unwrap();
    assert!(entries.iter().any(|(n, _)| n == "word/media/image_ct1.png"));

    let (r, _) = call(&mut inst, json!({"op": "inspect", "template": handle}), vec![]);
    assert!(r["result"]["variables"].as_array().unwrap().contains(&json!("client.name")));

    let (r, _) = call(&mut inst, json!({"op": "dispose", "template": handle}), vec![]);
    assert_eq!(r["ok"], true);
    assert_eq!(inst.template_count(), 0);
    let (r, _) = call(&mut inst, json!({"op": "dispose", "template": handle}), vec![]);
    assert_eq!(r["ok"], true, "double dispose is not an error");
    let (r, _) = call(&mut inst, json!({"op": "render", "template": handle}), vec![]);
    assert_eq!(r["errors"][0]["code"], "unknown_template");
}

#[test]
fn compile_errors_are_listed() {
    let mut inst = Instance::default();
    let (r, _) =
        call(&mut inst, json!({"op": "compile", "template": {"$blob": 0}, "options": {"hostFilters": []}}), vec![fixture("broken.docx")]);
    assert_eq!(r["ok"], false);
    let codes: Vec<&str> = r["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap()).collect();
    assert!(codes.len() >= 2, "{r}");
    assert!(codes.contains(&"unclosed_section"));
    assert!(codes.contains(&"unknown_filter"));
    assert_eq!(r["errors"][0]["part"], "word/document.xml");
}

#[test]
fn protocol_errors() {
    let mut inst = Instance::default();
    let resp = Frame::decode(&inst.call(b"garbage")).unwrap();
    assert!(resp.json.contains("invalid_request"));
    let (r, _) = call(&mut inst, json!({"op": "nope"}), vec![]);
    assert_eq!(r["errors"][0]["code"], "invalid_request");
    let (r, _) = call(&mut inst, json!({"op": "parse", "document": {"$blob": 3}}), vec![]);
    assert_eq!(r["errors"][0]["code"], "invalid_request");
    let (r, _) = call(&mut inst, json!({"op": "parse", "document": {"$blob": 0}}), vec![b"not a zip".to_vec()]);
    assert_eq!(r["errors"][0]["code"], "invalid_document");
    let (r, _) = call(
        &mut inst,
        json!({"op": "compile", "template": {"$blob": 0}, "options": {"delimiters": ["{{", "{{"]}}),
        vec![fixture("invoice.docx")],
    );
    assert_eq!(r["errors"][0]["code"], "invalid_options");
    let (r, _) =
        call(&mut inst, json!({"op": "parse", "document": {"$blob": 0}, "limits": {"maxEntries": 2}}), vec![fixture("invoice.docx")]);
    assert_eq!(r["errors"][0]["code"], "limit_exceeded");
    assert_eq!(r["errors"][0]["details"]["limit"], "maxEntries");
}

#[test]
fn parse_build_round_trip_over_frames() {
    let mut inst = Instance::default();
    let (r, _) = call(&mut inst, json!({"op": "parse", "document": {"$blob": 0}}), vec![fixture("report.xlsx")]);
    let mut model = r["result"]["model"].clone();
    assert_eq!(model["format"], "xlsx");
    model["sheets"][1]["name"] = json!("Renamed");
    let (r, blobs) = call(&mut inst, json!({"op": "build", "model": model, "base": {"$blob": 0}}), vec![fixture("report.xlsx")]);
    assert_eq!(r["ok"], true, "{r}");
    let (r, _) = call(&mut inst, json!({"op": "detect", "document": {"$blob": 0}}), blobs.clone());
    assert_eq!(r["result"]["format"], "xlsx");
    assert!(text(&mut inst, blobs[0].clone()).contains("Renamed\nAuthor"));
}

/// A host that answers callbacks like a wrapper would, recording the requests.
type Log = Rc<RefCell<Vec<(Value, Vec<Vec<u8>>, String)>>>;

fn recording_host(log: Log) -> impl FnMut(&[u8]) -> Option<Vec<u8>> {
    move |req: &[u8]| {
        let f = Frame::decode(req).unwrap();
        let body: Value = serde_json::from_str(&f.json).unwrap();
        log.borrow_mut().push((body.clone(), f.blobs.clone(), f.json.clone()));
        let answer = match body["call"].as_str().unwrap() {
            "filter" => match body["name"].as_str().unwrap() {
                "money" => json!({"ok": true, "value": format!("{} {}", body["value"], body["args"][0].as_str().unwrap())}),
                "size" => json!({"ok": true, "value": f.blobs.first().map(|b| b.len()).unwrap_or(0)}),
                "items" => json!({"ok": true, "value": [{"$date": "2024-05-17"}, {"$blob": 0}]}),
                _ => json!({"ok": false, "message": "unsupported currency"}),
            },
            "nullValue" => json!({"ok": true, "value": format!("<{}>", body["tag"]["expression"].as_str().unwrap())}),
            "stringify" => json!({"ok": true, "value": format!("S:{}", body["value"])}),
            _ => unreachable!(),
        };
        Some(Frame::new(answer.to_string(), vec![b"xyz".to_vec()]).encode())
    }
}

fn docx_with(text: &str) -> Vec<u8> {
    let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc = format!(
        r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    );
    coffee_core::zip::write(vec![
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", doc.as_bytes()),
    ])
    .unwrap()
}

#[test]
fn host_callbacks_over_frames() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut inst = Instance::new(Box::new(recording_host(log.clone())));
    let tpl = docx_with(r#"{{price | money: "RUB"}}|{{file | size}}|{{missing}}|{{#each x | items}}[{{.}}]{{/each}}|{{n}}|{{p | fail}}"#);
    let options = json!({"hostFilters": ["money", "size", "items"], "nullValue": {"mode": "host"}, "stringify": {"mode": "host"}});
    let data = json!({"price": 12345678901234567890u64, "file": {"$blob": 1}, "n": 7}); // blob 0 is the template
    let (r, _) = call(
        &mut inst,
        json!({"op": "renderOnce", "template": {"$blob": 0}, "options": options, "data": data}),
        vec![tpl.clone(), b"12345".to_vec()],
    );
    assert_eq!(r["errors"][0]["code"], "unknown_filter", "fail is not declared: {r}");

    let options = json!({"hostFilters": ["money", "size", "items", "fail"], "nullValue": {"mode": "host"}, "stringify": {"mode": "host"}});
    let tpl_ok = docx_with(r#"{{price | money: "RUB"}}|{{file | size}}|{{missing}}|{{#each x | items}}[{{.}}]{{/each}}|{{n}}"#);
    let (r, blobs) = call(
        &mut inst,
        json!({"op": "renderOnce", "template": {"$blob": 0}, "options": options, "data": data}),
        vec![tpl_ok, b"12345".to_vec()],
    );
    assert_eq!(r["ok"], true, "{r}");
    let out = text(&mut inst, blobs[0].clone());
    // size gets the blob; the host's blob comes back as bytes
    assert_eq!(out, "12345678901234567890 RUB|S:5|<missing>|[S:{\"$date\":\"2024-05-17\"}][S:{\"$blob\":0}]|S:7");
    let calls = log.borrow();
    // the number lexeme reaches the host unchanged (an f64 would print 12345678901234567000)
    assert!(calls[0].2.contains(r#""value":12345678901234567890,"args":["RUB"]"#), "{}", calls[0].2);
    let size_call = calls.iter().find(|(b, _, _)| b["name"] == "size").unwrap();
    assert_eq!(size_call.0["value"], json!({"$blob": 0}));
    assert_eq!(size_call.1[0], b"12345");
    assert_eq!(calls[0].0["tag"]["raw"], "{{price | money: \"RUB\"}}");
    assert_eq!(calls[0].0["tag"]["part"], "word/document.xml");
    drop(calls);

    let (r, _) =
        call(&mut inst, json!({"op": "renderOnce", "template": {"$blob": 0}, "options": options, "data": data}), vec![tpl.clone()]);
    assert_eq!(r["errors"][0]["code"], "invalid_request", "data refers to a blob that is not in the frame");
    let (r, _) = call(
        &mut inst,
        json!({"op": "renderOnce", "template": {"$blob": 0}, "options": options, "data": data}),
        vec![tpl, b"12345".to_vec()],
    );
    assert_eq!(r["errors"][0]["code"], "filter_failed");
    assert_eq!(r["errors"][0]["details"]["hostMessage"], "unsupported currency");
}

#[test]
fn host_without_callbacks_reports_host_error() {
    let mut inst = Instance::default();
    let tpl = docx_with("{{x | money}}");
    let (r, _) = call(&mut inst, json!({"op": "renderOnce", "template": {"$blob": 0}, "options": {"hostFilters": ["money"]}}), vec![tpl]);
    assert_eq!(r["errors"][0]["code"], "host_error");
}

#[test]
fn host_call_limit() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut inst = Instance::new(Box::new(recording_host(log)));
    let tpl = docx_with("{{#items}}{{. | money: \"X\"}}{{/items}}");
    let (r, _) = call(
        &mut inst,
        json!({"op": "renderOnce", "template": {"$blob": 0}, "options": {"hostFilters": ["money"], "limits": {"maxHostCalls": 2}}, "data": {"items": [1, 2, 3]}}),
        vec![tpl],
    );
    assert_eq!(r["errors"][0]["code"], "limit_exceeded");
    assert_eq!(r["errors"][0]["details"]["limit"], "maxHostCalls");
}
