//! XML escaping. Only the five predefined entities and numeric references are decoded;
//! DTD-defined entities are never expanded (spec §10).

use std::borrow::Cow;

/// Decodes `&lt;` `&gt;` `&amp;` `&quot;` `&apos;`, `&#NN;` and `&#xHH;`. Anything else is kept.
pub fn decode_xml(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        match decode_entity(tail) {
            Some((s, len)) => {
                out.push_str(&s);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// `tail` starts with `&`. Returns the decoded text and the length of the entity.
fn decode_entity(tail: &str) -> Option<(Cow<'static, str>, usize)> {
    let semi = tail[1..].find(';')? + 1;
    let body = &tail[1..semi];
    let len = semi + 1;
    if let Some(num) = body.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix('x') {
            Some(h) => (h, 16),
            None => (num, 10),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        let code = u32::from_str_radix(digits, radix).ok()?;
        let c = char::from_u32(code)?;
        return Some((Cow::Owned(c.to_string()), len));
    }
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let s = match body {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "quot" => "\"",
        "apos" => "'",
        _ => return None,
    };
    Some((Cow::Borrowed(s), len))
}

/// Characters that are not allowed in XML 1.0 documents at all.
#[inline]
fn invalid_xml_char(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}')
}

/// Escapes text for use as XML character data. Strips characters forbidden in XML 1.0.
pub fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    escape_into(&mut out, text, false);
    out
}

/// Escapes text for use inside a double-quoted XML attribute.
pub fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    escape_into(&mut out, text, true);
    out
}

pub fn escape_into(out: &mut String, text: &str, attr: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            c if invalid_xml_char(c) => {}
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        assert_eq!(decode_xml("a &lt;b&gt; &amp;amp; &#65;&#x42; &nbsp; &#xZZ; &"), "a <b> &amp; AB &nbsp; &#xZZ; &");
        assert_eq!(escape_xml("<b> & \"q\"\u{1}"), "&lt;b&gt; &amp; \"q\"");
        assert_eq!(escape_attr("\"x\""), "&quot;x&quot;");
    }
}
