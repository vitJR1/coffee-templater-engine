//! # coffee-abi
//!
//! The boundary between coffee-core and language wrappers (spec §4–§9): message frames, the
//! operation dispatcher and host callbacks. Both the WebAssembly module and the C ABI are thin
//! shells around [`Instance::call`].

pub mod frame;

use coffee_core::host::{Host, HostError, TagInfo};
use coffee_core::json::{self, Json};
use coffee_core::template::options::{parse_limits, Options};
use coffee_core::{DocumentFormat, Error, ErrorCode, Errors, Limits, OfficeDocument, Template, Value};
use frame::Frame;
use serde_json::{json, Map, Value as SJson};
use std::collections::BTreeMap;
use std::rc::Rc;

/// Version of exports/imports and the frame format.
pub const ABI_VERSION: u32 = 1;

/// Transport for host callbacks: sends a request frame, returns the response frame.
/// `None` means "the host does not support callbacks" (the import returned 0).
pub trait HostTransport {
    fn call(&mut self, request: &[u8]) -> Option<Vec<u8>>;
}

/// A transport without callbacks.
pub struct NoTransport;

impl HostTransport for NoTransport {
    fn call(&mut self, _: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

impl<F: FnMut(&[u8]) -> Option<Vec<u8>>> HostTransport for F {
    fn call(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        self(request)
    }
}

/// One core instance: owns compiled templates and the host transport. Single-threaded.
pub struct Instance {
    templates: BTreeMap<u32, Template>,
    next_handle: u32,
    transport: Box<dyn HostTransport>,
}

impl Default for Instance {
    fn default() -> Self {
        Instance::new(Box::new(NoTransport))
    }
}

type OpResult = Result<(SJson, Vec<Vec<u8>>), Errors>;

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidRequest, msg)
}

impl Instance {
    pub fn new(transport: Box<dyn HostTransport>) -> Self {
        Instance { templates: BTreeMap::new(), next_handle: 1, transport }
    }

    /// Number of live templates.
    pub fn template_count(&self) -> usize {
        self.templates.len()
    }

    /// Executes a request frame and returns a response frame. Never panics outward (native builds);
    /// a panic becomes an `internal` error.
    pub fn call(&mut self, request: &[u8]) -> Vec<u8> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.dispatch(request)));
        match result {
            Ok(f) => f.encode(),
            Err(p) => {
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".into());
                error_frame(None, &Errors::from(Error::new(ErrorCode::Internal, format!("Internal error: {msg}")))).encode()
            }
        }
    }

    fn dispatch(&mut self, request: &[u8]) -> Frame {
        let frame = match Frame::decode(request) {
            Ok(f) => f,
            Err(e) => return error_frame(None, &Errors::from(invalid(e.0))),
        };
        // A `build` model may nest as deep as the XML it describes; everything else is limited to
        // json::MAX_DEPTH. The parser takes the larger limit, the fields are checked right after.
        let req = match json::parse_with_depth(&frame.json, json::MAX_MODEL_DEPTH) {
            Ok(j @ Json::Obj(_)) => j,
            Ok(_) => return error_frame(None, &Errors::from(invalid("Request must be a JSON object"))),
            Err(e) => return error_frame(None, &Errors::from(e)),
        };
        if let Err(e) = check_depth(&req) {
            return error_frame(None, &Errors::from(e));
        }
        let id = req.get("id").map(Json::to_serde);
        let blobs: Vec<Rc<[u8]>> = frame.blobs.into_iter().map(Rc::from).collect();
        let result = match req.get("op").and_then(Json::as_str) {
            None => Err(Errors::from(invalid("Missing \"op\""))),
            Some(op) => self.op(op, &req, &blobs),
        };
        match result {
            Ok((value, out_blobs)) => {
                let mut m = Map::new();
                m.insert("ok".into(), SJson::Bool(true));
                if let Some(id) = id {
                    m.insert("id".into(), id);
                }
                m.insert("result".into(), value);
                m.insert("warnings".into(), SJson::Array(Vec::new()));
                Frame::new(serde_json::to_string(&SJson::Object(m)).unwrap_or_default(), out_blobs)
            }
            Err(e) => error_frame(id, &e),
        }
    }

    fn op(&mut self, op: &str, req: &Json, blobs: &[Rc<[u8]>]) -> OpResult {
        match op {
            "info" => Ok((info(), Vec::new())),
            "compile" => {
                let tpl = compile(req, blobs)?;
                let handle = self.next_handle;
                self.next_handle = self.next_handle.checked_add(1).unwrap_or(1);
                let res = json!({ "template": handle, "format": tpl.format, "parts": tpl.parts() });
                self.templates.insert(handle, tpl);
                Ok((res, Vec::new()))
            }
            "render" => {
                let handle = handle(req)?;
                let tpl = self.templates.get(&handle).ok_or_else(|| unknown_template(handle))?;
                let data = data(req, blobs)?;
                let mut host = FrameHost { transport: self.transport.as_mut() };
                let doc = tpl.render(&data, &mut host)?;
                Ok((json!({ "document": { "$blob": 0 } }), vec![doc]))
            }
            "renderOnce" => {
                let tpl = compile(req, blobs)?;
                let data = data(req, blobs)?;
                let mut host = FrameHost { transport: self.transport.as_mut() };
                let doc = tpl.render(&data, &mut host)?;
                Ok((json!({ "document": { "$blob": 0 } }), vec![doc]))
            }
            "inspect" => {
                let handle = handle(req)?;
                let tpl = self.templates.get(&handle).ok_or_else(|| unknown_template(handle))?;
                Ok((tpl.inspect().to_json(), Vec::new()))
            }
            "dispose" => {
                if let Ok(h) = handle(req) {
                    self.templates.remove(&h);
                }
                Ok((json!({}), Vec::new()))
            }
            "parse" => {
                let limits = limits(req)?;
                let doc = blob(req, "document", blobs)?.ok_or_else(|| invalid("Missing \"document\""))?;
                let model = coffee_core::parse(&doc, &limits)?;
                Ok((json!({ "model": model.to_json() }), Vec::new()))
            }
            "build" => {
                let limits = limits(req)?;
                let model = req.get("model").ok_or_else(|| invalid("Missing \"model\""))?;
                let model = OfficeDocument::from_json(model.to_serde())?;
                let base = blob(req, "base", blobs)?;
                let doc = coffee_core::build(&model, base.as_deref(), &limits)?;
                Ok((json!({ "document": { "$blob": 0 } }), vec![doc]))
            }
            "extractText" => {
                let limits = limits(req)?;
                let doc = blob(req, "document", blobs)?.ok_or_else(|| invalid("Missing \"document\""))?;
                Ok((json!({ "text": coffee_core::extract_text(&doc, &limits)? }), Vec::new()))
            }
            "detect" => {
                let limits = limits(req)?;
                let doc = blob(req, "document", blobs)?.ok_or_else(|| invalid("Missing \"document\""))?;
                Ok((json!({ "format": coffee_core::detect(&doc, &limits)? }), Vec::new()))
            }
            other => Err(Errors::from(invalid(format!("Unknown operation \"{other}\"")))),
        }
    }
}

fn info() -> SJson {
    json!({
        "coreVersion": coffee_core::CORE_VERSION,
        "abiVersion": ABI_VERSION,
        "modelVersion": coffee_core::MODEL_VERSION,
        "formats": DocumentFormat::ALL,
        "features": {
            "images": DocumentFormat::ALL.iter().filter(|f| f.supports_images()).collect::<Vec<_>>(),
            "slideLoops": false,
            "hostFilters": true,
        },
    })
}

fn error_frame(id: Option<SJson>, errors: &Errors) -> Frame {
    let mut m = Map::new();
    m.insert("ok".into(), SJson::Bool(false));
    if let Some(id) = id {
        m.insert("id".into(), id);
    }
    m.insert("errors".into(), serde_json::to_value(&errors.0).unwrap_or(SJson::Array(Vec::new())));
    Frame::new(serde_json::to_string(&SJson::Object(m)).unwrap_or_default(), Vec::new())
}

fn unknown_template(h: u32) -> Error {
    Error::new(ErrorCode::UnknownTemplate, format!("Template {h} was not found or is already disposed"))
}

fn handle(req: &Json) -> Result<u32, Error> {
    match req.get("template") {
        Some(Json::Num(n)) => n.parse().map_err(|_| invalid("\"template\" must be a template handle")),
        _ => Err(invalid("\"template\" must be a template handle")),
    }
}

/// Reads a binary field: `{"$blob": n}` or `{"$base64": "..."}`. `None` when absent or null.
fn blob(req: &Json, field: &str, blobs: &[Rc<[u8]>]) -> Result<Option<Rc<[u8]>>, Error> {
    match req.get(field) {
        None | Some(Json::Null) => Ok(None),
        Some(j) => match Value::from_json(j, blobs)? {
            Value::Bytes(b) => Ok(Some(b)),
            _ => Err(invalid(format!("\"{field}\" must be {{\"$blob\": <index>}}"))),
        },
    }
}

/// Nesting limits of a request: `model` of `build` may be as deep as a model of a document within
/// the request's `maxXmlDepth`; every other field is limited to `json::MAX_DEPTH`.
fn check_depth(req: &Json) -> Result<(), Error> {
    let Json::Obj(fields) = req else { return Ok(()) };
    let is_build = req.get("op").and_then(Json::as_str) == Some("build");
    let xml_depth = req
        .get("limits")
        .and_then(|l| l.get("maxXmlDepth"))
        .and_then(|d| match d {
            Json::Num(n) => n.parse::<u64>().ok(),
            _ => None,
        })
        .unwrap_or(Limits::default().max_xml_depth);
    for (k, v) in fields {
        let max = if is_build && k == "model" { json::model_depth(xml_depth) } else { json::MAX_DEPTH };
        if v.depth(max + 1) > max {
            return Err(invalid(format!("Invalid JSON: \"{k}\" is nested too deep")));
        }
    }
    Ok(())
}

fn limits(req: &Json) -> Result<Limits, Error> {
    match req.get("limits") {
        None => Ok(Limits::default()),
        Some(l) => parse_limits(&l.to_serde()),
    }
}

fn compile(req: &Json, blobs: &[Rc<[u8]>]) -> Result<Template, Errors> {
    let opts = Options::from_json(req.get("options").map(Json::to_serde).as_ref())?;
    let bytes = blob(req, "template", blobs)?.ok_or_else(|| invalid("Missing \"template\" blob"))?;
    Template::compile(&bytes, opts)
}

fn data(req: &Json, blobs: &[Rc<[u8]>]) -> Result<Value, Error> {
    match req.get("data") {
        None => Ok(Value::object(Default::default())),
        Some(d) => Value::from_json(d, blobs),
    }
}

// ---------------------------------------------------------------------------------------------
// Host callbacks over frames (spec §7)
// ---------------------------------------------------------------------------------------------

struct FrameHost<'a> {
    transport: &'a mut dyn HostTransport,
}

fn tag_json(tag: &TagInfo) -> Json {
    Json::Obj(vec![
        ("raw".into(), Json::Str(tag.raw.into())),
        ("expression".into(), Json::Str(tag.expression.into())),
        ("part".into(), Json::Str(tag.part.into())),
    ])
}

impl FrameHost<'_> {
    fn exchange(&mut self, request: Json, blobs: Vec<Rc<[u8]>>) -> Result<Value, HostError> {
        let mut text = String::new();
        json::write(&mut text, &request);
        let frame = Frame::new(text, blobs.iter().map(|b| b.to_vec()).collect());
        let response = self.transport.call(&frame.encode()).ok_or(HostError::Unsupported)?;
        let response = Frame::decode(&response).map_err(|e| HostError::Invalid(e.0))?;
        let body = json::parse(&response.json).map_err(|e| HostError::Invalid(e.message))?;
        let rblobs: Vec<Rc<[u8]>> = response.blobs.into_iter().map(Rc::from).collect();
        match body.get("ok") {
            Some(Json::Bool(true)) => match body.get("value") {
                Some(v) => Value::from_json(v, &rblobs).map_err(|e| HostError::Invalid(e.message)),
                None => Ok(Value::Undefined),
            },
            Some(Json::Bool(false)) => {
                Err(HostError::Failed(body.get("message").and_then(Json::as_str).unwrap_or("host callback failed").to_string()))
            }
            _ => Err(HostError::Invalid("response must have \"ok\": true|false".into())),
        }
    }
}

impl Host for FrameHost<'_> {
    fn filter(&mut self, name: &str, value: &Value, args: &[Value], tag: &TagInfo) -> Result<Value, HostError> {
        let mut blobs = Vec::new();
        let v = value.to_tagged_json(&mut blobs);
        let a = Json::Arr(args.iter().map(|x| x.to_tagged_json(&mut blobs)).collect());
        let req = Json::Obj(vec![
            ("call".into(), Json::Str("filter".into())),
            ("name".into(), Json::Str(name.into())),
            ("value".into(), v),
            ("args".into(), a),
            ("tag".into(), tag_json(tag)),
        ]);
        self.exchange(req, blobs)
    }

    fn null_value(&mut self, tag: &TagInfo) -> Result<Value, HostError> {
        let req = Json::Obj(vec![("call".into(), Json::Str("nullValue".into())), ("tag".into(), tag_json(tag))]);
        self.exchange(req, Vec::new())
    }

    fn stringify(&mut self, value: &Value, tag: &TagInfo) -> Result<Value, HostError> {
        let mut blobs = Vec::new();
        let v = value.to_tagged_json(&mut blobs);
        let req = Json::Obj(vec![("call".into(), Json::Str("stringify".into())), ("value".into(), v), ("tag".into(), tag_json(tag))]);
        self.exchange(req, blobs)
    }
}

/// An error response frame (for transports that fail before reaching an instance).
pub fn error_response(code: ErrorCode, message: &str) -> Vec<u8> {
    error_frame(None, &Errors::from(Error::new(code, message))).encode()
}

/// Builds a request frame from JSON text and blobs (helper for hosts and tests).
pub fn request(json: &str, blobs: Vec<Vec<u8>>) -> Vec<u8> {
    Frame::new(json, blobs).encode()
}
