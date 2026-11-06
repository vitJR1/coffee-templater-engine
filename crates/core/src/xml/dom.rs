//! Minimal mutable XML tree used by the document model (parse/build).

use super::escape::{decode_xml, escape_into};
use super::tokenizer::{parse_attributes, tokenize, TokKind};
use crate::error::{Error, Result};
use crate::limits::{exceeds, xml_depth};

#[derive(Debug, Clone, PartialEq)]
pub struct XmlElement {
    pub name: String,
    /// Decoded attribute values, in source order.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum XmlNode {
    Element(XmlElement),
    Text(String),
    /// Comments, processing instructions: kept verbatim.
    Other(String),
}

#[derive(Debug, Clone)]
pub struct XmlDocument {
    pub prolog: Vec<XmlNode>,
    pub root: XmlElement,
}

impl XmlElement {
    pub fn new(name: &str) -> Self {
        XmlElement { name: name.to_string(), attrs: Vec::new(), children: Vec::new() }
    }

    pub fn with_attrs(name: &str, attrs: &[(&str, &str)]) -> Self {
        XmlElement {
            name: name.to_string(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            children: Vec::new(),
        }
    }

    pub fn child(mut self, c: XmlElement) -> Self {
        self.children.push(XmlNode::Element(c));
        self
    }

    pub fn text_child(mut self, t: &str) -> Self {
        self.children.push(XmlNode::Text(t.to_string()));
        self
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn set_attr(&mut self, name: &str, value: &str) {
        if let Some(slot) = self.attrs.iter_mut().find(|(k, _)| k == name) {
            slot.1 = value.to_string();
        } else {
            self.attrs.push((name.to_string(), value.to_string()));
        }
    }

    pub fn remove_attr(&mut self, name: &str) {
        self.attrs.retain(|(k, _)| k != name);
    }

    pub fn elements(&self) -> impl Iterator<Item = &XmlElement> {
        self.children.iter().filter_map(|c| match c {
            XmlNode::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn elements_mut(&mut self) -> impl Iterator<Item = &mut XmlElement> {
        self.children.iter_mut().filter_map(|c| match c {
            XmlNode::Element(e) => Some(e),
            _ => None,
        })
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a XmlElement> + 'a {
        self.elements().filter(move |e| e.name == name)
    }

    pub fn first_child(&self, name: &str) -> Option<&XmlElement> {
        self.elements().find(|e| e.name == name)
    }

    pub fn first_child_mut(&mut self, name: &str) -> Option<&mut XmlElement> {
        self.elements_mut().find(|e| e.name == name)
    }

    /// Depth-first search for the first descendant with the given name.
    pub fn find_descendant(&self, name: &str) -> Option<&XmlElement> {
        for c in self.elements() {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = c.find_descendant(name) {
                return Some(f);
            }
        }
        None
    }

    pub fn find_descendant_mut(&mut self, name: &str) -> Option<&mut XmlElement> {
        for c in self.elements_mut() {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = c.find_descendant_mut(name) {
                return Some(f);
            }
        }
        None
    }

    pub fn find_descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a XmlElement>) {
        for c in self.elements() {
            if c.name == name {
                out.push(c);
            }
            c.find_descendants(name, out);
        }
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        serialize_element(&mut out, self);
        out
    }

    pub fn serialize_children(&self) -> String {
        let mut out = String::new();
        for c in &self.children {
            serialize_node(&mut out, c);
        }
        out
    }
}

pub fn parse_xml(xml: &str, max_depth: u64) -> Result<XmlDocument> {
    let tokens = tokenize(xml)?;
    let mut prolog = Vec::new();
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut root: Option<XmlElement> = None;

    let attach = |stack: &mut Vec<XmlElement>, root: &mut Option<XmlElement>, el: XmlElement| {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(XmlNode::Element(el));
        } else if root.is_none() {
            *root = Some(el);
        }
    };

    for tok in tokens {
        match tok.kind {
            TokKind::Open { self_closing } => {
                let attrs = parse_attributes(tok.raw).into_iter().map(|(k, v)| (k, decode_xml(&v).into_owned())).collect();
                let el = XmlElement { name: tok.name.to_string(), attrs, children: Vec::new() };
                if self_closing {
                    attach(&mut stack, &mut root, el);
                } else {
                    // An element after the root is parsed and then dropped by `attach`.
                    stack.push(el);
                    if exceeds(stack.len() as u64, xml_depth(max_depth)) {
                        return Err(Error::limit("maxXmlDepth", xml_depth(max_depth)));
                    }
                }
            }
            TokKind::Close => {
                let top = stack.pop().ok_or_else(|| Error::invalid_document(format!("Malformed XML: unexpected </{}>", tok.name)))?;
                if top.name != tok.name {
                    return Err(Error::invalid_document(format!("Malformed XML: unexpected </{}>", tok.name)));
                }
                attach(&mut stack, &mut root, top);
            }
            TokKind::Text => {
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(XmlNode::Text(decode_xml(tok.raw).into_owned()));
                }
            }
            TokKind::Other => {
                if let Some(parent) = stack.last_mut() {
                    if let Some(cdata) = tok.raw.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")) {
                        parent.children.push(XmlNode::Text(cdata.to_string()));
                    } else {
                        parent.children.push(XmlNode::Other(tok.raw.to_string()));
                    }
                } else if root.is_none() {
                    prolog.push(XmlNode::Other(tok.raw.to_string()));
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(Error::invalid_document(format!(
            "Malformed XML: <{}> is never closed",
            stack.last().map(|e| e.name.as_str()).unwrap_or("")
        )));
    }
    let root = root.ok_or_else(|| Error::invalid_document("Malformed XML: no root element"))?;
    Ok(XmlDocument { prolog, root })
}

pub fn serialize_xml(doc: &XmlDocument) -> String {
    let mut out = String::new();
    for n in &doc.prolog {
        serialize_node(&mut out, n);
    }
    serialize_element(&mut out, &doc.root);
    out
}

pub fn serialize_node(out: &mut String, node: &XmlNode) {
    match node {
        XmlNode::Text(t) => escape_into(out, t, false),
        XmlNode::Other(raw) => out.push_str(raw),
        XmlNode::Element(e) => serialize_element(out, e),
    }
}

fn serialize_element(out: &mut String, el: &XmlElement) {
    out.push('<');
    out.push_str(&el.name);
    for (k, v) in &el.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_into(out, v, true);
        out.push('"');
    }
    if el.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for c in &el.children {
        serialize_node(out, c);
    }
    out.push_str("</");
    out.push_str(&el.name);
    out.push('>');
}

pub fn serialize_nodes(nodes: &[XmlNode]) -> String {
    let mut out = String::new();
    for n in nodes {
        serialize_node(&mut out, n);
    }
    out
}

/// Parses an XML fragment (possibly several sibling elements).
pub fn parse_fragment(xml: &str, max_depth: u64) -> Result<Vec<XmlNode>> {
    if xml.is_empty() {
        return Ok(Vec::new());
    }
    let depth = xml_depth(max_depth) + 1;
    Ok(parse_xml(&format!("<__root>{xml}</__root>"), depth)?.root.children)
}

/// Concatenated text of all descendant text nodes.
pub fn text_content(node: &XmlNode) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

pub fn element_text(el: &XmlElement) -> String {
    let mut out = String::new();
    for c in &el.children {
        collect_text(c, &mut out);
    }
    out
}

fn collect_text(node: &XmlNode, out: &mut String) {
    match node {
        XmlNode::Text(t) => out.push_str(t),
        XmlNode::Other(_) => {}
        XmlNode::Element(e) => {
            for c in &e.children {
                collect_text(c, out);
            }
        }
    }
}

/// Sorts element children according to a schema order list; unknown names keep relative order at
/// the end.
pub fn sort_children(children: Vec<XmlNode>, order: &[&str]) -> Vec<XmlNode> {
    let rank = |n: &XmlNode| match n {
        XmlNode::Element(e) => order.iter().position(|o| *o == e.name).unwrap_or(order.len()),
        _ => order.len(),
    };
    let mut indexed: Vec<(usize, XmlNode)> = children.into_iter().map(|n| (rank(&n), n)).collect();
    indexed.sort_by_key(|(r, _)| *r); // stable
    indexed.into_iter().map(|(_, n)| n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_serialize() {
        let doc = parse_xml(r#"<?xml version="1.0"?><a x="&amp;1"><b>t &lt; u</b><c/><![CDATA[<raw>]]><!--c--></a>"#, 256).unwrap();
        assert_eq!(doc.root.attr("x"), Some("&1"));
        assert_eq!(serialize_xml(&doc), r#"<?xml version="1.0"?><a x="&amp;1"><b>t &lt; u</b><c/>&lt;raw&gt;<!--c--></a>"#);
        assert_eq!(text_content(&XmlNode::Element(doc.root.clone())), "t < u<raw>");
    }

    #[test]
    fn depth_limit_and_errors() {
        let deep = format!("{}{}", "<a>".repeat(10), "</a>".repeat(10));
        assert!(parse_xml(&deep, 5).is_err());
        assert!(parse_xml(&deep, 10).is_ok());
        assert!(parse_xml("<a><b></a>", 0).is_err());
        assert!(parse_xml("<a>", 0).is_err());
    }
}
