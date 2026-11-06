//! A tiny, lossless XML tokenizer: joining `raw` of every token reproduces the input exactly.
//! It is intentionally forgiving: OOXML produced by Office is well-formed, and we never need
//! validation.

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Open {
        self_closing: bool,
    },
    Close,
    Text,
    /// `<?xml?>`, comments, CDATA, doctype.
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct XmlToken<'a> {
    pub kind: TokKind,
    pub raw: &'a str,
    /// Element name for open/close tokens, empty otherwise.
    pub name: &'a str,
}

pub fn tokenize<'a>(xml: &'a str) -> Result<Vec<XmlToken<'a>>> {
    let mut tokens = Vec::new();
    let b = xml.as_bytes();
    let len = b.len();
    let mut i = 0;
    let other = |from: usize, needle: &str, tokens: &mut Vec<XmlToken<'a>>| -> Result<usize> {
        let end = xml[from..]
            .find(needle)
            .map(|p| p + from)
            .ok_or_else(|| Error::invalid_document(format!("Malformed XML: missing \"{needle}\" after offset {from}")))?;
        let to = end + needle.len();
        tokens.push(XmlToken { kind: TokKind::Other, raw: &xml[from..to], name: "" });
        Ok(to)
    };
    while i < len {
        let Some(lt) = xml[i..].find('<').map(|p| p + i) else {
            tokens.push(XmlToken { kind: TokKind::Text, raw: &xml[i..], name: "" });
            break;
        };
        if lt > i {
            tokens.push(XmlToken { kind: TokKind::Text, raw: &xml[i..lt], name: "" });
        }
        let rest = &xml[lt..];
        if rest.starts_with("<!--") {
            i = other(lt, "-->", &mut tokens)?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = other(lt, "]]>", &mut tokens)?;
            continue;
        }
        if rest.starts_with("<?") {
            i = other(lt, "?>", &mut tokens)?;
            continue;
        }
        if rest.starts_with("<!") {
            i = other(lt, ">", &mut tokens)?;
            continue;
        }
        // Element tag: scan to the closing '>' while respecting quoted attribute values.
        let mut j = lt + 1;
        let mut quote = 0u8;
        while j < len {
            let ch = b[j];
            if quote != 0 {
                if ch == quote {
                    quote = 0;
                }
            } else if ch == b'"' || ch == b'\'' {
                quote = ch;
            } else if ch == b'>' {
                break;
            }
            j += 1;
        }
        if j >= len {
            return Err(Error::invalid_document(format!("Malformed XML: unterminated tag at offset {lt}")));
        }
        let raw = &xml[lt..=j];
        if raw.as_bytes().get(1) == Some(&b'/') {
            tokens.push(XmlToken { kind: TokKind::Close, raw, name: raw[2..raw.len() - 1].trim() });
        } else {
            let self_closing = raw.len() >= 3 && raw.as_bytes()[raw.len() - 2] == b'/';
            let name_end = raw[1..].find(|c: char| c.is_whitespace() || c == '/' || c == '>').map(|p| p + 1).unwrap_or(raw.len());
            tokens.push(XmlToken { kind: TokKind::Open { self_closing }, raw, name: &raw[1..name_end] });
        }
        i = j + 1;
    }
    Ok(tokens)
}

/// Parses attributes from a raw open tag such as `<w:t xml:space="preserve">`.
/// Values are returned raw (still escaped).
pub fn parse_attributes(raw: &str) -> Vec<(String, String)> {
    let mut attrs: Vec<(String, String)> = Vec::new();
    let Some(start) = raw.find(char::is_whitespace) else { return attrs };
    let body = &raw[start..];
    let b = body.as_bytes();
    let is_name = |c: u8| !(c.is_ascii_whitespace() || matches!(c, b'=' | b'/' | b'<' | b'>'));
    let mut i = 0;
    while i < b.len() {
        if !is_name(b[i]) {
            i += 1;
            continue;
        }
        let ns = i;
        while i < b.len() && is_name(b[i]) {
            i += 1;
        }
        let name = &body[ns..i];
        let mut k = i;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= b.len() || b[k] != b'=' {
            continue;
        }
        k += 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= b.len() || (b[k] != b'"' && b[k] != b'\'') {
            continue;
        }
        let q = b[k];
        let vs = k + 1;
        let Some(ve) = body[vs..].find(q as char).map(|p| p + vs) else { continue };
        let value = body[vs..ve].to_string();
        if let Some(slot) = attrs.iter_mut().find(|(n, _)| n == name) {
            slot.1 = value;
        } else {
            attrs.push((name.to_string(), value));
        }
        i = ve + 1;
    }
    attrs
}

/// Finds `\s{name}\s*=\s*("..."|'...')` in a raw tag. Returns (start of whitespace, start of value
/// quote, end after closing quote).
fn find_attr(raw: &str, name: &str) -> Option<(usize, usize, usize)> {
    let b = raw.as_bytes();
    let mut from = 0;
    while let Some(p) = raw[from..].find(name).map(|p| p + from) {
        from = p + 1;
        if p == 0 || !b[p - 1].is_ascii_whitespace() {
            continue;
        }
        let mut k = p + name.len();
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= b.len() || b[k] != b'=' {
            continue;
        }
        k += 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= b.len() || (b[k] != b'"' && b[k] != b'\'') {
            continue;
        }
        let q = b[k] as char;
        let Some(end) = raw[k + 1..].find(q).map(|e| e + k + 1) else { continue };
        return Some((p - 1, k, end + 1));
    }
    None
}

/// Raw (escaped) value of attribute `name` in a raw open tag.
pub fn raw_attr<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    find_attr(raw, name).map(|(_, q, end)| &raw[q + 1..end - 1])
}

/// Returns a copy of a raw open tag with attribute `name` set (escaped value must be provided).
pub fn set_raw_attribute(raw: &str, name: &str, escaped_value: &str) -> String {
    if let Some((_, q, end)) = find_attr(raw, name) {
        return format!("{}\"{}\"{}", &raw[..q], escaped_value, &raw[end..]);
    }
    let self_closing = raw.ends_with("/>");
    let head = &raw[..raw.len() - if self_closing { 2 } else { 1 }];
    let head = head.trim_end();
    format!("{head} {name}=\"{escaped_value}\"{}", if self_closing { "/>" } else { ">" })
}

/// Removes attribute `name` from a raw open tag.
pub fn remove_raw_attribute(raw: &str, name: &str) -> String {
    match find_attr(raw, name) {
        Some((ws, _, end)) => format!("{}{}", &raw[..ws], &raw[end..]),
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless() {
        let xml = r#"<?xml version="1.0"?><a x="1>2"><!-- c --><b/>text<![CDATA[<x>]]></a>"#;
        let toks = tokenize(xml).unwrap();
        assert_eq!(toks.iter().map(|t| t.raw).collect::<String>(), xml);
        assert_eq!(toks[1].name, "a");
        assert!(matches!(toks[3].kind, TokKind::Open { self_closing: true }));
    }

    #[test]
    fn attributes() {
        let a = parse_attributes(r#"<w:t xml:space="preserve" a='1' b = "x y" bogus c="2">"#);
        assert_eq!(
            a,
            vec![("xml:space".into(), "preserve".into()), ("a".into(), "1".into()), ("b".into(), "x y".into()), ("c".into(), "2".into())]
        );
        assert_eq!(set_raw_attribute("<w:t>", "xml:space", "preserve"), r#"<w:t xml:space="preserve">"#);
        assert_eq!(set_raw_attribute(r#"<c r="A1" t="s"/>"#, "t", "inlineStr"), r#"<c r="A1" t="inlineStr"/>"#);
        assert_eq!(set_raw_attribute(r#"<c r="A1" />"#, "t", "b"), r#"<c r="A1" t="b"/>"#);
        assert_eq!(remove_raw_attribute(r#"<c r="A1" t="s">"#, "t"), r#"<c r="A1">"#);
        assert_eq!(raw_attr(r#"<row r="3" spans="1:2">"#, "r"), Some("3"));
        assert_eq!(raw_attr(r#"<row spans="1:2">"#, "r"), None);
    }
}
