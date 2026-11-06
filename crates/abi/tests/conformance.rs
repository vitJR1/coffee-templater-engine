//! Runs the language-independent conformance cases (`conformance/cases/*/case.json`, spec §11)
//! through the ABI dispatcher, exactly as a wrapper would.
//!
//! `CONFORMANCE_PRINT=1 cargo test -p coffee-abi --test conformance -- --nocapture` prints the
//! actual sha256 of every rendered document; `CONFORMANCE_DUMP=<dir>` writes the documents there.

use coffee_abi::frame::Frame;
use coffee_abi::{request, Instance};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

fn call(inst: &mut Instance, req: Value, blobs: Vec<Vec<u8>>) -> (Value, Vec<Vec<u8>>) {
    let resp = Frame::decode(&inst.call(&request(&req.to_string(), blobs))).expect("response frame");
    (serde_json::from_str(&resp.json).expect("response JSON"), resp.blobs)
}

/// `String(v)` for a JSON value.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(coffee_core::num::js_to_string).unwrap_or_else(|| n.to_string()),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

/// Declarative host filters: `format` (`{value}`, `{argN}`), `upper`, `throw`.
fn host(filters: Value) -> impl FnMut(&[u8]) -> Option<Vec<u8>> {
    move |req: &[u8]| {
        let f = Frame::decode(req).ok()?;
        let body: Value = serde_json::from_str(&f.json).ok()?;
        let spec = &filters[body["name"].as_str().unwrap_or("")];
        let answer = match spec["type"].as_str() {
            Some("format") => {
                let mut s = spec["pattern"].as_str().unwrap_or("").replace("{value}", &js_string(&body["value"]));
                for (i, a) in body["args"].as_array().into_iter().flatten().enumerate() {
                    s = s.replace(&format!("{{arg{i}}}"), &js_string(a));
                }
                json!({"ok": true, "value": s})
            }
            Some("upper") => json!({"ok": true, "value": js_string(&body["value"]).to_uppercase()}),
            Some("throw") => json!({"ok": false, "message": spec["message"]}),
            _ => json!({"ok": false, "message": "unknown host filter"}),
        };
        Some(Frame::new(answer.to_string(), vec![]).encode())
    }
}

/// Resolves `a.b[0].c` in a JSON value.
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        let (name, rest) = seg.split_once('[').map(|(n, r)| (n, Some(r))).unwrap_or((seg, None));
        if !name.is_empty() {
            cur = cur.get(name)?;
        }
        if let Some(r) = rest {
            for idx in r.split('[') {
                cur = cur.get(idx.trim_end_matches(']').parse::<usize>().ok()?)?;
            }
        }
    }
    Some(cur)
}

/// `expected` is a subset of `actual` (objects: listed keys only; arrays: same length).
fn matches(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e.iter().all(|(k, ev)| a.get(k).is_some_and(|av| matches(av, ev))),
        (Value::Array(a), Value::Array(e)) => a.len() == e.len() && a.iter().zip(e).all(|(x, y)| matches(x, y)),
        (Value::Number(a), Value::Number(e)) => a.as_f64() == e.as_f64(),
        (a, e) => a == e,
    }
}

fn check_paths(name: &str, what: &str, root: &Value, checks: &Value) {
    let list = match checks {
        Value::Array(a) => a.clone(),
        v => vec![v.clone()],
    };
    for c in list {
        let path = c["path"].as_str().unwrap_or("");
        let actual = at(root, path).unwrap_or_else(|| panic!("[{name}] {what}: no value at `{path}`"));
        assert!(matches(actual, &c["equals"]), "[{name}] {what} `{path}`:\n  expected {}\n  actual   {}", c["equals"], actual);
    }
}

fn read_blobs(dir: &Path) -> Vec<Vec<u8>> {
    let Ok(rd) = std::fs::read_dir(dir.join("blobs")) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    files.iter().map(|p| std::fs::read(p).unwrap()).collect()
}

fn run_case(dir: &Path) {
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    let case: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("case.json")).unwrap()).unwrap();
    let expect = &case["expect"];
    let host_filters = case.get("hostFilters").cloned().unwrap_or(json!({}));
    let mut inst = Instance::new(Box::new(host(host_filters.clone())));
    let template = case["template"].as_str().map(|t| std::fs::read(root().join(t)).unwrap());
    let data = match std::fs::read_to_string(dir.join("data.json")) {
        Ok(t) => serde_json::from_str(&t).unwrap(),
        Err(_) => case.get("data").cloned().unwrap_or(json!({})),
    };
    let mut options = case.get("options").cloned().unwrap_or(json!({}));
    if let Some(m) = host_filters.as_object() {
        options["hostFilters"] = json!(m.keys().collect::<Vec<_>>());
    }
    let limits = case.get("limits").cloned();
    let op = case["op"].as_str().unwrap();

    // compile first for template operations, then run the operation itself
    let (resp, blobs) = match op {
        "info" => call(&mut inst, json!({"op": "info"}), vec![]),
        "compile" | "render" | "inspect" => {
            let (c, _) =
                call(&mut inst, json!({"op": "compile", "template": {"$blob": 0}, "options": options}), vec![template.clone().unwrap()]);
            if op == "compile" || c["ok"] != true {
                (c, vec![])
            } else {
                let handle = c["result"]["template"].clone();
                let r = if op == "render" {
                    call(&mut inst, json!({"op": "render", "template": handle, "data": data}), read_blobs(dir))
                } else {
                    call(&mut inst, json!({"op": "inspect", "template": handle}), vec![])
                };
                call(&mut inst, json!({"op": "dispose", "template": handle}), vec![]);
                r
            }
        }
        "parse" | "extractText" | "detect" => {
            let mut req = json!({"op": op, "document": {"$blob": 0}});
            if let Some(l) = limits {
                req["limits"] = l;
            }
            call(&mut inst, req, vec![template.clone().unwrap()])
        }
        other => panic!("[{name}] unsupported op {other}"),
    };

    if let Some(errors) = expect.get("errors") {
        assert_eq!(resp["ok"], false, "[{name}] expected errors, got {resp}");
        let actual = resp["errors"].as_array().unwrap();
        let wanted = errors.as_array().unwrap();
        assert!(actual.len() >= wanted.len(), "[{name}] expected {} errors, got {resp}", wanted.len());
        for (a, e) in actual.iter().zip(wanted) {
            assert!(matches(a, e), "[{name}] error:\n  expected {e}\n  actual   {a}");
        }
        return;
    }
    assert_eq!(resp["ok"], true, "[{name}] {resp}");
    if let Some(r) = expect.get("result") {
        check_paths(&name, "result", &resp["result"], r);
    }
    if op == "extractText" {
        if let Some(t) = expect.get("text") {
            assert_eq!(resp["result"]["text"], *t, "[{name}] text");
        }
        return;
    }
    if op == "parse" {
        if let Some(m) = expect.get("model") {
            check_paths(&name, "model", &resp["result"]["model"], m);
        }
        return;
    }
    if op != "render" {
        return;
    }
    let doc = blobs.into_iter().next().unwrap_or_else(|| panic!("[{name}] no document blob"));

    let text = || {
        let (r, _) = call(&mut Instance::default(), json!({"op": "extractText", "document": {"$blob": 0}}), vec![doc.clone()]);
        r["result"]["text"].as_str().unwrap().to_string()
    };
    if let Some(t) = expect.get("text") {
        assert_eq!(text(), t.as_str().unwrap(), "[{name}] text");
    }
    if let Some(list) = expect.get("textContains") {
        let t = text();
        for s in list.as_array().unwrap() {
            assert!(t.contains(s.as_str().unwrap()), "[{name}] text does not contain {s:?}:\n{t}");
        }
    }
    if let Some(m) = expect.get("model") {
        let (r, _) = call(&mut Instance::default(), json!({"op": "parse", "document": {"$blob": 0}}), vec![doc.clone()]);
        check_paths(&name, "model", &r["result"]["model"], m);
    }
    let entries = coffee_core::zip::read(&doc, &Default::default()).unwrap();
    if let Some(parts) = expect.get("parts").and_then(Value::as_object) {
        for (part, checks) in parts {
            let xml = entries
                .iter()
                .find(|(n, _)| n == part)
                .map(|(_, d)| String::from_utf8_lossy(d).to_string())
                .unwrap_or_else(|| panic!("[{name}] no part {part}"));
            for s in checks["contains"].as_array().into_iter().flatten() {
                assert!(xml.contains(s.as_str().unwrap()), "[{name}] {part} does not contain {s}");
            }
            for s in checks["notContains"].as_array().into_iter().flatten() {
                assert!(!xml.contains(s.as_str().unwrap()), "[{name}] {part} contains {s}");
            }
        }
    }
    if let Some(files) = expect.get("files") {
        for s in files["contains"].as_array().into_iter().flatten() {
            assert!(entries.iter().any(|(n, _)| n == s.as_str().unwrap()), "[{name}] missing file {s}");
        }
        for s in files["notContains"].as_array().into_iter().flatten() {
            assert!(!entries.iter().any(|(n, _)| n == s.as_str().unwrap()), "[{name}] unexpected file {s}");
        }
    }
    if let Ok(dir) = std::env::var("CONFORMANCE_DUMP") {
        std::fs::write(Path::new(&dir).join(format!("{name}.bin")), &doc).unwrap();
    }
    let sha = format!("{:x}", Sha256::digest(&doc));
    if std::env::var("CONFORMANCE_PRINT").is_ok() {
        println!("{name}: sha256 {sha}");
    }
    if let Some(want) = expect.get("sha256").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        assert_eq!(sha, want, "[{name}] sha256");
    }
}

#[test]
fn conformance_cases() {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root().join("cases"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("case.json").exists())
        .collect();
    dirs.sort();
    assert!(dirs.len() >= 10);
    for d in &dirs {
        run_case(d);
    }
}
