//! XLSX adapter: inlines shared strings that hold tags, keeps numeric/boolean values typed, and
//! renumbers rows (with formulas, merges, validations, hyperlinks) after row loops.

use crate::opc::{rel, Package};
use crate::template::engine::{FormatHooks, FormatSpec, ImageMode};
use crate::value::{number_text, Value};
use crate::xml::dom::{element_text, parse_xml, XmlNode};
use crate::xml::escape::{decode_xml, escape_xml};
use crate::xml::tokenizer::{raw_attr, remove_raw_attribute, set_raw_attribute};
use std::collections::{BTreeSet, HashMap};

const GAP_ATTR: &str = "data-ct-gap";
const MARK: char = '\u{0}';
const MARK_SEP: char = '\u{1}';

pub static SPEC: FormatSpec = FormatSpec {
    text_tags: &["t"],
    group_tags: &["c"],
    paragraph_tag: None,
    row_tags: &["row"],
    content_tags: &[],
    preserve_space: true,
    image_mode: ImageMode::None,
    shape_tag: None,
};

#[derive(Debug, Clone)]
pub struct SharedString {
    /// Inner XML of `<si>`.
    pub xml: String,
    pub text: String,
}

pub fn workbook_path(pkg: &Package) -> String {
    pkg.main_document_path().unwrap_or_else(|| "xl/workbook.xml".into())
}

pub fn shared_strings_path(pkg: &Package, workbook: &str) -> Option<String> {
    pkg.related_parts(workbook, rel::SHARED_STRINGS).into_iter().next().map(|(_, p)| p)
}

pub fn read_shared_strings(pkg: &Package, workbook: &str) -> Vec<SharedString> {
    let Some(xml) = shared_strings_path(pkg, workbook).and_then(|p| pkg.get_text(&p)) else { return Vec::new() };
    let Ok(doc) = parse_xml(&xml, pkg.max_xml_depth) else { return Vec::new() };
    doc.root
        .children_named("si")
        .map(|si| SharedString {
            xml: si.serialize_children(),
            // phonetic runs (rPh) are not part of the visible text
            text: si
                .children
                .iter()
                .filter(|c| !matches!(c, XmlNode::Element(e) if e.name == "rPh"))
                .map(crate::xml::dom::text_content)
                .collect(),
        })
        .collect()
}

pub struct SheetRef {
    pub name: String,
    pub path: String,
    pub r_id: String,
}

pub fn worksheet_parts(pkg: &Package) -> Vec<SheetRef> {
    let wb = workbook_path(pkg);
    let Some(xml) = pkg.get_text(&wb) else { return Vec::new() };
    let Ok(doc) = parse_xml(&xml, pkg.max_xml_depth) else { return Vec::new() };
    let rels: Vec<(String, String)> = pkg.related_parts(&wb, rel::WORKSHEET).into_iter().map(|(r, p)| (r.id, p)).collect();
    let Some(sheets) = doc.root.first_child("sheets") else { return Vec::new() };
    sheets
        .children_named("sheet")
        .filter_map(|s| {
            let r_id = s.attr("r:id").unwrap_or_default().to_string();
            let path = rels.iter().find(|(id, _)| *id == r_id).map(|(_, p)| p.clone())?;
            if path.is_empty() || !pkg.has(&path) {
                return None;
            }
            Some(SheetRef { name: s.attr("name").unwrap_or_default().to_string(), path, r_id })
        })
        .collect()
}

pub fn template_parts(pkg: &Package) -> Vec<String> {
    worksheet_parts(pkg).into_iter().map(|s| s.path).collect()
}

pub struct Xlsx;

impl FormatHooks for Xlsx {
    fn spec(&self) -> &'static FormatSpec {
        &SPEC
    }

    fn render_scalar(&self, value: &Value, text: &str) -> Option<String> {
        let (kind, v) = match value {
            Value::Number(n) if n.value.is_finite() => ('N', number_text(n)),
            Value::Decimal(d) => ('N', crate::num::decimal_plain(d)?),
            Value::Bool(b) => ('B', if *b { "1".into() } else { "0".into() }),
            _ => return None,
        };
        Some(format!("{MARK}{kind}{v}{MARK_SEP}{}{MARK}", escape_xml(text)))
    }
}

/// State computed while compiling, used when rendering.
#[derive(Debug, Clone, Default)]
pub struct XlsxState {
    pub shared: Option<Vec<SharedString>>,
    /// Row numbers present in each template sheet.
    pub template_rows: HashMap<String, BTreeSet<i64>>,
}

fn tag_attr(attrs: &str, name: &str) -> Option<String> {
    raw_attr(&format!("<x{attrs}>"), name).map(str::to_string)
}

pub fn preprocess(pkg: &Package, part: &str, xml: &str, state: &mut XlsxState, delimiter_start: &str) -> String {
    let shared = state.shared.get_or_insert_with(|| read_shared_strings(pkg, &workbook_path(pkg)));
    // 1. Inline shared strings that contain tags, so each cell owns its text.
    let xml = crate::re!(r"<c\b([^>]*?)(?:/>|>([\s\S]*?)</c>)").replace_all(xml, |c: &regex_lite::Captures| {
        let whole = c[0].to_string();
        let attrs = &c[1];
        let Some(inner) = c.get(2).map(|m| m.as_str()) else { return whole };
        if inner.is_empty() || tag_attr(attrs, "t").as_deref() != Some("s") {
            return whole;
        }
        let idx = crate::re!(r"<v>\s*(\d+)\s*</v>").captures(inner).and_then(|m| m[1].parse::<usize>().ok());
        let Some(si) = idx.and_then(|i| shared.get(i)) else { return whole };
        if !si.text.contains(delimiter_start) {
            return whole;
        }
        let head = set_raw_attribute(&format!("<c{attrs}>"), "t", "inlineStr");
        let rest = crate::re!(r"<v>[\s\S]*?</v>").replacen(inner, 1, "");
        format!("{head}{rest}<is>{}</is></c>", si.xml)
    });
    // 2. Remember the gap between consecutive rows so we can renumber after loops.
    let mut rows = BTreeSet::new();
    let mut prev = 0i64;
    let xml = crate::re!(r"<row\b[^>]*>").replace_all(&xml, |c: &regex_lite::Captures| {
        let tag = &c[0];
        let r = raw_attr(tag, "r").and_then(|v| v.parse::<i64>().ok()).unwrap_or(prev + 1);
        rows.insert(r);
        let gap = r - prev;
        prev = r;
        set_raw_attribute(tag, GAP_ATTR, &gap.to_string())
    });
    state.template_rows.insert(part.to_string(), rows);
    xml.into_owned()
}

pub fn postprocess(xml: &str, part: &str, state: &XlsxState) -> String {
    let xml = resolve_scalar_markers(xml);
    let empty = BTreeSet::new();
    renumber_rows(&xml, state.template_rows.get(part).unwrap_or(&empty))
}

pub fn finalize(pkg: &mut Package) {
    // Cached calculation order is invalid after rows move: drop it and ask Excel to recalculate.
    let wb = workbook_path(pkg);
    for (_, path) in pkg.related_parts(&wb, rel::CALC_CHAIN) {
        pkg.remove(&path);
        pkg.remove_override(&path);
    }
    pkg.remove_relationships(&wb, |r| r.rel_type == rel::CALC_CHAIN);
    let Some(xml) = pkg.get_text(&wb) else { return };
    let next = if let Some(m) = crate::re!(r"<calcPr\b[^>]*?/?>").find(&xml) {
        format!("{}{}{}", &xml[..m.start()], set_raw_attribute(m.as_str(), "fullCalcOnLoad", "1"), &xml[m.end()..])
    } else if xml.contains("</workbook>") {
        // calcPr must precede these elements
        let before = crate::re!(
            r"<(oleSize|customWorkbookViews|pivotCaches|smartTagPr|smartTagTypes|webPublishing|fileRecoveryPr|webPublishObjects|extLst)\b"
        );
        match before.find(&xml) {
            Some(m) => format!("{}<calcPr fullCalcOnLoad=\"1\"/>{}", &xml[..m.start()], &xml[m.start()..]),
            None => xml.replacen("</workbook>", "<calcPr fullCalcOnLoad=\"1\"/></workbook>", 1),
        }
    } else {
        xml
    };
    pkg.set_text(&wb, &next);
}

/// Turns cells that consist of a single numeric/boolean value into real typed cells.
fn resolve_scalar_markers(xml: &str) -> String {
    if !xml.contains(MARK) {
        return xml.to_string();
    }
    let cell =
        crate::re!(r"<c\b([^>]*?)>(\s*<f\b[^>]*>[^<]*</f>)?\s*<is>\s*<t\b[^>]*>\x00([NB])([^\x01]*)\x01[^\x00]*\x00</t>\s*</is>\s*</c>");
    let xml = cell.replace_all(xml, |c: &regex_lite::Captures| {
        let mut head = remove_raw_attribute(&format!("<c{}>", &c[1]), "t");
        if &c[3] == "B" {
            head = set_raw_attribute(&head, "t", "b");
        }
        format!("{head}<v>{}</v></c>", &c[4])
    });
    // Values mixed with other text stay strings.
    crate::re!(r"\x00[NB][^\x01]*\x01([^\x00]*)\x00").replace_all(&xml, "$1").into_owned()
}

// ------------------------------------------------------------------------------------------
// Row renumbering
// ------------------------------------------------------------------------------------------

fn strip_gaps(xml: &str) -> String {
    crate::re!(r#"\sdata-ct-gap="\d+""#).replace_all(xml, "").into_owned()
}

struct RowMap<'a> {
    map: HashMap<i64, Vec<i64>>,
    sorted: Vec<i64>,
    template_rows: &'a BTreeSet<i64>,
}

impl RowMap<'_> {
    /// Maps an original row reference to its new number. `end` = last copy (for range ends).
    fn row(&self, r: i64, end: bool, own: Option<(i64, i64)>) -> Option<i64> {
        if let Some((o, n)) = own {
            if r == o {
                return Some(n);
            }
        }
        if let Some(list) = self.map.get(&r) {
            return Some(if end { *list.last()? } else { list[0] });
        }
        if self.template_rows.contains(&r) {
            return None; // the row was removed by an empty loop
        }
        // A row that is not in the template (empty row): shift by the delta of the closest row above.
        let base = self.sorted.iter().take_while(|o| **o < r).last();
        match base {
            None => Some(r),
            Some(b) => {
                let l = &self.map[b];
                Some(r + (l[l.len() - 1] - b))
            }
        }
    }

    fn cell_ref(&self, s: &str, end: bool, own: Option<(i64, i64)>) -> String {
        crate::re!(r"(\$?)([A-Z]{1,3})(\$?)(\d+)")
            .replace_all(s, |c: &regex_lite::Captures| {
                let n = c[4].parse::<i64>().ok().and_then(|r| self.row(r, end, own));
                match n {
                    Some(n) => format!("{}{}{}{}", &c[1], &c[2], &c[3], n),
                    None => c[0].to_string(),
                }
            })
            .into_owned()
    }

    fn range(&self, r: &str, own: Option<(i64, i64)>) -> String {
        match r.split_once(':') {
            None => self.cell_ref(r, false, own),
            Some((a, b)) => format!("{}:{}", self.cell_ref(a, false, own), self.cell_ref(b, true, own)),
        }
    }

    fn formula(&self, f: &str, own: (i64, i64)) -> String {
        map_formula(f, &|a, end| self.cell_ref(a, end, Some(own)))
    }
}

/// Parses `\$?[A-Z]{1,3}\$?\d+` at `i`, returns the end offset.
fn scan_ref(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i;
    if b.get(k) == Some(&b'$') {
        k += 1;
    }
    let ls = k;
    while k < b.len() && b[k].is_ascii_uppercase() {
        k += 1;
    }
    if k == ls || k - ls > 3 {
        return None;
    }
    if b.get(k) == Some(&b'$') {
        k += 1;
    }
    let ds = k;
    while k < b.len() && b[k].is_ascii_digit() {
        k += 1;
    }
    if k == ds {
        return None;
    }
    Some(k)
}

fn ref_boundary_ok(b: &[u8], end: usize) -> bool {
    !matches!(b.get(end), Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'(')
}

/// Rewrites cell references of a formula, leaving string literals and references to other sheets
/// (`Sheet2!A1`, `'My sheet'!A1`) untouched.
pub fn map_formula(f: &str, map: &dyn Fn(&str, bool) -> String) -> String {
    let b = f.as_bytes();
    let mut out = String::with_capacity(f.len());
    let mut seg = 0;
    let mut i = 0;
    while i < b.len() {
        let skip_to = match b[i] {
            b'"' => {
                let mut j = i + 1;
                let mut end = None;
                while j < b.len() {
                    if b[j] == b'"' {
                        if b.get(j + 1) == Some(&b'"') {
                            j += 2;
                            continue;
                        }
                        end = Some(j + 1);
                        break;
                    }
                    j += 1;
                }
                end
            }
            b'\'' => {
                let mut j = i + 1;
                let mut end = None;
                while j < b.len() {
                    if b[j] == b'\'' {
                        if b.get(j + 1) == Some(&b'\'') {
                            j += 2;
                            continue;
                        }
                        end = Some(j + 1);
                        break;
                    }
                    j += 1;
                }
                end.and_then(|e| {
                    if b.get(e) != Some(&b'!') {
                        return None;
                    }
                    let r1 = scan_ref(b, e + 1)?;
                    if b.get(r1) == Some(&b':') {
                        if let Some(r2) = scan_ref(b, r1 + 1) {
                            return Some(r2);
                        }
                    }
                    Some(r1)
                })
            }
            _ => None,
        };
        if let Some(e) = skip_to {
            out.push_str(&map_refs(&f[seg..i], map));
            out.push_str(&f[i..e]);
            i = e;
            seg = e;
            continue;
        }
        i += 1;
    }
    out.push_str(&map_refs(&f[seg..], map));
    out
}

fn map_refs(s: &str, map: &dyn Fn(&str, bool) -> String) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        let pre_ok = i == 0 || !matches!(b[i - 1], c if c.is_ascii_alphanumeric() || c == b'_' || c == b'!' || c == b'.');
        if pre_ok && (b[i] == b'$' || b[i].is_ascii_uppercase()) {
            if let Some(r1) = scan_ref(b, i) {
                let range_end = if b.get(r1) == Some(&b':') { scan_ref(b, r1 + 1).filter(|e| ref_boundary_ok(b, *e)) } else { None };
                let matched = match range_end {
                    Some(r2) => Some((r1, Some(r2))),
                    None if ref_boundary_ok(b, r1) => Some((r1, None)),
                    None => None,
                };
                if let Some((r1, r2)) = matched {
                    out.push_str(&s[last..i]);
                    out.push_str(&map(&s[i..r1], false));
                    let end = match r2 {
                        Some(r2) => {
                            out.push(':');
                            out.push_str(&map(&s[r1 + 1..r2], true));
                            r2
                        }
                        None => r1,
                    };
                    i = end;
                    last = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

fn renumber_rows(xml: &str, template_rows: &BTreeSet<i64>) -> String {
    let Some(sd_start) = xml.find("<sheetData") else { return xml.to_string() };
    let Some(sd_end) = xml.find("</sheetData>") else { return strip_gaps(xml) };
    if sd_end < sd_start {
        return strip_gaps(xml);
    }
    let row_re = crate::re!(r"<row\b([^>]*?)(/>|>([\s\S]*?)</row>)");
    let body = &xml[sd_start..sd_end];

    // pass 1: assign new row numbers
    let mut rows: Vec<(i64, i64)> = Vec::new();
    let mut cur = 0i64;
    let mut prev_orig = 0i64;
    for c in row_re.captures_iter(body) {
        let gap = tag_attr(&c[1], GAP_ATTR).and_then(|g| g.parse().ok()).unwrap_or(1);
        let orig = tag_attr(&c[1], "r").and_then(|r| r.parse().ok()).unwrap_or(prev_orig + 1);
        cur += gap;
        rows.push((orig, cur));
        prev_orig = orig;
    }
    let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
    for (o, n) in &rows {
        map.entry(*o).or_default().push(*n);
    }
    let changed = rows.iter().any(|(o, n)| o != n) || template_rows.iter().any(|r| !map.contains_key(r)) || rows.len() != map.len();
    if !changed {
        return strip_gaps(xml);
    }
    let mut sorted: Vec<i64> = map.keys().copied().collect();
    sorted.sort_unstable();
    let rm = RowMap { map, sorted, template_rows };

    // pass 2: rewrite rows, cells and formulas
    let mut n = 0;
    let new_body = row_re.replace_all(body, |c: &regex_lite::Captures| {
        let (orig, next) = rows.get(n).copied().unwrap_or((0, 0));
        n += 1;
        let self_closing = &c[2] == "/>";
        let head = remove_raw_attribute(&format!("<row{}{}", &c[1], if self_closing { "/>" } else { ">" }), GAP_ATTR);
        let head = set_raw_attribute(&head, "r", &next.to_string());
        if self_closing {
            return head;
        }
        let inner = c.get(3).map(|m| m.as_str()).unwrap_or("");
        let content = crate::re!(r#"(<c\b[^>]*?\sr=")([A-Z]+)\d+""#)
            .replace_all(inner, |m: &regex_lite::Captures| format!("{}{}{}\"", &m[1], &m[2], next));
        let content = rewrite_formulas(&content, &rm, (orig, next));
        format!("{head}{content}</row>")
    });
    let mut out = format!("{}{}{}", &xml[..sd_start], new_body, &xml[sd_end..]);

    // merged cells: duplicate merges inside repeated rows, shift the others
    out = crate::re!(r"<mergeCells\b[^>]*>([\s\S]*?)</mergeCells>")
        .replace(&out, |c: &regex_lite::Captures| {
            let mut merges: Vec<String> = Vec::new();
            for m in crate::re!(r#"<mergeCell\b[^>]*\sref="([^"]*)"[^>]*/>"#).captures_iter(&c[1]) {
                let r = &m[1];
                let (a, b) = r.split_once(':').unwrap_or((r, r));
                let row_of = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>().parse::<i64>().unwrap_or(0);
                let col_of = |s: &str| s.chars().filter(|ch| ch.is_ascii_alphabetic()).collect::<String>();
                let (ra, rb, ca, cb) = (row_of(a), row_of(b), col_of(a), col_of(b));
                if ra == rb {
                    if let Some(list) = rm.map.get(&ra) {
                        for r in list {
                            merges.push(format!("{ca}{r}:{cb}{r}"));
                        }
                        continue;
                    }
                }
                let (Some(na), Some(nb)) = (rm.row(ra, false, None), rm.row(rb, true, None)) else { continue };
                merges.push(format!("{ca}{na}:{cb}{nb}"));
            }
            if merges.is_empty() {
                return String::new();
            }
            let inner: String = merges.iter().map(|r| format!("<mergeCell ref=\"{r}\"/>")).collect();
            format!("<mergeCells count=\"{}\">{inner}</mergeCells>", merges.len())
        })
        .into_owned();
    // ranges in conditional formatting, data validation and hyperlinks
    out = crate::re!(r#"(<(?:conditionalFormatting|dataValidation)\b[^>]*\ssqref=")([^"]*)""#)
        .replace_all(&out, |c: &regex_lite::Captures| {
            let list: Vec<String> = c[2].split_whitespace().map(|r| rm.range(r, None)).collect();
            format!("{}{}\"", &c[1], list.join(" "))
        })
        .into_owned();
    out = crate::re!(r#"(<hyperlink\b[^>]*\sref=")([^"]*)""#)
        .replace_all(&out, |c: &regex_lite::Captures| format!("{}{}\"", &c[1], rm.range(&c[2], None)))
        .into_owned();
    // the used-range hint is optional; drop it rather than keep a wrong one
    crate::re!(r"<dimension\b[^>]*/>").replacen(&out, 1, "").into_owned()
}

/// Rewrites `<f ref="...">formula</f>` inside a row.
fn rewrite_formulas(content: &str, rm: &RowMap, own: (i64, i64)) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    loop {
        let Some(p) = find_f(rest) else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..p]);
        let Some(tag_end) = rest[p..].find('>').map(|e| p + e + 1) else {
            out.push_str(&rest[p..]);
            return out;
        };
        let open = &rest[p..tag_end];
        let open = match raw_attr(open, "ref") {
            Some(r) => set_raw_attribute(open, "ref", &rm.range(r, Some(own))),
            None => open.to_string(),
        };
        if open.ends_with("/>") {
            out.push_str(&open);
            rest = &rest[tag_end..];
            continue;
        }
        let Some(close) = rest[tag_end..].find("</f>").map(|e| tag_end + e) else {
            out.push_str(&open);
            out.push_str(&rest[tag_end..]);
            return out;
        };
        out.push_str(&open);
        out.push_str(&escape_xml(&rm.formula(&decode_xml(&rest[tag_end..close]), own)));
        out.push_str("</f>");
        rest = &rest[close + 4..];
    }
}

fn find_f(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(p) = s[from..].find("<f").map(|p| p + from) {
        from = p + 2;
        if matches!(s.as_bytes().get(p + 2), Some(c) if c.is_ascii_whitespace() || *c == b'>' || *c == b'/') {
            return Some(p);
        }
    }
    None
}

/// Text of an element's children, ignoring phonetic runs.
pub fn visible_text(el: &crate::xml::dom::XmlElement) -> String {
    el.elements().filter(|e| e.name != "rPh").map(element_text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_refs() {
        let shift = |s: &str, _end: bool| {
            crate::re!(r"\d+").replace_all(s, |c: &regex_lite::Captures| (c[0].parse::<i64>().unwrap() + 10).to_string()).into_owned()
        };
        assert_eq!(map_formula("SUM(D3:D5)+A1*$B$2", &shift), "SUM(D13:D15)+A11*$B$12");
        assert_eq!(map_formula("\"A1\"&Sheet2!A1&'My sheet'!B2&LOG10(A1)", &shift), "\"A1\"&Sheet2!A1&'My sheet'!B2&LOG10(A11)");
        assert_eq!(map_formula("ABCD1+X1Y", &shift), "ABCD1+X1Y");
    }

    #[test]
    fn markers() {
        let xml = "<c r=\"B3\" t=\"inlineStr\"><is><t xml:space=\"preserve\">\u{0}N2\u{1}2\u{0}</t></is></c><c r=\"A1\" t=\"inlineStr\"><is><t>Report: \u{0}N2024\u{1}2024\u{0}</t></is></c>";
        assert_eq!(resolve_scalar_markers(xml), "<c r=\"B3\"><v>2</v></c><c r=\"A1\" t=\"inlineStr\"><is><t>Report: 2024</t></is></c>");
    }
}
