//! Finds tags in a sequence of text segments that belong to one paragraph/cell.
//!
//! Office splits text into runs arbitrarily (spell-check, formatting, editing history), so
//! `{{name}}` may arrive as `{{na` + `me}}`. The concatenated text is scanned, and every tag is
//! moved entirely into the segment where it starts — it inherits that run's formatting. Escaped
//! delimiters are unescaped.

use super::options::Options;
use crate::error::{Error, ErrorCode};

#[derive(Debug, Clone, PartialEq)]
pub enum LexPart {
    Text(String),
    Tag { raw: String, inner: String },
}

struct Edit {
    from: usize,
    to: usize,
    tag: Option<(String, String)>,
}

fn head(s: &str, from: usize, chars: usize) -> String {
    s[from..].chars().take(chars).collect()
}

pub fn lex_segments(segments: &[String], opts: &Options, part: &str) -> Result<Vec<Vec<LexPart>>, Error> {
    let full: String = segments.concat();
    let (start, end) = (opts.start.as_str(), opts.end.as_str());
    let esc = opts.escape_char.as_deref();
    let fb = full.as_bytes();
    let starts_with_at = |pos: usize, s: &str| fb.get(pos..pos + s.len()) == Some(s.as_bytes());

    let mut edits: Vec<Edit> = Vec::new();
    let mut i = 0;
    while i < full.len() {
        let Some(next) = full[i..].find(start).map(|p| p + i) else { break };
        if let Some(e) = esc {
            if next >= e.len() && starts_with_at(next - e.len(), e) {
                let esc_start = next - e.len();
                let doubled = esc_start >= e.len() && starts_with_at(esc_start - e.len(), e);
                if !doubled {
                    // `\{{` → literal `{{`: drop the escape character
                    edits.push(Edit { from: esc_start, to: next, tag: None });
                    i = next + start.len();
                    continue;
                }
                // `\\{{tag}}` → literal `\` followed by a real tag: drop one escape character
                edits.push(Edit { from: esc_start, to: next, tag: None });
            }
        }
        let after = next + start.len();
        let Some(close) = full[after..].find(end).map(|p| p + after) else {
            return Err(Error::template(
                ErrorCode::UnclosedTag,
                format!("Unclosed tag: missing \"{end}\""),
                part,
                Some(&head(&full, next, 40)),
            ));
        };
        let inner = &full[after..close];
        if let Some(nested) = inner.find(start) {
            let tag = &full[next..after + nested];
            return Err(Error::template(ErrorCode::UnclosedTag, format!("Unclosed tag: missing \"{end}\""), part, Some(tag)));
        }
        let to = close + end.len();
        edits.push(Edit { from: next, to, tag: Some((full[next..to].to_string(), inner.to_string())) });
        i = to;
    }

    if edits.is_empty() {
        return Ok(segments.iter().map(|s| vec![LexPart::Text(s.clone())]).collect());
    }

    let mut result = Vec::with_capacity(segments.len());
    let mut offset = 0;
    let mut e = 0;
    for seg in segments {
        let seg_start = offset;
        let seg_end = offset + seg.len();
        let mut parts: Vec<LexPart> = Vec::new();
        let mut cursor = seg_start;
        // skip edits that ended before this segment
        while e < edits.len() && edits[e].to <= seg_start && !(edits[e].tag.is_some() && edits[e].from >= seg_start) {
            e += 1;
        }
        let mut k = e;
        while k < edits.len() && edits[k].from < seg_end {
            let ed = &edits[k];
            if ed.from > cursor {
                push_text(&mut parts, &full[cursor..ed.from]);
            }
            if let Some((raw, inner)) = &ed.tag {
                if ed.from >= seg_start {
                    parts.push(LexPart::Tag { raw: raw.clone(), inner: inner.clone() });
                }
            }
            cursor = cursor.max(seg_end.min(ed.to));
            k += 1;
        }
        if cursor < seg_end {
            push_text(&mut parts, &full[cursor..seg_end]);
        }
        result.push(parts);
        offset = seg_end;
    }
    Ok(result)
}

fn push_text(parts: &mut Vec<LexPart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(LexPart::Text(last)) = parts.last_mut() {
        last.push_str(value);
    } else {
        parts.push(LexPart::Text(value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(segs: &[&str]) -> Vec<Vec<LexPart>> {
        let segs: Vec<String> = segs.iter().map(|s| s.to_string()).collect();
        lex_segments(&segs, &Options::default(), "p").unwrap()
    }

    #[test]
    fn moves_split_tags_into_first_segment() {
        let r = lex(&["Hello, ", "{{na", "me}}", "!"]);
        assert_eq!(r[0], vec![LexPart::Text("Hello, ".into())]);
        assert_eq!(r[1], vec![LexPart::Tag { raw: "{{name}}".into(), inner: "name".into() }]);
        assert_eq!(r[2], vec![]);
        assert_eq!(r[3], vec![LexPart::Text("!".into())]);
    }

    #[test]
    fn escapes() {
        let r = lex(&["\\{{name}} = {{name}}"]);
        assert_eq!(r[0][0], LexPart::Text("{{name}} = ".into()));
        let r = lex(&["C:\\\\{{dir}}"]);
        assert_eq!(r[0][0], LexPart::Text("C:\\".into()));
    }

    #[test]
    fn unclosed() {
        let segs = vec!["{{name".to_string()];
        assert_eq!(lex_segments(&segs, &Options::default(), "p").unwrap_err().code, ErrorCode::UnclosedTag);
        let segs = vec!["{{a {{b}}".to_string()];
        assert_eq!(lex_segments(&segs, &Options::default(), "p").unwrap_err().tag.as_deref(), Some("{{a "));
    }
}
