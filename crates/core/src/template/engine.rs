//! The template engine: finds tags in document text, decides what each section repeats
//! (inline content, whole paragraphs or table rows) and renders the node tree.

use super::expression::{resolve_operand, Expression, Scope};
use super::filters;
use super::lexer::{lex_segments, LexPart};
use super::options::{NullValue, Options, StringifyMode};
use super::tags::{closes, parse_tag, ExprTag, OpenTag, ParsedTag, SectionMode};
use crate::error::{Error, ErrorCode, Result};
use crate::host::{Host, HostError, TagInfo};
use crate::image::{is_image_value, resolve_image, ResolvedImage};
use crate::limits::exceeds;
use crate::opc::Package;
use crate::value::Value;
use crate::xml::escape::{decode_xml, escape_xml};
use crate::xml::tokenizer::{set_raw_attribute, tokenize, TokKind};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------------------------
// Format specification: tells the engine how a given XML dialect stores text and structure.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMode {
    Inline,
    Shape,
    None,
}

pub struct FormatSpec {
    /// Elements whose character data is document text (`w:t`, `a:t`, `t`).
    pub text_tags: &'static [&'static str],
    /// Elements that bound a tag: a tag can't span two of them (`w:p`, `a:p`, `c`).
    pub group_tags: &'static [&'static str],
    /// Paragraph element; a section whose tags are alone in their paragraphs repeats paragraphs.
    pub paragraph_tag: Option<&'static str>,
    /// Table-row elements; a section spanning cells of one row repeats the row.
    pub row_tags: &'static [&'static str],
    /// Elements that count as content when deciding whether a paragraph holds only a tag.
    pub content_tags: &'static [&'static str],
    /// Add `xml:space="preserve"` to text elements that receive values (DOCX, XLSX).
    pub preserve_space: bool,
    pub image_mode: ImageMode,
    /// Element replaced by a picture in `Shape` mode (`p:sp`).
    pub shape_tag: Option<&'static str>,
}

/// Format-specific rendering hooks.
pub trait FormatHooks {
    fn spec(&self) -> &'static FormatSpec;
    /// XML inserted for `\n` / `\t` in values, computed from the tag position.
    fn breaks(&self, _ctx: &TokenContext) -> (Option<String>, Option<String>) {
        (None, None)
    }
    /// Custom rendering for a value (e.g. numeric cells in XLSX).
    fn render_scalar(&self, _value: &Value, _text: &str) -> Option<String> {
        None
    }
    fn render_image(&self, _img: &ResolvedImage, _ctx: &mut RenderCtx, _shape_xml: Option<&str>) -> Result<String> {
        Err(Error::new(ErrorCode::Unsupported, "Image tags are not supported in this format yet"))
    }
}

pub struct TokenContext<'a> {
    pub tokens: &'a [Tok],
    /// Indices of open elements enclosing the tag, outermost first.
    pub stack: &'a [usize],
    pub close_of: &'a HashMap<usize, usize>,
}

// ---------------------------------------------------------------------------------------------
// Tokens and nodes
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlKind {
    Open,
    Close,
    SelfClosing,
    Other,
}

// Tokens only live while a part is compiled; boxing the tag would cost more than it saves.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Tok {
    Xml { raw: String, name: String, kind: XmlKind },
    Text(String),
    Tag(ParsedTag),
}

impl Tok {
    pub fn name(&self) -> &str {
        match self {
            Tok::Xml { name, .. } => name,
            _ => "",
        }
    }
    fn xml_text(&self) -> String {
        match self {
            Tok::Xml { raw, .. } => raw.clone(),
            Tok::Text(v) => escape_xml(v),
            Tok::Tag(_) => String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Raw(String),
    Text(String),
    Var { tag: ExprTag, br: Option<String>, tab: Option<String> },
    Image { tag: ExprTag, shape_xml: Option<String> },
    Section { tag: OpenTag, body: Vec<Node>, else_body: Option<Vec<Node>> },
}

#[derive(Debug, Clone)]
pub struct CompiledPart {
    pub part: String,
    pub nodes: Vec<Node>,
    pub has_tags: bool,
}

#[derive(Debug, Clone)]
struct Ranged {
    is_image: bool,
    from: usize,
    to: usize,
    order: usize,
    open: usize,
    bodies: Vec<(usize, usize)>,
    tag_index: usize,
}

// ---------------------------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------------------------

pub fn compile_part(xml: &str, hooks: &dyn FormatHooks, opts: &Options, part: &str) -> std::result::Result<CompiledPart, Vec<Error>> {
    let first: String = opts.start.chars().take(1).collect();
    if !xml.contains(&escape_xml(&first)) {
        return Ok(CompiledPart { part: part.into(), nodes: vec![Node::Raw(xml.into())], has_tags: false });
    }
    let spec = hooks.spec();
    let tokens = prepare_tokens(xml, spec, opts, part)?;
    if !tokens.iter().any(|t| matches!(t, Tok::Tag(_))) {
        let xml: String = tokens.iter().map(Tok::xml_text).collect();
        return Ok(CompiledPart { part: part.into(), nodes: vec![Node::Raw(xml)], has_tags: false });
    }
    let (close_of, stacks) = compute_structure(&tokens, part, opts.limits.max_xml_depth).map_err(|e| vec![e])?;
    let items = compute_ranges(&tokens, &stacks, &close_of, spec, part)?;
    let nodes = build_nodes(&tokens, &stacks, &close_of, hooks, &items, part).map_err(|e| vec![e])?;
    Ok(CompiledPart { part: part.into(), nodes, has_tags: true })
}

/// Tokenizes XML, finds tags inside document text and splits text tokens around them.
fn prepare_tokens(xml: &str, spec: &FormatSpec, opts: &Options, part: &str) -> std::result::Result<Vec<Tok>, Vec<Error>> {
    let xt = tokenize(xml).map_err(|e| vec![e.with_part(part)])?;

    // 1. Collect text-container character data grouped by paragraph/cell.
    let mut group_segments: IndexMap<isize, Vec<usize>> = IndexMap::new();
    let mut text_open_of: HashMap<usize, usize> = HashMap::new();
    let mut group_stack: Vec<usize> = Vec::new();
    let mut open_text: Option<usize> = None;
    for (i, tok) in xt.iter().enumerate() {
        match tok.kind {
            TokKind::Open { self_closing: true } => {}
            TokKind::Open { self_closing: false } => {
                if spec.group_tags.contains(&tok.name) {
                    group_stack.push(i);
                }
                if spec.text_tags.contains(&tok.name) {
                    open_text = Some(i);
                }
            }
            TokKind::Close => {
                if spec.text_tags.contains(&tok.name) {
                    open_text = None;
                }
                if spec.group_tags.contains(&tok.name) {
                    group_stack.pop();
                }
            }
            TokKind::Text => {
                if let Some(ot) = open_text {
                    let g = group_stack.last().map(|g| *g as isize).unwrap_or(-1);
                    group_segments.entry(g).or_default().push(i);
                    text_open_of.insert(i, ot);
                }
            }
            TokKind::Other => {}
        }
    }

    // 2. Lex each group. A group with an error is reported and left as plain text.
    let mut errors: Vec<Error> = Vec::new();
    let mut parts_of: HashMap<usize, Vec<LexPart>> = HashMap::new();
    for indices in group_segments.values() {
        let segs: Vec<String> = indices.iter().map(|&i| decode_xml(xt[i].raw).into_owned()).collect();
        match lex_segments(&segs, opts, part) {
            Ok(lexed) => {
                for (idx, parts) in indices.iter().zip(lexed) {
                    parts_of.insert(*idx, parts);
                }
            }
            Err(e) => errors.push(e),
        }
    }

    // 3. Mark text elements that receive tags (xml:space="preserve").
    let mut preserve: HashSet<usize> = HashSet::new();
    if spec.preserve_space {
        for (idx, parts) in &parts_of {
            if parts.iter().any(|p| matches!(p, LexPart::Tag { .. })) {
                if let Some(o) = text_open_of.get(idx) {
                    preserve.insert(*o);
                }
            }
        }
    }

    // 4. Emit engine tokens.
    let mut out: Vec<Tok> = Vec::with_capacity(xt.len());
    for (i, tok) in xt.iter().enumerate() {
        if let Some(parts) = parts_of.remove(&i) {
            for p in parts {
                match p {
                    LexPart::Text(v) => out.push(Tok::Text(v)),
                    LexPart::Tag { raw, inner } => match parse_tag(&inner, &raw, opts, part) {
                        Ok(t) => out.push(Tok::Tag(t)),
                        Err(e) => errors.push(e),
                    },
                }
            }
            continue;
        }
        out.push(match tok.kind {
            TokKind::Open { self_closing } => {
                let raw = if preserve.contains(&i) { set_raw_attribute(tok.raw, "xml:space", "preserve") } else { tok.raw.to_string() };
                Tok::Xml { raw, name: tok.name.to_string(), kind: if self_closing { XmlKind::SelfClosing } else { XmlKind::Open } }
            }
            TokKind::Close => Tok::Xml { raw: tok.raw.to_string(), name: tok.name.to_string(), kind: XmlKind::Close },
            _ => Tok::Xml { raw: tok.raw.to_string(), name: String::new(), kind: XmlKind::Other },
        });
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

type Stacks = HashMap<usize, Vec<usize>>;

fn compute_structure(tokens: &[Tok], part: &str, max_depth: u64) -> Result<(HashMap<usize, usize>, Stacks)> {
    let mut close_of = HashMap::new();
    let mut stacks = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        match t {
            Tok::Xml { kind: XmlKind::Open, .. } => {
                stack.push(i);
                if exceeds(stack.len() as u64, max_depth) {
                    return Err(Error::limit("maxXmlDepth", max_depth).with_part(part));
                }
            }
            Tok::Xml { kind: XmlKind::Close, name, .. } => {
                let open = stack
                    .pop()
                    .ok_or_else(|| Error::template(ErrorCode::Unsupported, format!("Malformed XML: unexpected </{name}>"), part, None))?;
                close_of.insert(open, i);
            }
            Tok::Tag(_) => {
                stacks.insert(i, stack.clone());
            }
            _ => {}
        }
    }
    Ok((close_of, stacks))
}

fn open_tag(tokens: &[Tok], i: usize) -> &OpenTag {
    match &tokens[i] {
        Tok::Tag(ParsedTag::Open(t)) => t,
        _ => unreachable!("token {i} is not an opening tag"),
    }
}

fn tag_raw(tokens: &[Tok], i: usize) -> String {
    match &tokens[i] {
        Tok::Tag(t) => t.raw().to_string(),
        _ => String::new(),
    }
}

fn compute_ranges(
    tokens: &[Tok],
    stacks: &Stacks,
    close_of: &HashMap<usize, usize>,
    spec: &FormatSpec,
    part: &str,
) -> std::result::Result<Vec<Ranged>, Vec<Error>> {
    let name_at = |i: usize| tokens[i].name();
    let end_of = |i: usize| close_of.get(&i).copied().unwrap_or(i);
    let err = |code, msg: &str, tag: String| Error::template(code, msg, part, Some(&tag));

    // True when element `el` contains no content except whitespace and the tag at `tag_idx`.
    let alone = |el: usize, tag_idx: usize| -> bool {
        let end = end_of(el);
        for (i, t) in tokens.iter().enumerate().take(end).skip(el + 1) {
            match t {
                Tok::Tag(_) if i != tag_idx => return false,
                Tok::Text(v) if !v.trim().is_empty() => return false,
                Tok::Xml { name, .. } if !name.is_empty() && spec.content_tags.contains(&name.as_str()) => return false,
                _ => {}
            }
        }
        true
    };
    let same_names = |a: &[usize], b: &[usize]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| name_at(*x) == name_at(*y));

    // Pair open/else/close tags.
    struct Pair {
        open: usize,
        close: usize,
        else_at: Option<usize>,
    }
    let mut errors = Vec::new();
    let mut pairs: Vec<Pair> = Vec::new();
    let mut open: Vec<(usize, Option<usize>)> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let Tok::Tag(tag) = t else { continue };
        match tag {
            ParsedTag::Open(_) => open.push((i, None)),
            ParsedTag::Else { raw } => match open.last_mut() {
                None => errors.push(err(ErrorCode::UnmatchedClose, "\"else\" outside of a section", raw.clone())),
                Some((_, Some(_))) => errors.push(err(ErrorCode::InvalidSection, "Duplicate \"else\"", raw.clone())),
                Some(top) => top.1 = Some(i),
            },
            ParsedTag::Close { raw, name } => match open.pop() {
                None => errors.push(err(ErrorCode::UnmatchedClose, "Closing tag without an opening tag", raw.clone())),
                Some((o, else_at)) => {
                    let ot = open_tag(tokens, o);
                    if closes(ot, name) {
                        pairs.push(Pair { open: o, close: i, else_at });
                    } else {
                        errors.push(err(ErrorCode::UnmatchedClose, &format!("Closing tag does not match {}", ot.raw), raw.clone()));
                    }
                }
            },
            _ => {}
        }
    }
    for (o, _) in &open {
        errors.push(err(ErrorCode::UnclosedSection, "Section is never closed", open_tag(tokens, *o).raw.clone()));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut items: Vec<Ranged> = Vec::new();
    let section = |from, to, open, bodies| Ranged { is_image: false, from, to, order: open, open, bodies, tag_index: open };
    for p in &pairs {
        let s = &stacks[&p.open];
        let e = &stacks[&p.close];
        let raw = open_tag(tokens, p.open).raw.clone();
        let mut k = 0;
        while k < s.len() && k < e.len() && s[k] == e[k] {
            k += 1;
        }
        let lca = if k > 0 { Some(s[k - 1]) } else { None };
        let in_group = s[..k].iter().any(|i| spec.group_tags.contains(&name_at(*i)));

        let inline = || -> Result<Ranged> {
            if !same_names(&s[k..], &e[k..]) {
                return Err(err(
                    ErrorCode::InvalidSection,
                    "Opening and closing tags must be in the same paragraph, the same table row, or each alone in its own paragraph",
                    raw.clone(),
                ));
            }
            let bodies = match p.else_at {
                None => vec![(p.open + 1, p.close.saturating_sub(1))],
                Some(el) => {
                    if !same_names(&stacks[&el], s) {
                        return Err(err(ErrorCode::InvalidSection, "\"else\" must be at the same level as its section tags", raw.clone()));
                    }
                    vec![(p.open + 1, el.saturating_sub(1)), (el + 1, p.close.saturating_sub(1))]
                }
            };
            Ok(section(p.open, p.close, p.open, bodies))
        };

        let blocks = |from: usize, to: usize, body_from: usize, body_to: usize, depth: usize, what: &str| -> Result<Ranged> {
            let bodies = match p.else_at {
                None => vec![(body_from, body_to)],
                Some(else_at) => {
                    let el = stacks[&else_at].get(depth).copied();
                    match el {
                        Some(el) if alone(el, else_at) && el >= from && el <= to => {
                            vec![(body_from, el.saturating_sub(1)), (end_of(el) + 1, body_to)]
                        }
                        _ => return Err(err(ErrorCode::InvalidSection, &format!("\"else\" must be alone in its own {what}"), raw.clone())),
                    }
                }
            };
            Ok(section(from, to, p.open, bodies))
        };

        let item = if in_group || lca.is_none() {
            inline()
        } else if spec.row_tags.contains(&name_at(lca.unwrap_or(0))) {
            let lca = lca.unwrap_or(0);
            if p.else_at.is_some() {
                Err(err(ErrorCode::InvalidSection, "\"else\" is not supported in a single-row section; put it in its own row", raw.clone()))
            } else {
                let end = end_of(lca);
                Ok(section(lca, end, p.open, vec![(lca, end)]))
            }
        } else {
            match (s.get(k).copied(), e.get(k).copied()) {
                (Some(a), Some(b)) if spec.row_tags.contains(&name_at(a)) && spec.row_tags.contains(&name_at(b)) => {
                    let end = end_of(b);
                    blocks(a, end, a, end, k, "row")
                }
                (Some(a), Some(b))
                    if spec.paragraph_tag.is_some_and(|pt| name_at(a) == pt && name_at(b) == pt)
                        && alone(a, p.open)
                        && alone(b, p.close) =>
                {
                    blocks(a, end_of(b), end_of(a) + 1, b.saturating_sub(1), k, "paragraph")
                }
                _ => inline(),
            }
        };
        match item {
            Ok(it) => items.push(it),
            Err(e) => errors.push(e),
        }
    }

    if spec.image_mode == ImageMode::Shape {
        if let Some(shape_tag) = spec.shape_tag {
            let mut image_tags: Vec<usize> =
                stacks.keys().copied().filter(|i| matches!(tokens[*i], Tok::Tag(ParsedTag::Image(_)))).collect();
            image_tags.sort_unstable();
            for idx in image_tags {
                let stack = &stacks[&idx];
                match stack.iter().rev().find(|d| name_at(**d) == shape_tag) {
                    None => {
                        errors.push(err(ErrorCode::InvalidImage, "Image tag must be placed inside a text box/shape", tag_raw(tokens, idx)))
                    }
                    Some(&sp) => items.push(Ranged {
                        is_image: true,
                        from: sp,
                        to: end_of(sp),
                        order: idx,
                        open: idx,
                        bodies: Vec::new(),
                        tag_index: idx,
                    }),
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    items.sort_by(|a, b| a.from.cmp(&b.from).then(b.to.cmp(&a.to)).then(a.order.cmp(&b.order)));
    Ok(items)
}

fn build_nodes(
    tokens: &[Tok],
    stacks: &Stacks,
    close_of: &HashMap<usize, usize>,
    hooks: &dyn FormatHooks,
    items: &[Ranged],
    part: &str,
) -> Result<Vec<Node>> {
    let spec = hooks.spec();
    let overlap = |it: &Ranged| {
        Error::template(
            ErrorCode::InvalidSection,
            "Sections overlap: a section must be fully inside another one",
            part,
            Some(&tag_raw(tokens, it.tag_index)),
        )
    };

    fn push(nodes: &mut Vec<Node>, node: Node) {
        if let (Node::Raw(x), Some(Node::Raw(last))) = (&node, nodes.last_mut()) {
            last.push_str(x);
            return;
        }
        nodes.push(node);
    }

    struct Ctx<'a> {
        tokens: &'a [Tok],
        stacks: &'a Stacks,
        close_of: &'a HashMap<usize, usize>,
        hooks: &'a dyn FormatHooks,
        spec: &'a FormatSpec,
        part: &'a str,
    }

    fn build(c: &Ctx, from: usize, to: usize, list: &[Ranged], overlap: &dyn Fn(&Ranged) -> Error) -> Result<Vec<Node>> {
        let mut nodes = Vec::new();
        let mut idx = 0;
        let mut i = from;
        while i <= to && i < c.tokens.len() {
            let item = list.get(idx);
            if let Some(item) = item {
                if item.from < i {
                    return Err(overlap(item));
                }
                if item.from == i {
                    let mut j = idx + 1;
                    while j < list.len() && list[j].from <= item.to {
                        if list[j].to > item.to {
                            return Err(overlap(&list[j]));
                        }
                        j += 1;
                    }
                    push(&mut nodes, make_node(c, item, &list[idx + 1..j], overlap)?);
                    idx = j;
                    i = item.to + 1;
                    continue;
                }
            }
            match &c.tokens[i] {
                Tok::Xml { raw, .. } => push(&mut nodes, Node::Raw(raw.clone())),
                Tok::Text(v) => push(&mut nodes, Node::Text(v.clone())),
                Tok::Tag(ParsedTag::Var(t)) => {
                    let (br, tab) = c.hooks.breaks(&TokenContext { tokens: c.tokens, stack: &c.stacks[&i], close_of: c.close_of });
                    nodes.push(Node::Var { tag: t.clone(), br, tab });
                }
                Tok::Tag(ParsedTag::Image(t)) => {
                    if c.spec.image_mode == ImageMode::None {
                        return Err(Error::template(
                            ErrorCode::Unsupported,
                            "Image tags are not supported in this format yet",
                            c.part,
                            Some(&t.raw),
                        ));
                    }
                    nodes.push(Node::Image { tag: t.clone(), shape_xml: None });
                }
                // open/close/else tags render as nothing
                Tok::Tag(_) => {}
            }
            i += 1;
        }
        if let Some(rest) = list.get(idx) {
            return Err(overlap(rest));
        }
        Ok(nodes)
    }

    fn make_node(c: &Ctx, item: &Ranged, children: &[Ranged], overlap: &dyn Fn(&Ranged) -> Error) -> Result<Node> {
        if item.is_image {
            let Tok::Tag(ParsedTag::Image(tag)) = &c.tokens[item.tag_index] else { unreachable!() };
            let xml: String = c.tokens[item.from..=item.to].iter().map(Tok::xml_text).collect();
            return Ok(Node::Image { tag: tag.clone(), shape_xml: Some(xml) });
        }
        let tag = open_tag(c.tokens, item.open).clone();
        let mut bodies = Vec::new();
        for &(f, t) in &item.bodies {
            let inside: Vec<Ranged> = children.iter().filter(|ch| ch.from >= f && ch.to <= t).cloned().collect();
            for ch in children {
                let is_inside = ch.from >= f && ch.to <= t;
                if !is_inside && ch.from <= t && ch.to >= f {
                    return Err(overlap(ch));
                }
            }
            bodies.push(if f > t { Vec::new() } else { build(c, f, t, &inside, overlap)? });
        }
        let mut it = bodies.into_iter();
        let body = it.next().unwrap_or_default();
        Ok(Node::Section { tag, body, else_body: it.next() })
    }

    let c = Ctx { tokens, stacks, close_of, hooks, spec, part };
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    build(&c, 0, tokens.len() - 1, items, &overlap)
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

pub struct RenderCtx<'a> {
    pub opts: &'a Options,
    pub part: String,
    pub pkg: &'a mut Package,
    pub host: &'a mut dyn Host,
    /// Unique ids for drawings (`wp:docPr`, `p:cNvPr`).
    pub doc_pr: u32,
    pub shape_id: u32,
    pub loop_iterations: u64,
    pub host_calls: u64,
}

impl RenderCtx<'_> {
    fn count_host_call(&mut self) -> Result<()> {
        self.host_calls += 1;
        let max = self.opts.limits.max_host_calls;
        if exceeds(self.host_calls, max) {
            return Err(Error::limit("maxHostCalls", max).with_part(&self.part));
        }
        Ok(())
    }
}

fn host_failure(e: HostError, filter: bool, part: &str, raw: &str) -> Error {
    match e {
        HostError::Failed(m) if filter => {
            Error::template(ErrorCode::FilterFailed, format!("Filter failed: {m}"), part, Some(raw)).with_detail("hostMessage", m)
        }
        HostError::Failed(m) => {
            Error::template(ErrorCode::HostError, format!("Host callback failed: {m}"), part, Some(raw)).with_detail("hostMessage", m)
        }
        HostError::Unsupported => Error::template(ErrorCode::HostError, "The host does not support callbacks", part, Some(raw)),
        HostError::Invalid(m) => Error::template(ErrorCode::HostError, format!("Invalid host response: {m}"), part, Some(raw)),
    }
}

fn evaluate(raw: &str, expr: &Expression, scope: &Scope, ctx: &mut RenderCtx) -> Result<Value> {
    let mut value = resolve_operand(&expr.operand, scope);
    for call in &expr.calls {
        let args: Vec<Value> = call.args.iter().map(|a| resolve_operand(a, scope)).collect();
        if ctx.opts.is_host_filter(&call.name) {
            ctx.count_host_call()?;
            let info = TagInfo { raw, expression: &expr.source, part: &ctx.part };
            value = ctx.host.filter(&call.name, &value, &args, &info).map_err(|e| host_failure(e, true, &ctx.part, raw))?;
        } else {
            value = filters::apply(&call.name, value, &args).ok_or_else(|| {
                Error::template(ErrorCode::UnknownFilter, format!("Unknown filter \"{}\"", call.name), &ctx.part, Some(raw))
            })?;
        }
    }
    Ok(value)
}

/// Default conversion of a value into text (the TS `defaultStringify`).
pub fn default_stringify(value: &Value, date_format: &str) -> String {
    match value {
        Value::String(s) => s.to_string(),
        Value::Undefined | Value::Null => String::new(),
        Value::Date(d) => {
            if date_format == "iso" || date_format.is_empty() {
                d.to_iso()
            } else {
                d.format(date_format)
            }
        }
        Value::Array(a) => a.iter().map(|x| default_stringify(x, date_format)).collect::<Vec<_>>().join(", "),
        Value::Object(_) => value.to_json_string("").unwrap_or_default(),
        v => v.js_string(),
    }
}

fn stringify(value: &Value, tag: &ExprTag, ctx: &mut RenderCtx) -> Result<String> {
    if let Value::String(s) = value {
        return Ok(s.to_string());
    }
    if ctx.opts.stringify == StringifyMode::Host && !value.is_nullish() {
        ctx.count_host_call()?;
        let info = TagInfo { raw: &tag.raw, expression: &tag.expr.source, part: &ctx.part };
        let v = ctx.host.stringify(value, &info).map_err(|e| host_failure(e, false, &ctx.part, &tag.raw))?;
        return Ok(match v {
            Value::String(s) => s.to_string(),
            other => other.js_string(),
        });
    }
    Ok(default_stringify(value, &ctx.opts.date_format))
}

pub fn render_part(compiled: &CompiledPart, data: &Value, hooks: &dyn FormatHooks, ctx: &mut RenderCtx) -> Result<String> {
    if !compiled.has_tags {
        if let Some(Node::Raw(x)) = compiled.nodes.first() {
            return Ok(x.clone());
        }
    }
    let mut out = String::new();
    let scope = Scope::root(data.clone());
    render_nodes(&compiled.nodes, &scope, hooks, ctx, &mut out)?;
    Ok(out)
}

fn check_output(out: &str, ctx: &RenderCtx) -> Result<()> {
    let max = ctx.opts.limits.max_output_bytes;
    if exceeds(out.len() as u64, max) {
        return Err(Error::limit("maxOutputBytes", max).with_part(&ctx.part));
    }
    Ok(())
}

fn render_nodes(nodes: &[Node], scope: &Scope, hooks: &dyn FormatHooks, ctx: &mut RenderCtx, out: &mut String) -> Result<()> {
    for node in nodes {
        match node {
            Node::Raw(x) => out.push_str(x),
            Node::Text(v) => out.push_str(&escape_xml(v)),
            Node::Var { tag, br, tab } => {
                let mut value = evaluate(&tag.raw, &tag.expr, scope, ctx)?;
                if value.is_nullish() {
                    if ctx.opts.strict || ctx.opts.null_value == NullValue::Error {
                        return Err(Error::template(
                            ErrorCode::MissingValue,
                            format!("Missing value for \"{}\"", tag.expr.source),
                            &ctx.part,
                            Some(&tag.raw),
                        ));
                    }
                    value = match &ctx.opts.null_value {
                        NullValue::Empty | NullValue::Error => Value::str(""),
                        NullValue::Text(t) => Value::str(t),
                        NullValue::Keep => Value::str(&tag.raw),
                        NullValue::Host => {
                            ctx.count_host_call()?;
                            let info = TagInfo { raw: &tag.raw, expression: &tag.expr.source, part: &ctx.part };
                            ctx.host.null_value(&info).map_err(|e| host_failure(e, false, &ctx.part, &tag.raw))?
                        }
                    };
                }
                let text = if value.is_nullish() { String::new() } else { stringify(&value, tag, ctx)? };
                if let Some(custom) = hooks.render_scalar(&value, &text) {
                    out.push_str(&custom);
                    continue;
                }
                let mut xml = escape_xml(&text);
                if ctx.opts.linebreaks {
                    if let Some(br) = br {
                        xml = xml.replace("\r\n", "\n").replace('\r', "\n").replace('\n', br);
                    }
                    if let Some(tab) = tab {
                        xml = xml.replace('\t', tab);
                    }
                }
                out.push_str(&xml);
            }
            Node::Image { tag, shape_xml } => {
                let value = evaluate(&tag.raw, &tag.expr, scope, ctx)?;
                let empty = value.is_nullish() || matches!(value, Value::Bool(false)) || matches!(&value, Value::String(s) if s.is_empty());
                if empty {
                    if ctx.opts.strict && value.is_nullish() {
                        return Err(Error::template(
                            ErrorCode::MissingValue,
                            format!("Missing image for \"{}\"", tag.expr.source),
                            &ctx.part,
                            Some(&tag.raw),
                        ));
                    }
                    continue;
                }
                if !is_image_value(&value) {
                    return Err(Error::template(
                        ErrorCode::InvalidImage,
                        "Image tag expects an object like { data, width?, height?, extension? }",
                        &ctx.part,
                        Some(&tag.raw),
                    ));
                }
                let img = resolve_image(&value).map_err(|m| Error::template(ErrorCode::InvalidImage, m, &ctx.part, Some(&tag.raw)))?;
                let xml = hooks.render_image(&img, ctx, shape_xml.as_deref()).map_err(|e| e.with_part(&ctx.part).with_tag(&tag.raw))?;
                out.push_str(&xml);
            }
            Node::Section { tag, body, else_body } => render_section(tag, body, else_body.as_deref(), scope, hooks, ctx, out)?,
        }
        check_output(out, ctx)?;
    }
    Ok(())
}

fn render_section(
    tag: &OpenTag,
    body: &[Node],
    else_body: Option<&[Node]>,
    scope: &Scope,
    hooks: &dyn FormatHooks,
    ctx: &mut RenderCtx,
    out: &mut String,
) -> Result<()> {
    let value = evaluate(&tag.raw, &tag.expr, scope, ctx)?;
    let render_else = |ctx: &mut RenderCtx, out: &mut String| -> Result<()> {
        match else_body {
            Some(b) => render_nodes(b, scope, hooks, ctx, out),
            None => Ok(()),
        }
    };
    let run_loop = |items: Vec<Value>, keys: Option<Vec<String>>, ctx: &mut RenderCtx, out: &mut String| -> Result<()> {
        let len = items.len();
        for (index, item) in items.into_iter().enumerate() {
            ctx.loop_iterations += 1;
            let max = ctx.opts.limits.max_loop_iterations;
            if exceeds(ctx.loop_iterations, max) {
                return Err(Error::limit("maxLoopIterations", max).with_part(&ctx.part).with_tag(&tag.raw));
            }
            let mut locals: Vec<(&'static str, Value)> = vec![
                ("index", Value::num(index as f64)),
                ("number", Value::num(index as f64 + 1.0)),
                ("first", Value::Bool(index == 0)),
                ("last", Value::Bool(index + 1 == len)),
                ("length", Value::num(len as f64)),
            ];
            if let Some(keys) = &keys {
                locals.push(("key", Value::str(&keys[index])));
            }
            let child = scope.child(item, locals);
            render_nodes(body, &child, hooks, ctx, out)?;
        }
        Ok(())
    };

    match tag.mode {
        SectionMode::If => {
            if value.section_truthy() {
                render_nodes(body, scope, hooks, ctx, out)
            } else {
                render_else(ctx, out)
            }
        }
        SectionMode::Unless | SectionMode::Inverted => {
            if !value.section_truthy() {
                render_nodes(body, scope, hooks, ctx, out)
            } else {
                render_else(ctx, out)
            }
        }
        SectionMode::Each => match &value {
            Value::Array(a) if !a.is_empty() => run_loop(a.to_vec(), None, ctx, out),
            Value::Object(o) if !o.is_empty() => {
                let keys: Vec<String> = o.keys().cloned().collect();
                run_loop(o.values().cloned().collect(), Some(keys), ctx, out)
            }
            _ => render_else(ctx, out),
        },
        SectionMode::Section => match &value {
            Value::Array(a) => {
                if a.is_empty() {
                    render_else(ctx, out)
                } else {
                    run_loop(a.to_vec(), None, ctx, out)
                }
            }
            Value::Object(_) => {
                let child = scope.child(value.clone(), Vec::new());
                render_nodes(body, &child, hooks, ctx, out)
            }
            v if v.truthy() => render_nodes(body, scope, hooks, ctx, out),
            _ => render_else(ctx, out),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum TemplateTag {
    Variable {
        path: String,
        expression: String,
        filters: Vec<String>,
        raw: String,
    },
    Image {
        path: String,
        expression: String,
        raw: String,
    },
    Section {
        mode: SectionMode,
        path: String,
        expression: String,
        raw: String,
        children: Vec<TemplateTag>,
        else_children: Option<Vec<TemplateTag>>,
    },
}

pub fn inspect_nodes(nodes: &[Node]) -> Vec<TemplateTag> {
    let mut out = Vec::new();
    for n in nodes {
        match n {
            Node::Var { tag, .. } => out.push(TemplateTag::Variable {
                path: tag.expr.path.clone(),
                expression: tag.expr.source.clone(),
                filters: tag.expr.filters.clone(),
                raw: tag.raw.clone(),
            }),
            Node::Image { tag, .. } => {
                out.push(TemplateTag::Image { path: tag.expr.path.clone(), expression: tag.expr.source.clone(), raw: tag.raw.clone() })
            }
            Node::Section { tag, body, else_body } => out.push(TemplateTag::Section {
                mode: tag.mode,
                path: tag.expr.path.clone(),
                expression: tag.expr.source.clone(),
                raw: tag.raw.clone(),
                children: inspect_nodes(body),
                else_children: else_body.as_deref().map(inspect_nodes),
            }),
            _ => {}
        }
    }
    out
}
