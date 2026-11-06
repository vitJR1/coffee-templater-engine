//! Properties elements (`w:rPr`, `a:pPr`, ...) are kept verbatim in the model as `props` XML, and
//! the convenient fields (bold, size, ...) are read from them. When building, a field is written
//! back only if it differs from the value in `props`, so everything we don't model survives a
//! round trip.

use crate::error::{Error, Result};
use crate::xml::dom::{parse_fragment, sort_children, XmlElement, XmlNode};
use serde_json::{Map, Value as SJson};

pub type ReadChild = fn(&XmlElement) -> Option<SJson>;
pub type WriteChild = fn(&SJson) -> Vec<XmlElement>;
pub type ReadAttr = fn(&str) -> Option<SJson>;
pub type WriteAttr = fn(&SJson) -> Option<String>;

pub enum Field {
    /// `<w:b/>` = true, `<w:b w:val="0"/>` = false.
    OnOff { name: &'static str },
    /// `<w:pStyle w:val="Heading1"/>`.
    Val { name: &'static str },
    /// A child element (or group of elements), the first name is used for reading.
    Child { names: &'static [&'static str], read: ReadChild, write: WriteChild },
    /// An attribute of the properties element.
    Attr { attr: &'static str, read: ReadAttr, write: WriteAttr },
}

pub type FieldMap = &'static [(&'static str, Field)];

const OFF: [&str; 4] = ["0", "false", "off", "none"];

impl Field {
    fn names(&self) -> &[&'static str] {
        match self {
            Field::OnOff { name } | Field::Val { name } => std::slice::from_ref(name),
            Field::Child { names, .. } => names,
            Field::Attr { .. } => &[],
        }
    }
}

pub fn read_fields(props: Option<&XmlElement>, fields: FieldMap) -> Map<String, SJson> {
    let mut out = Map::new();
    let Some(props) = props else { return out };
    for (key, f) in fields {
        let v = match f {
            Field::Attr { attr, read, .. } => props.attr(attr).and_then(read),
            _ => {
                let first = f.names()[0];
                props.elements().find(|c| c.name == first).and_then(|c| match f {
                    Field::OnOff { .. } => Some(SJson::Bool(!OFF.contains(&c.attr("w:val").unwrap_or("true")))),
                    Field::Val { .. } => c.attr("w:val").map(|v| SJson::String(v.to_string())),
                    Field::Child { read, .. } => read(c),
                    Field::Attr { .. } => None,
                })
            }
        };
        if let Some(v) = v {
            if !v.is_null() {
                out.insert(key.to_string(), v);
            }
        }
    }
    out
}

/// Structural JSON equality where `14` equals `14.0`.
pub fn json_same(a: Option<&SJson>, b: Option<&SJson>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(SJson::Number(x)), Some(SJson::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(SJson::Array(x)), Some(SJson::Array(y))) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_same(Some(p), Some(q))),
        (Some(SJson::Object(x)), Some(SJson::Object(y))) => x.len() == y.len() && x.iter().all(|(k, v)| json_same(Some(v), y.get(k))),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Builds the properties element from original `props` XML + model fields. Returns "" when empty.
pub fn write_props(
    name: &str,
    props_xml: Option<&str>,
    model: &Map<String, SJson>,
    fields: FieldMap,
    order: &[&str],
    max_depth: u64,
) -> Result<String> {
    let parsed = match props_xml {
        Some(x) if !x.is_empty() => parse_fragment(x, max_depth)
            .map_err(|e| Error::invalid_model(format!("Invalid props XML: {}", e.message)))?
            .into_iter()
            .find_map(|n| match n {
                XmlNode::Element(e) => Some(e),
                _ => None,
            }),
        _ => None,
    };
    let mut el = parsed.unwrap_or_else(|| XmlElement::new(name));
    el.name = name.to_string();
    let original = read_fields(Some(&el), fields);

    for (key, f) in fields {
        let value = model.get(*key).filter(|v| !v.is_null());
        if json_same(value, original.get(*key)) {
            continue;
        }
        match f {
            Field::Attr { attr, write, .. } => match value.and_then(write) {
                Some(v) => el.set_attr(attr, &v),
                None => el.remove_attr(attr),
            },
            _ => {
                let names = f.names();
                el.children.retain(|c| !matches!(c, XmlNode::Element(e) if names.contains(&e.name.as_str())));
                if let Some(v) = value {
                    let new: Vec<XmlElement> = match f {
                        Field::OnOff { name } => {
                            if v.as_bool().unwrap_or(false) {
                                vec![XmlElement::new(name)]
                            } else {
                                vec![XmlElement::with_attrs(name, &[("w:val", "0")])]
                            }
                        }
                        Field::Val { name } => vec![XmlElement::with_attrs(name, &[("w:val", &json_text(v))])],
                        Field::Child { write, .. } => write(v),
                        Field::Attr { .. } => Vec::new(),
                    };
                    el.children.extend(new.into_iter().map(XmlNode::Element));
                }
            }
        }
    }
    el.children = sort_children(std::mem::take(&mut el.children), order);
    if el.children.is_empty() && el.attrs.is_empty() {
        return Ok(String::new());
    }
    Ok(el.serialize())
}

/// `String(v)` of a JSON scalar.
pub fn json_text(v: &SJson) -> String {
    match v {
        SJson::String(s) => s.clone(),
        SJson::Number(n) => n.as_f64().map(crate::num::js_to_string).unwrap_or_else(|| n.to_string()),
        SJson::Bool(b) => b.to_string(),
        SJson::Null => "null".into(),
        other => other.to_string(),
    }
}

pub fn json_f64(v: &SJson) -> Option<f64> {
    match v {
        SJson::Number(n) => n.as_f64(),
        SJson::String(s) => Some(crate::num::js_string_to_number(s)).filter(|x| !x.is_nan()),
        _ => None,
    }
}
