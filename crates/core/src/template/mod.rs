//! Templating: compile a document once, render it many times.

pub mod engine;
pub mod expression;
pub mod filters;
pub mod lexer;
pub mod locale;
pub mod options;
pub mod tags;

use crate::error::{Error, Errors, Result};
use crate::formats::xlsx::{self, XlsxState};
use crate::formats::DocumentFormat;
use crate::host::Host;
use crate::opc::Package;
use crate::value::Value;
use crate::xml::tokenizer::{element_end, find_local_tags, find_tags, raw_attr, set_raw_attribute};
use engine::{compile_part, inspect_nodes, render_part, CompiledPart, RenderCtx, TemplateTag};
use options::Options;
use serde_json::{json, Map, Value as SJson};
use std::collections::HashMap;

/// A parsed template, ready to be rendered many times.
#[derive(Debug, Clone)]
pub struct Template {
    pub format: DocumentFormat,
    pkg: Package,
    compiled: Vec<CompiledPart>,
    opts: Options,
    xlsx: XlsxState,
    /// First id for new drawings: above every `docPr`/`cNvPr` id in the template (repeats are fixed
    /// after rendering).
    first_drawing_id: u32,
    /// Per template part: how often each drawing id occurs (DOCX/PPTX).
    drawing_ids: Vec<HashMap<u32, usize>>,
}

/// New drawing ids start here (as in the TS version) unless the template already uses higher ids.
const DRAWING_ID_BASE: u32 = 0x7000;

/// Ids of every `docPr` / `cNvPr` element (any prefix) in `xml`.
fn drawing_ids<'a>(xml: &'a str, local: &'a str) -> impl Iterator<Item = (usize, usize, Option<u32>)> + 'a {
    find_local_tags(xml, local).map(|(s, e)| (s, e, raw_attr(&xml[s..e], "id").and_then(|v| v.trim().parse::<u32>().ok())))
}

fn max_drawing_id(xml: &str) -> u32 {
    drawing_ids(xml, "docPr").chain(drawing_ids(xml, "cNvPr")).filter_map(|(_, _, id)| id).max().unwrap_or(0)
}

/// Every `local` id of `xml`: (start, end, id, start of the enclosing `mc:AlternateContent`,
/// inside `mc:Fallback`). Office writes the same id in `mc:Choice` and `mc:Fallback` on purpose.
fn id_occurrences(xml: &str, local: &str) -> Vec<(usize, usize, u32, Option<usize>, bool)> {
    let ranges = |tag: &str| -> Vec<(usize, usize)> {
        if !xml.contains(tag) {
            return Vec::new();
        }
        find_tags(xml, tag).filter_map(|(s, e)| Some((s, element_end(xml, s, e)?))).collect()
    };
    let (alts, fallbacks) = (ranges("<mc:AlternateContent"), ranges("<mc:Fallback"));
    drawing_ids(xml, local)
        .filter_map(|(s, e, id)| {
            let alt = alts.iter().rev().find(|(a, b)| *a <= s && s < *b).map(|(a, _)| *a);
            let fallback = fallbacks.iter().any(|(a, b)| *a <= s && s < *b);
            Some((s, e, id?, alt, fallback))
        })
        .collect()
}

/// How often the template uses each drawing id (fallback copies not counted).
fn template_ids(xml: &str, local: &str) -> HashMap<u32, usize> {
    let mut m = HashMap::new();
    for (_, _, id, _, fallback) in id_occurrences(xml, local) {
        if !fallback {
            *m.entry(id).or_default() += 1;
        }
    }
    m
}

/// Gives a fresh id (from `counter`) to drawing ids that occur more often than in the template:
/// drawings copied by loops. `seen` counts occurrences across the scope (DOCX: the document,
/// PPTX: one slide). The `mc:Fallback` of a renumbered `mc:Choice` gets the same new id, and
/// connectors (`a:stCxn`, `a:endCxn`) follow the latest id given to the shape they refer to.
fn dedupe_drawing_ids(
    xml: String,
    local: &str,
    template: &HashMap<u32, usize>,
    seen: &mut HashMap<u32, usize>,
    counter: &mut u32,
) -> String {
    if !xml.contains(local) {
        return xml;
    }
    // (start, end, id, enclosing AlternateContent, in a Fallback, is a connection)
    let mut items: Vec<(usize, usize, u32, Option<usize>, bool, bool)> =
        id_occurrences(&xml, local).into_iter().map(|(s, e, id, alt, fb)| (s, e, id, alt, fb, false)).collect();
    if local == "cNvPr" && xml.contains("Cxn ") {
        for tag in ["<a:stCxn", "<a:endCxn"] {
            for (s, e) in find_tags(&xml, tag) {
                if let Some(id) = raw_attr(&xml[s..e], "id").and_then(|v| v.trim().parse::<u32>().ok()) {
                    items.push((s, e, id, None, false, true));
                }
            }
        }
        items.sort_by_key(|i| i.0);
    }
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    let mut renamed: HashMap<(usize, u32), u32> = HashMap::new();
    let mut latest: HashMap<u32, u32> = HashMap::new();
    for (s, e, id, alt, fallback, connection) in items {
        let new = if connection {
            latest.get(&id).copied().filter(|n| *n != id)
        } else if fallback {
            alt.and_then(|a| renamed.get(&(a, id)).copied())
        } else {
            let n = seen.entry(id).or_default();
            *n += 1;
            let fresh = (*n > template.get(&id).copied().unwrap_or(0).max(1)).then(|| {
                *counter = counter.checked_add(1).unwrap_or(1);
                if let Some(a) = alt {
                    renamed.insert((a, id), *counter);
                }
                *counter
            });
            latest.insert(id, fresh.unwrap_or(id));
            fresh
        };
        if let Some(new) = new {
            out.push_str(&xml[last..s]);
            out.push_str(&set_raw_attribute(&xml[s..e], "id", &new.to_string()));
            last = e;
        }
    }
    if last == 0 {
        return xml;
    }
    out.push_str(&xml[last..]);
    out
}

impl Template {
    /// Compiles a template. All template problems are reported, not only the first one.
    pub fn compile(input: &[u8], opts: Options) -> std::result::Result<Template, Errors> {
        opts.validate()?;
        let pkg = Package::load(input, &opts.limits)?;
        let format = DocumentFormat::detect(&pkg)?;
        let hooks = format.hooks();
        let mut xlsx = XlsxState::default();
        let mut compiled = Vec::new();
        let mut errors = Vec::new();
        let mut first_drawing_id = DRAWING_ID_BASE;
        let mut drawing_ids = Vec::new();
        for part in format.template_parts(&pkg) {
            let mut xml = pkg.get_text(&part).unwrap_or_default();
            if format == DocumentFormat::Xlsx {
                xml = xlsx::preprocess(&pkg, &part, &xml, &mut xlsx, &opts.start);
            } else {
                first_drawing_id = first_drawing_id.max(max_drawing_id(&xml));
                drawing_ids.push(template_ids(&xml, format.drawing_id_element()));
            }
            match compile_part(&xml, hooks, &opts, &part) {
                Ok(c) => compiled.push(c),
                Err(mut e) => errors.append(&mut e),
            }
        }
        if !errors.is_empty() {
            return Err(Errors(errors));
        }
        xlsx.shared = None; // only needed while compiling
        Ok(Template { format, pkg, compiled, opts, xlsx, first_drawing_id, drawing_ids })
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    /// Template parts, in document order.
    pub fn parts(&self) -> Vec<String> {
        self.compiled.iter().map(|c| c.part.clone()).collect()
    }

    /// Renders the template with `data` and returns the bytes of the resulting document.
    pub fn render(&self, data: &Value, host: &mut dyn Host) -> Result<Vec<u8>> {
        let mut pkg = self.pkg.clone();
        let hooks = self.format.hooks();
        let mut ctx = RenderCtx {
            opts: &self.opts,
            part: String::new(),
            pkg: &mut pkg,
            host,
            doc_pr: self.first_drawing_id,
            shape_id: self.first_drawing_id,
            loop_iterations: 0,
            host_calls: 0,
        };
        let mut row_maps = Vec::new();
        // drawing ids are unique across a DOCX document, and within one PPTX slide
        let doc_ids: HashMap<u32, usize> = match self.format {
            DocumentFormat::Docx => self.drawing_ids.iter().flat_map(|m| m.iter()).fold(HashMap::new(), |mut acc, (id, n)| {
                *acc.entry(*id).or_default() += n;
                acc
            }),
            _ => HashMap::new(),
        };
        let mut doc_seen = HashMap::new();
        for (i, c) in self.compiled.iter().enumerate() {
            ctx.part = c.part.clone();
            let mut xml = render_part(c, data, hooks, &mut ctx)?;
            match self.format {
                DocumentFormat::Xlsx => {
                    let (next, map) = xlsx::postprocess(&xml, &c.part, &self.xlsx);
                    xml = next;
                    if let Some(m) = map {
                        row_maps.push((c.part.clone(), m));
                    }
                }
                DocumentFormat::Docx => xml = dedupe_drawing_ids(xml, "docPr", &doc_ids, &mut doc_seen, &mut ctx.doc_pr),
                DocumentFormat::Pptx => {
                    let mut seen = HashMap::new();
                    xml = dedupe_drawing_ids(xml, "cNvPr", &self.drawing_ids[i], &mut seen, &mut ctx.shape_id);
                }
            }
            ctx.pkg.set_text(&c.part, &xml);
        }
        if self.format == DocumentFormat::Xlsx {
            xlsx::finalize(ctx.pkg, &row_maps);
        }
        pkg.to_bytes(&self.opts.limits)
    }

    /// Describes the tags of the template: tree, variable paths and a sample data shape.
    pub fn inspect(&self) -> InspectResult {
        let parts: Vec<(String, Vec<TemplateTag>)> = self.compiled.iter().map(|c| (c.part.clone(), inspect_nodes(&c.nodes))).collect();
        let tags: Vec<TemplateTag> = parts.iter().flat_map(|(_, t)| t.clone()).collect();
        let mut variables: Vec<String> = Vec::new();
        let mut schema = Map::new();
        collect(&tags, "", &mut schema, &mut variables, 0);
        InspectResult { format: self.format, parts, tags, variables, schema }
    }
}

#[derive(Debug, Clone)]
pub struct InspectResult {
    pub format: DocumentFormat,
    /// Tag tree per package part.
    pub parts: Vec<(String, Vec<TemplateTag>)>,
    /// Tag tree of the whole document (all parts merged, in order).
    pub tags: Vec<TemplateTag>,
    /// Every data path used, loops marked with `[]`: `client.name`, `items[].price`.
    pub variables: Vec<String>,
    /// A sample data object with the shape the template expects.
    pub schema: Map<String, SJson>,
}

impl TemplateTag {
    pub fn to_json(&self) -> SJson {
        match self {
            TemplateTag::Variable { path, expression, filters, raw } => {
                json!({ "type": "variable", "path": path, "expression": expression, "filters": filters, "raw": raw })
            }
            TemplateTag::Image { path, expression, raw } => json!({ "type": "image", "path": path, "expression": expression, "raw": raw }),
            TemplateTag::Section { mode, path, expression, raw, children, else_children } => {
                let mut o = json!({
                    "type": "section", "mode": mode, "path": path, "expression": expression, "raw": raw,
                    "children": children.iter().map(TemplateTag::to_json).collect::<Vec<_>>(),
                });
                if let Some(e) = else_children {
                    o["elseChildren"] = SJson::Array(e.iter().map(TemplateTag::to_json).collect());
                }
                o
            }
        }
    }
}

impl InspectResult {
    pub fn to_json(&self) -> SJson {
        json!({
            "format": self.format,
            "parts": self.parts.iter().map(|(p, t)| json!({ "part": p, "tags": t.iter().map(TemplateTag::to_json).collect::<Vec<_>>() })).collect::<Vec<_>>(),
            "tags": self.tags.iter().map(TemplateTag::to_json).collect::<Vec<_>>(),
            "variables": self.variables,
            "schema": self.schema,
        })
    }
}

fn add_var(out: &mut Vec<String>, v: String) {
    if !out.contains(&v) {
        out.push(v);
    }
}

/// The sample data shape stops growing below this depth, so the `inspect` response stays within
/// the JSON nesting limits of hosts (often 128) whatever the paths look like.
const MAX_SCHEMA_DEPTH: usize = 64;

/// Walks the tag tree and fills `schema` (sample data, `depth` levels below the root) and `out`
/// (dotted paths). Returns true when any tag inside references data (including `{{.}}`).
fn collect(tags: &[TemplateTag], prefix: &str, schema: &mut Map<String, SJson>, out: &mut Vec<String>, depth: usize) -> bool {
    let mut referenced = false;
    for t in tags {
        let (path, is_section) = match t {
            TemplateTag::Variable { path, .. } | TemplateTag::Image { path, .. } => (path.as_str(), false),
            TemplateTag::Section { path, .. } => (path.as_str(), true),
        };
        let is_self = path == "." || path == "this";
        let is_local = path.starts_with('@');
        let mut clean = path;
        while let Some(r) = clean.strip_prefix("../") {
            clean = r;
        }
        let clean = clean.strip_prefix("this.").unwrap_or(clean);
        let full = format!("{prefix}{clean}");
        if !path.is_empty() && !is_local {
            referenced = true;
        }

        if !is_section {
            if path.is_empty() || is_local || is_self {
                continue;
            }
            add_var(out, full);
            if depth + split_path(clean).len() <= MAX_SCHEMA_DEPTH && get_path(schema, clean).is_none() {
                let v = if matches!(t, TemplateTag::Image { .. }) { SJson::Null } else { SJson::String(String::new()) };
                set_path(schema, clean, v);
            }
            continue;
        }
        let TemplateTag::Section { mode, children, else_children, .. } = t else { continue };
        if path.is_empty() || is_local {
            referenced = collect(children, prefix, schema, out, depth) || referenced;
        } else {
            // the loop item sits one array level below the path
            let item_depth = depth + split_path(clean).len() + 1;
            let mut child_schema = Map::new();
            let mut child_vars = Vec::new();
            let child_refs = collect(children, "", &mut child_schema, &mut child_vars, item_depth);
            // {{#each x}} is always a loop; {{#x}} is treated as a loop when its body uses data.
            let is_loop = *mode == tags::SectionMode::Each || (*mode == tags::SectionMode::Section && child_refs);
            if is_loop {
                if !is_self && item_depth <= MAX_SCHEMA_DEPTH {
                    let item = if child_schema.is_empty() { SJson::String(String::new()) } else { SJson::Object(child_schema) };
                    set_path(schema, clean, SJson::Array(vec![item]));
                }
                for v in &child_vars {
                    add_var(out, format!("{full}[].{v}"));
                }
                if child_vars.is_empty() {
                    add_var(out, format!("{full}[]"));
                }
            } else {
                if !is_self {
                    add_var(out, full.clone());
                    if item_depth <= MAX_SCHEMA_DEPTH && get_path(schema, clean).is_none() {
                        set_path(schema, clean, SJson::Bool(true));
                    }
                }
                // condition bodies share the outer scope
                collect(children, prefix, schema, out, depth);
            }
        }
        if let Some(e) = else_children {
            referenced = collect(e, prefix, schema, out, depth) || referenced;
        }
    }
    referenced
}

/// Merges two sample shapes, e.g. two loops over the same array using different fields.
fn merge_shape(a: Option<SJson>, b: SJson) -> SJson {
    match (a, b) {
        (None, b) => b,
        (Some(SJson::Array(mut a)), SJson::Array(mut b)) => {
            let x = if a.is_empty() { None } else { Some(a.remove(0)) };
            let y = if b.is_empty() { SJson::Null } else { b.remove(0) };
            SJson::Array(vec![merge_shape(x, y)])
        }
        (Some(SJson::Object(mut a)), SJson::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(slot) => {
                        let prev = std::mem::take(slot);
                        *slot = merge_shape(Some(prev), v);
                    }
                    None => {
                        a.insert(k, v);
                    }
                }
            }
            SJson::Object(a)
        }
        (Some(a), b) => {
            if a.is_object() || a.is_array() {
                a
            } else if b.is_object() || b.is_array() {
                b
            } else {
                a
            }
        }
    }
}

/// `a.b[0]["k"]` → `a`, `b`, `k` (indexes are dropped).
fn split_path(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            '[' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut inner = String::new();
                for n in chars.by_ref() {
                    if n == ']' {
                        break;
                    }
                    inner.push(n);
                }
                let t = inner.trim();
                if (t.starts_with('"') && t.ends_with('"') || t.starts_with('\'') && t.ends_with('\'')) && t.len() >= 2 {
                    let k = &t[1..t.len() - 1];
                    if !k.is_empty() {
                        out.push(k.to_string());
                    }
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn get_path<'a>(obj: &'a Map<String, SJson>, path: &str) -> Option<&'a SJson> {
    let segs = split_path(path);
    let (first, rest) = segs.split_first()?;
    let mut cur = obj.get(first)?;
    for s in rest {
        cur = cur.as_object()?.get(s)?;
    }
    Some(cur)
}

fn set_path(obj: &mut Map<String, SJson>, path: &str, value: SJson) {
    let segs = split_path(path);
    let Some((last, init)) = segs.split_last() else { return };
    let mut cur = obj;
    for seg in init {
        let slot = cur.entry(seg.clone()).or_insert(SJson::Null);
        if !slot.is_object() {
            *slot = SJson::Object(Map::new());
        }
        let SJson::Object(m) = slot else { return };
        cur = m;
    }
    match cur.get_mut(last) {
        Some(slot) => {
            let prev = std::mem::take(slot);
            *slot = merge_shape(Some(prev), value);
        }
        None => {
            cur.insert(last.clone(), value);
        }
    }
}

/// Convenience: compile + render once.
pub fn render_once(input: &[u8], opts: Options, data: &Value, host: &mut dyn Host) -> std::result::Result<Vec<u8>, Errors> {
    let t = Template::compile(input, opts)?;
    t.render(data, host).map_err(Errors::from)
}

impl From<Errors> for Error {
    fn from(e: Errors) -> Self {
        e.0.into_iter().next().unwrap_or_else(|| Error::new(crate::ErrorCode::Internal, "empty error list"))
    }
}
