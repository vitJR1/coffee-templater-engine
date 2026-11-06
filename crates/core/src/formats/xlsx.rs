//! XLSX adapter: inlines shared strings that hold tags, keeps numeric/boolean values typed, and
//! renumbers rows (with formulas, merges, validations, hyperlinks) after row loops.

use crate::opc::{rel, Package};
use crate::template::engine::{FormatHooks, FormatSpec, ImageMode};
use crate::value::{number_text, Value};
use crate::xml::dom::{element_text, parse_xml, XmlNode};
use crate::xml::escape::{decode_xml, escape_attr, escape_xml};
use crate::xml::tokenizer::{element_end, find_tags, raw_attr, remove_raw_attribute, set_raw_attribute, tag_end};
use std::collections::{BTreeSet, HashMap};

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
    /// Sheet part → sheet name.
    pub sheet_names: HashMap<String, String>,
}

fn tag_attr(attrs: &str, name: &str) -> Option<String> {
    raw_attr(&format!("<x{attrs}>"), name).map(str::to_string)
}

pub fn preprocess(pkg: &Package, part: &str, xml: &str, state: &mut XlsxState, delimiter_start: &str) -> String {
    if state.sheet_names.is_empty() {
        state.sheet_names = worksheet_parts(pkg).into_iter().map(|s| (s.path, s.name)).collect();
    }
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
    // 2. Remember the template rows; give every row an explicit number, so copies made by loops
    //    can be told apart from the rows that follow them.
    let mut rows = BTreeSet::new();
    let mut prev = 0i64;
    let xml = crate::re!(r"<row\b[^>]*>").replace_all(&xml, |c: &regex_lite::Captures| {
        let tag = &c[0];
        match raw_attr(tag, "r").and_then(|v| v.parse::<i64>().ok()) {
            Some(r) => {
                rows.insert(r);
                prev = r;
                tag.to_string()
            }
            None => {
                prev += 1;
                rows.insert(prev);
                set_raw_attribute(tag, "r", &prev.to_string())
            }
        }
    });
    state.template_rows.insert(part.to_string(), rows);
    xml.into_owned()
}

/// Resolves typed scalars and renumbers rows. Returns the row map when rows moved; `finalize`
/// uses it to update references to this sheet from tables, defined names and other sheets.
pub fn postprocess(xml: &str, part: &str, state: &XlsxState) -> (String, Option<RowMap>) {
    let xml = resolve_scalar_markers(xml);
    let empty = BTreeSet::new();
    renumber_rows(&xml, state.template_rows.get(part).unwrap_or(&empty), state.sheet_names.get(part).map(String::as_str))
}

const REL_TABLE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";
const REL_COMMENTS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const REL_VML: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
const REL_DRAWING: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing";
const REL_CHART: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";

/// Last row of a worksheet.
const MAX_ROW: i64 = 1_048_576;

fn update_text(pkg: &mut Package, path: &str, f: impl FnOnce(&str) -> String) {
    let Some(xml) = pkg.get_text(path) else { return };
    let next = f(&xml);
    if next != xml {
        pkg.set_text(path, &next);
    }
}

/// Updates what lives outside the rendered sheets (`maps`: sheet part → row map), then drops the
/// calculation chain.
pub fn finalize(pkg: &mut Package, maps: &[(String, RowMap)]) {
    let wb = workbook_path(pkg);
    if !maps.is_empty() {
        let sheets = worksheet_parts(pkg);
        let named: Vec<(&str, &RowMap)> =
            sheets.iter().filter_map(|s| maps.iter().find(|(p, _)| *p == s.path).map(|(_, m)| (s.name.as_str(), m))).collect();
        let qualified = |f: &str, sheets: &[(&str, &RowMap)]| if f.contains('!') { Some(map_qualified(f, sheets, None)) } else { None };
        for s in &sheets {
            // references to rendered sheets (`Sheet1!A5`, `'My sheet'!A5`) from any sheet; a sheet's
            // references to itself were mapped when it was renumbered
            let others: Vec<(&str, &RowMap)> = named.iter().filter(|(n, _)| !n.eq_ignore_ascii_case(&s.name)).copied().collect();
            if !others.is_empty() {
                update_text(pkg, &s.path, |xml| {
                    let mut xml = xml.to_string();
                    for (open, close) in FORMULA_ELEMENTS {
                        xml = rewrite_elements(&xml, open, close, None, &|f| qualified(f, &others));
                    }
                    rewrite_attr(&xml, "<hyperlink", "location", &|v| qualified(v, &others))
                });
            }
            // chart series on this sheet's drawings
            for (_, drawing) in pkg.related_parts(&s.path, REL_DRAWING) {
                for (_, chart) in pkg.related_parts(&drawing, REL_CHART) {
                    update_text(pkg, &chart, |xml| rewrite_elements(xml, "<c:f", "</c:f>", None, &|f| qualified(f, &named)));
                }
            }
        }
        // defined names (print areas, named ranges)
        update_text(pkg, &wb, |xml| rewrite_elements(xml, "<definedName", "</definedName>", None, &|f| qualified(f, &named)));
        for (part, map) in maps {
            finalize_tables(pkg, part, map);
            finalize_comments(pkg, part, map);
            for (_, drawing) in pkg.related_parts(part, REL_DRAWING) {
                // anchors are 0-based rows
                update_text(pkg, &drawing, |xml| {
                    rewrite_elements(xml, "<xdr:row", "</xdr:row>", None, &|t| {
                        let r = t.trim().parse::<i64>().ok()?;
                        Some((map.row_or_shift(r + 1) - 1).to_string())
                    })
                });
            }
        }
    }
    finalize_calc(pkg, &wb);
}

/// Tables on a rendered sheet: map their ranges; a table whose rows are all gone is removed, and a
/// table left without data rows keeps one (Excel does not accept a table without data rows).
fn finalize_tables(pkg: &mut Package, part: &str, map: &RowMap) {
    for (r, table) in pkg.related_parts(part, REL_TABLE) {
        let Some(xml) = pkg.get_text(&table) else { continue };
        let Some((s, e)) = tags(&xml, "<table").next() else { continue };
        let tag = &xml[s..e];
        let Some(old) = raw_attr(tag, "ref") else { continue };
        let num = |name: &str, default: i64| raw_attr(tag, name).and_then(|v| v.parse::<i64>().ok()).unwrap_or(default);
        let (header, totals) = (num("headerRowCount", 1), num("totalsRowCount", 0));
        let Some(new) = map.range_text(old, None) else {
            pkg.remove(&table);
            pkg.remove_override(&table);
            pkg.remove_relationships(part, |x| x.id == r.id);
            update_text(pkg, part, |sheet| {
                let mut out = sheet.to_string();
                if let Some((ts, te)) = tags(sheet, "<tablePart").find(|(ts, te)| raw_attr(&sheet[*ts..*te], "r:id") == Some(r.id.as_str()))
                {
                    out.replace_range(ts..te, "");
                }
                fix_wrapper(&out, "<tableParts", "<tablePart", &["count"])
            });
            continue;
        };
        let Some(((ca, x), (cb, mut y))) = split_range(&new) else { continue };
        y = y.max(x + header + totals).min(MAX_ROW);
        let new = format!("{ca}{x}:{cb}{y}");
        let mut out = format!("{}{}{}", &xml[..s], set_raw_attribute(tag, "ref", &new), &xml[e..]);
        // the table's filter covers the table without its totals row
        out = rewrite_attr(&out, "<autoFilter", "ref", &|_| Some(format!("{ca}{x}:{cb}{}", y - totals)));
        for t in ["<sortState", "<sortCondition"] {
            out = rewrite_attr(&out, t, "ref", &|v| map.range_text(v, None));
        }
        if out != xml {
            pkg.set_text(&table, &out);
        }
    }
}

/// Cell comments and their legacy VML shapes follow their cells; comments on removed rows go.
fn finalize_comments(pkg: &mut Package, part: &str, map: &RowMap) {
    for (_, comments) in pkg.related_parts(part, REL_COMMENTS) {
        update_text(pkg, &comments, |xml| {
            let out = rewrite_each(xml, "<comment", |tag, body| {
                let Some(r) = raw_attr(tag, "ref") else { return Some(format!("{tag}{body}")) };
                let n = map.range_text(r, None)?;
                Some(format!("{}{body}", set_raw_attribute(tag, "ref", &n)))
            });
            out
        });
    }
    for (_, vml) in pkg.related_parts(part, REL_VML) {
        update_text(pkg, &vml, |xml| {
            rewrite_each(xml, "<v:shape", |tag, body| {
                let Some(rs) = body.find("<x:Row>") else { return Some(format!("{tag}{body}")) };
                let re = body[rs..].find("</x:Row>")? + rs;
                let Ok(old) = body[rs + 7..re].trim().parse::<i64>() else { return Some(format!("{tag}{body}")) };
                let new = map.row(old + 1, false, None)? - 1;
                let delta = new - old;
                let mut body = format!("{}<x:Row>{new}</x:Row>{}", &body[..rs], &body[re + 8..]);
                if let Some(a) = body.find("<x:Anchor>") {
                    if let Some(ae) = body[a..].find("</x:Anchor>").map(|e| e + a) {
                        let vals: Vec<String> = body[a + 10..ae]
                            .split(',')
                            .enumerate()
                            .map(|(i, v)| match (i, v.trim().parse::<i64>()) {
                                (2 | 6, Ok(n)) => (n + delta).to_string(),
                                _ => v.trim().to_string(),
                            })
                            .collect();
                        body = format!("{}<x:Anchor>{}</x:Anchor>{}", &body[..a], vals.join(", "), &body[ae + 11..]);
                    }
                }
                Some(format!("{tag}{body}"))
            })
        });
    }
}

/// Cached calculation order is invalid after rows move: drop it and ask Excel to recalculate.
fn finalize_calc(pkg: &mut Package, wb: &str) {
    let wb = wb.to_string();
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

// ------------------------------------------------------------------------------------------
// Scanning helpers (hand-written: this code runs once per row and cell)
// ------------------------------------------------------------------------------------------

fn tags<'a>(xml: &'a str, tag: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    find_tags(xml, tag)
}

/// Elements whose text holds a formula.
const FORMULA_ELEMENTS: [(&str, &str); 5] =
    [("<f", "</f>"), ("<formula", "</formula>"), ("<formula1", "</formula1>"), ("<formula2", "</formula2>"), ("<xm:f", "</xm:f>")];

/// Rewrites the text of every `<open ...>text</close>` element (and optionally its open tag).
/// `text` gets the decoded text and returns `None` to keep it as it is.
fn rewrite_elements(
    xml: &str,
    open: &str,
    close: &str,
    head: Option<&dyn Fn(&str) -> String>,
    text: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    let mut skip_to = 0;
    for (s, e) in tags(xml, open) {
        if s < skip_to {
            continue;
        }
        let tag = &xml[s..e];
        out.push_str(&xml[last..s]);
        out.push_str(&head.map(|h| h(tag)).unwrap_or_else(|| tag.to_string()));
        last = e;
        if tag.ends_with("/>") {
            continue;
        }
        let Some(c) = xml[e..].find(close).map(|c| c + e) else { break };
        match text(&decode_xml(&xml[e..c])) {
            Some(t) => out.push_str(&escape_xml(&t)),
            None => out.push_str(&xml[e..c]),
        }
        last = c;
        skip_to = c;
    }
    out.push_str(&xml[last..]);
    out
}

/// Rewrites attribute `name` of every `tag`; `map` gets the decoded value and returns `None` to
/// keep it.
fn rewrite_attr(xml: &str, tag: &str, name: &str, map: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    for (s, e) in tags(xml, tag) {
        let t = &xml[s..e];
        let Some(v) = raw_attr(t, name) else { continue };
        let Some(n) = map(&decode_xml(v)) else { continue };
        out.push_str(&xml[last..s]);
        out.push_str(&set_raw_attribute(t, name, &escape_attr(&n)));
        last = e;
    }
    out.push_str(&xml[last..]);
    out
}

/// Calls `f(open_tag, rest_of_element)` for every `tag` element; the element is replaced by the
/// result, or removed on `None`.
fn rewrite_each(xml: &str, tag: &str, mut f: impl FnMut(&str, &str) -> Option<String>) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    let mut skip_to = 0;
    for (s, e) in tags(xml, tag) {
        if s < skip_to {
            continue;
        }
        let Some(end) = element_end(xml, s, e) else { break };
        out.push_str(&xml[last..s]);
        if let Some(x) = f(&xml[s..e], &xml[e..end]) {
            out.push_str(&x);
        }
        last = end;
        skip_to = end;
    }
    out.push_str(&xml[last..]);
    out
}

/// Drops a `wrapper` element left without `child` elements (the schema requires at least one),
/// and updates its count attributes.
fn fix_wrapper(xml: &str, wrapper: &str, child: &str, counts: &[&str]) -> String {
    let Some((s, e)) = tags(xml, wrapper).next() else { return xml.to_string() };
    let Some(end) = element_end(xml, s, e) else { return xml.to_string() };
    let inner = &xml[e..end];
    let n = tags(inner, child).count();
    if n == 0 {
        return format!("{}{}", &xml[..s], &xml[end..]);
    }
    let mut head = xml[s..e].to_string();
    for c in counts {
        if raw_attr(&head, c).is_some() {
            let v = if *c == "manualBreakCount" {
                tags(inner, child).filter(|(a, b)| matches!(raw_attr(&inner[*a..*b], "man"), Some("1" | "true"))).count()
            } else {
                n
            };
            head = set_raw_attribute(&head, c, &v.to_string());
        }
    }
    format!("{}{head}{}", &xml[..s], &xml[e..])
}

/// Splits `$A$12` into (`$A$`, 12) and `$12` into (`$`, 12).
fn split_ref(r: &str) -> Option<(&str, i64)> {
    let b = r.as_bytes();
    let end = scan_cell(b, 0).or_else(|| scan_row(b, 0))?;
    if end != b.len() {
        return None;
    }
    let digits = b.iter().position(u8::is_ascii_digit)?;
    Some((&r[..digits], r[digits..].parse().ok()?))
}

/// A reference split into its column part and row: (`$A$`, 12).
type RefParts<'a> = (&'a str, i64);

/// Splits `A2:C5` (or `A2`) into its two ends.
fn split_range(r: &str) -> Option<(RefParts<'_>, RefParts<'_>)> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    Some((split_ref(a)?, split_ref(b)?))
}

fn col_number(letters: &str) -> i64 {
    letters.bytes().filter(u8::is_ascii_uppercase).fold(0, |n, c| n * 26 + i64::from(c - b'A' + 1))
}

// ------------------------------------------------------------------------------------------
// Typed scalars
// ------------------------------------------------------------------------------------------

/// `\0{N|B}value\1text\0` → `text` (values mixed with other text stay strings).
fn strip_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find(MARK) {
        out.push_str(&rest[..p]);
        let after = &rest[p + 1..];
        match (after.find(MARK_SEP), after.find(MARK)) {
            (Some(sep), Some(end)) if sep < end => {
                out.push_str(&after[sep + 1..end]);
                rest = &after[end + 1..];
            }
            _ => rest = after,
        }
    }
    out.push_str(rest);
    out
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_ascii_whitespace())
}

/// Matches a cell body that holds exactly one scalar marker:
/// `[<f>…</f>] <is> <t …>\0{N|B}value\1text\0</t> </is>`. Returns (formula, kind, value).
fn scalar_cell(inner: &str) -> Option<(&str, char, &str)> {
    let mut rest = skip_ws(inner);
    let mut formula = "";
    if rest.starts_with("<f") && matches!(rest.as_bytes().get(2), Some(c) if c.is_ascii_whitespace() || *c == b'>') {
        let gt = tag_end(rest, 0)?;
        if rest[..gt].ends_with("/>") {
            return None;
        }
        let close = rest[gt..].find("</f>")? + gt;
        if rest[gt..close].contains('<') {
            return None;
        }
        formula = &rest[..close + 4];
        rest = skip_ws(&rest[close + 4..]);
    }
    rest = skip_ws(rest.strip_prefix("<is>")?);
    if !rest.starts_with("<t") || !matches!(rest.as_bytes().get(2), Some(c) if c.is_ascii_whitespace() || *c == b'>') {
        return None;
    }
    rest = &rest[tag_end(rest, 0)?..];
    rest = rest.strip_prefix(MARK)?;
    let kind = rest.chars().next().filter(|k| *k == 'N' || *k == 'B')?;
    rest = &rest[1..];
    let sep = rest.find(MARK_SEP)?;
    let value = &rest[..sep];
    if value.contains(MARK) {
        return None;
    }
    rest = &rest[sep + 1..];
    rest = &rest[rest.find(MARK)? + 1..];
    rest = skip_ws(rest.strip_prefix("</t>")?);
    rest = skip_ws(rest.strip_prefix("</is>")?);
    rest.is_empty().then_some((formula, kind, value))
}

/// Turns cells that consist of a single numeric/boolean value into real typed cells. A formula in
/// the cell is kept, with the value as its cached result.
fn resolve_scalar_markers(xml: &str) -> String {
    if !xml.contains(MARK) {
        return xml.to_string();
    }
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    let mut skip_to = 0;
    for (s, e) in tags(xml, "<c") {
        if s < skip_to {
            continue;
        }
        let tag = &xml[s..e];
        if tag.ends_with("/>") {
            continue;
        }
        let Some(close) = xml[e..].find("</c>").map(|c| c + e) else { break };
        skip_to = close;
        let inner = &xml[e..close];
        if !inner.contains(MARK) {
            continue;
        }
        out.push_str(&strip_markers(&xml[last..s]));
        match scalar_cell(inner) {
            Some((formula, kind, value)) => {
                let mut head = remove_raw_attribute(tag, "t");
                if kind == 'B' {
                    head = set_raw_attribute(&head, "t", "b");
                }
                out.push_str(&head);
                out.push_str(formula);
                out.push_str("<v>");
                out.push_str(value);
                out.push_str("</v>");
            }
            None => {
                out.push_str(tag);
                out.push_str(&strip_markers(inner));
            }
        }
        last = close;
    }
    out.push_str(&strip_markers(&xml[last..]));
    out
}

// ------------------------------------------------------------------------------------------
// Row renumbering
// ------------------------------------------------------------------------------------------

/// Where template rows went after rendering.
#[derive(Debug, Clone)]
pub struct RowMap {
    /// Original row → new numbers of its copies, in order.
    map: HashMap<i64, Vec<i64>>,
    /// Original rows that are still present, sorted.
    sorted: Vec<i64>,
    /// Template rows removed by empty loops.
    removed: BTreeSet<i64>,
}

/// The row a formula lives in: its template row, its new number and which loop copy it is.
#[derive(Debug, Clone, Copy)]
struct Own {
    orig: i64,
    new: i64,
    copy: usize,
}

impl RowMap {
    /// True when `a` and `b` are rows of the same repeated block (a loop over several rows):
    /// their copies interleave one to one.
    fn same_block(&self, a: i64, b: i64) -> bool {
        if a == b {
            return true;
        }
        let (Some(x), Some(y)) = (self.map.get(&a), self.map.get(&b)) else { return false };
        x.len() > 1 && x.len() == y.len() && (0..x.len() - 1).all(|k| x[k].max(y[k]) < x[k + 1].min(y[k + 1]))
    }

    fn removed_between(&self, lo: i64, hi: i64) -> i64 {
        if hi <= lo + 1 {
            return 0;
        }
        self.removed.range(lo + 1..hi).count() as i64
    }

    /// New number of a row that is not a template row (a blank row), or of a removed row treated
    /// as one: it moves with the closest present row above, minus the removed rows in between.
    fn shift_blank(&self, r: i64) -> i64 {
        let at = self.sorted.partition_point(|o| *o < r);
        if at == 0 {
            return r - self.removed_between(0, r);
        }
        let base = self.sorted[at - 1];
        let last = self.map[&base].last().copied().unwrap_or(base);
        last + (r - base) - self.removed_between(base, r)
    }

    /// Maps an original row to its new number; `end` = last copy (for range ends). A row of the
    /// same repeated block as `own` maps to the same copy. `None` when the row was removed.
    fn row(&self, r: i64, end: bool, own: Option<Own>) -> Option<i64> {
        if r >= MAX_ROW {
            // ranges that run to the end of the sheet stay there
            return Some(r);
        }
        let n = match (own, self.map.get(&r)) {
            (Some(o), _) if r == o.orig => o.new,
            (Some(o), Some(list)) if self.same_block(o.orig, r) => list[o.copy],
            (_, Some(list)) => {
                if end {
                    *list.last()?
                } else {
                    list[0]
                }
            }
            (_, None) if self.removed.contains(&r) => return None,
            (_, None) => self.shift_blank(r),
        };
        Some(n.min(MAX_ROW))
    }

    /// Like `row`, but a removed row moves like a blank row instead of disappearing.
    fn row_or_shift(&self, r: i64) -> i64 {
        self.row(r, false, None).unwrap_or_else(|| self.shift_blank(r).min(MAX_ROW))
    }

    /// Maps a row span, clamping ends that fall on removed rows. `None` when every row is gone.
    fn span(&self, a: i64, b: i64, own: Option<Own>) -> Option<(i64, i64)> {
        let (mut s, mut e) = (a.min(b), a.max(b));
        while s <= e && self.removed.contains(&s) {
            s += 1;
        }
        while e >= s && self.removed.contains(&e) {
            e -= 1;
        }
        if s > e {
            return None;
        }
        Some((self.row(s, false, own)?, self.row(e, true, own)?))
    }

    /// Maps `A1`, `A1:B5` or `1:5` text. `None` when the referenced rows were removed.
    fn ref_text(&self, a: &str, b: Option<&str>, own: Option<Own>) -> Option<String> {
        let Some((pa, ra)) = split_ref(a) else { return Some(b.map(|b| format!("{a}:{b}")).unwrap_or_else(|| a.to_string())) };
        match b {
            None => self.row(ra, false, own).map(|n| format!("{pa}{n}")),
            Some(b) => {
                let Some((pb, rb)) = split_ref(b) else { return Some(format!("{a}:{b}")) };
                let (x, y) = self.span(ra, rb, own)?;
                let (x, y) = if ra <= rb { (x, y) } else { (y, x) };
                Some(format!("{pa}{x}:{pb}{y}"))
            }
        }
    }

    fn range_text(&self, r: &str, own: Option<Own>) -> Option<String> {
        match r.split_once(':') {
            None => self.ref_text(r, None, own),
            Some((a, b)) => self.ref_text(a, Some(b), own),
        }
    }

    /// Maps a space-separated list of ranges (`sqref`). A single cell is a one-cell range, so a
    /// cell of a repeated row covers every copy. `None` when nothing is left.
    fn sqref(&self, list: &str) -> Option<String> {
        let out: Vec<String> = list
            .split_whitespace()
            .filter_map(|r| {
                let (a, b) = r.split_once(':').unwrap_or((r, r));
                let t = self.ref_text(a, Some(b), None)?;
                Some(match t.split_once(':') {
                    Some((x, y)) if x == y => x.to_string(),
                    _ => t,
                })
            })
            .collect();
        (!out.is_empty()).then(|| out.join(" "))
    }

    /// Maps a formula of this sheet: unqualified references and references qualified with the
    /// sheet's own name (`own_name`).
    fn formula(&self, f: &str, own: Option<Own>, own_name: Option<&str>) -> String {
        let out = map_formula(f, &|a, b| self.ref_text(a, b, own).unwrap_or_else(|| "#REF!".into()));
        match own_name {
            Some(n) if out.contains('!') => map_qualified(&out, &[(n, self)], own),
            _ => out,
        }
    }
}

/// `$?[A-Z]{1,3}$?[0-9]+` (row 1..=1048576) at `i`; returns the end offset.
fn scan_cell(b: &[u8], i: usize) -> Option<usize> {
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
    scan_row(b, k)
}

/// `$?[0-9]+` (1..=1048576) at `i`; returns the end offset.
fn scan_row(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i;
    if b.get(k) == Some(&b'$') {
        k += 1;
    }
    let ds = k;
    while k < b.len() && b[k].is_ascii_digit() && k - ds < 8 {
        k += 1;
    }
    if k == ds || k - ds > 7 || b.get(k).is_some_and(u8::is_ascii_digit) || b[ds] == b'0' {
        return None;
    }
    let n: i64 = std::str::from_utf8(&b[ds..k]).ok()?.parse().ok()?;
    (n <= MAX_ROW).then_some(k)
}

/// A reference may not be followed by these (names, functions, sheet names, structured refs).
fn ref_boundary_ok(b: &[u8], end: usize) -> bool {
    !matches!(b.get(end), Some(c) if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'(' | b'.' | b'!' | b'[' | b'$'))
}

/// A cell reference, a cell range or a row range (`5:7`) at `i`: (end of the first part, end of
/// the second part).
fn scan_ref_token(b: &[u8], i: usize) -> Option<(usize, Option<usize>)> {
    if let Some(r1) = scan_cell(b, i) {
        if b.get(r1) == Some(&b':') {
            if let Some(r2) = scan_cell(b, r1 + 1).filter(|e| ref_boundary_ok(b, *e)) {
                return Some((r1, Some(r2)));
            }
        }
        return ref_boundary_ok(b, r1).then_some((r1, None));
    }
    let r1 = scan_row(b, i)?;
    if b.get(r1) != Some(&b':') {
        return None;
    }
    let r2 = scan_row(b, r1 + 1).filter(|e| ref_boundary_ok(b, *e))?;
    Some((r1, Some(r2)))
}

/// End of a `"…"` string literal starting at `i` (with `""` escapes).
fn string_end(b: &[u8], i: usize, q: u8) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return Some(j + 1);
        }
        j += 1;
    }
    None
}

/// End of a `[…]` group starting at `i` (structured references nest them).
fn bracket_end(b: &[u8], i: usize) -> usize {
    let mut depth = 0;
    let mut j = i;
    while j < b.len() {
        match b[j] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            b'\'' => j += 1, // escape character inside structured references
            _ => {}
        }
        j += 1;
    }
    b.len()
}

/// Skips the reference that follows a sheet prefix (`A1`, `A1:B2`, `A:C`, `$5:$5`, a name).
fn token_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'$' | b'_' | b'.' | b':' | b'#')) {
        i += 1;
    }
    if b.get(i) == Some(&b'!') && b.get(i.wrapping_sub(1)) == Some(&b'F') {
        i += 1; // #REF!
    }
    i
}

/// Rewrites unqualified references of a formula (`A1`, `$B$2:C5`, `3:5`). String literals,
/// structured references (`Table1[Col]`) and references to sheets (`Sheet2!A1`, `'My sheet'!A:A`,
/// `[1]Sheet!A1`) are left untouched. `map` receives a reference or both ends of a range and
/// returns the replacement.
pub fn map_formula(f: &str, map: &dyn Fn(&str, Option<&str>) -> String) -> String {
    let b = f.as_bytes();
    let mut out = String::with_capacity(f.len());
    let mut seg = 0;
    let mut i = 0;
    while i < b.len() {
        let skip_to = match b[i] {
            b'"' => Some(string_end(b, i, b'"').unwrap_or(b.len())),
            b'[' => Some(bracket_end(b, i)),
            b'\'' => Some(match string_end(b, i, b'\'') {
                Some(e) if b.get(e) == Some(&b'!') => token_end(b, e + 1),
                Some(e) => e,
                None => b.len(),
            }),
            _ => None,
        };
        if let Some(e) = skip_to {
            map_refs(&f[seg..i], map, &mut out);
            out.push_str(&f[i..e]);
            i = e;
            seg = e;
            continue;
        }
        i += 1;
    }
    map_refs(&f[seg..], map, &mut out);
    out
}

fn map_refs(s: &str, map: &dyn Fn(&str, Option<&str>) -> String, out: &mut String) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        if !(b[i] == b'$' || b[i].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let prev = if i == 0 { None } else { Some(b[i - 1]) };
        if matches!(prev, Some(c) if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'$' | b':' | b']')) {
            i += 1;
            continue;
        }
        if prev == Some(b'!') {
            // qualified reference (Sheet2!A1:B2): another sheet's rows, skip it whole
            i = token_end(b, i).max(i + 1);
            continue;
        }
        let Some((r1, r2)) = scan_ref_token(b, i) else {
            i += 1;
            continue;
        };
        let end = r2.unwrap_or(r1);
        out.push_str(&s[last..i]);
        out.push_str(&map(&s[i..r1], r2.map(|r2| &s[r1 + 1..r2])));
        i = end;
        last = end;
    }
    out.push_str(&s[last..]);
}

/// Rewrites references qualified with one of `sheets` (`Sheet1!A5`, `'My sheet'!A1:B2`,
/// `Data!$3:$4`). References to other workbooks (`[1]Data!A5`) and 3D references
/// (`S1:S3!A5`) are left untouched.
fn map_qualified(f: &str, sheets: &[(&str, &RowMap)], own: Option<Own>) -> String {
    let b = f.as_bytes();
    let name_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c >= 0x80;
    let mut out = String::with_capacity(f.len());
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        let (name, bang) = match b[i] {
            b'"' => {
                i = string_end(b, i, b'"').unwrap_or(b.len());
                continue;
            }
            b'[' => {
                // a structured reference, or the workbook of an external reference: skip the
                // sheet name and reference that follow it
                i = bracket_end(b, i);
                let mut k = i;
                while k < b.len() && name_char(b[k]) {
                    k += 1;
                }
                if b.get(k) == Some(&b'!') {
                    i = token_end(b, k + 1);
                }
                continue;
            }
            b'\'' => match string_end(b, i, b'\'') {
                Some(e) if b.get(e) == Some(&b'!') => (f[i + 1..e - 1].replace("''", "'"), e),
                Some(e) => {
                    i = e;
                    continue;
                }
                None => break,
            },
            c if name_char(c) && (i == 0 || !name_char(b[i - 1])) => {
                let mut e = i;
                while e < b.len() && name_char(b[e]) {
                    e += 1;
                }
                if b.get(e) == Some(&b':') {
                    let mut k = e + 1;
                    while k < b.len() && name_char(b[k]) {
                        k += 1;
                    }
                    if k > e + 1 && b.get(k) == Some(&b'!') {
                        i = token_end(b, k + 1); // 3D reference
                        continue;
                    }
                }
                if b.get(e) != Some(&b'!') {
                    i = e;
                    continue;
                }
                (f[i..e].to_string(), e)
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(map) = sheets.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)).map(|(_, m)| *m) else {
            i = token_end(b, bang + 1).max(bang + 1);
            continue;
        };
        let Some((r1, r2)) = scan_ref_token(b, bang + 1) else {
            i = token_end(b, bang + 1).max(bang + 1);
            continue;
        };
        let end = r2.unwrap_or(r1);
        let mapped = map.ref_text(&f[bang + 1..r1], r2.map(|r2| &f[r1 + 1..r2]), own);
        out.push_str(&f[last..i]);
        match mapped {
            Some(t) => {
                out.push_str(&f[i..=bang]);
                out.push_str(&t);
            }
            None => out.push_str("#REF!"),
        }
        i = end;
        last = end;
    }
    out.push_str(&f[last..]);
    out
}

struct RowSpan<'a> {
    start: usize,
    /// Attributes of the open tag (between `<row` and `>` / `/>`).
    attrs: &'a str,
    /// Inner XML, `None` for `<row/>`.
    inner: Option<&'a str>,
    end: usize,
}

fn rows(body: &str) -> Vec<RowSpan<'_>> {
    let mut out = Vec::new();
    let mut skip_to = 0;
    for (s, e) in tags(body, "<row") {
        if s < skip_to {
            continue;
        }
        let tag = &body[s..e];
        if let Some(attrs) = tag.strip_suffix("/>") {
            out.push(RowSpan { start: s, attrs: &attrs[4..], inner: None, end: e });
            continue;
        }
        let Some(close) = body[e..].find("</row>").map(|c| c + e) else { break };
        out.push(RowSpan { start: s, attrs: &tag[4..tag.len() - 1], inner: Some(&body[e..close]), end: close + 6 });
        skip_to = close + 6;
    }
    out
}

fn attr_num(tag: &str, name: &str) -> Option<i64> {
    raw_attr(tag, name).and_then(|v| v.parse().ok())
}

/// Cells of a row: (start, end of open tag, end of element).
fn cells(inner: &str) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    let mut skip_to = 0;
    tags(inner, "<c").filter_map(move |(s, e)| {
        if s < skip_to {
            return None;
        }
        let end = if inner[..e].ends_with("/>") { e } else { inner[e..].find("</c>").map(|c| c + e + 4)? };
        skip_to = end;
        Some((s, e, end))
    })
}

/// The `<f>` of a cell: (start, end of open tag, end of text, end of element).
fn formula_of(cell: &str) -> Option<(usize, usize, usize, usize)> {
    let (s, e) = tags(cell, "<f").next()?;
    if cell[..e].ends_with("/>") {
        return Some((s, e, e, e));
    }
    let c = cell[e..].find("</f>")? + e;
    Some((s, e, c, c + 4))
}

/// Shared formulas that touch a repeated row are expanded into ordinary formulas: copies of a
/// group's master would otherwise repeat its `si` and overlap its range.
#[derive(Default)]
struct SharedPlan {
    /// si → (formula text, column, template row) of the master.
    masters: HashMap<String, (String, i64, i64)>,
    expand: BTreeSet<String>,
}

fn shared_plan(body: &str, spans: &[RowSpan], origs: &[i64], map: &HashMap<i64, Vec<i64>>) -> SharedPlan {
    let mut plan = SharedPlan::default();
    if !body.contains("t=\"shared\"") || map.values().all(|l| l.len() < 2) {
        return plan;
    }
    for (r, orig) in spans.iter().zip(origs) {
        let Some(inner) = r.inner else { continue };
        let repeated = map.get(orig).is_some_and(|l| l.len() > 1);
        for (s, e, end) in cells(inner) {
            let cell = &inner[s..end];
            let Some((fs, fe, te, _)) = formula_of(cell) else { continue };
            let tag = &cell[fs..fe];
            if raw_attr(tag, "t") != Some("shared") {
                continue;
            }
            let Some(si) = raw_attr(tag, "si") else { continue };
            if repeated {
                plan.expand.insert(si.to_string());
            }
            if raw_attr(tag, "ref").is_some() && !plan.masters.contains_key(si) {
                let col = raw_attr(&inner[s..e], "r").map(col_number).unwrap_or(0);
                plan.masters.insert(si.to_string(), (decode_xml(&cell[fe..te]).into_owned(), col, *orig));
            }
        }
    }
    plan.expand.retain(|si| plan.masters.contains_key(si));
    plan
}

fn renumber_rows(xml: &str, template_rows: &BTreeSet<i64>, own_name: Option<&str>) -> (String, Option<RowMap>) {
    let Some(sd_start) = xml.find("<sheetData") else { return (xml.to_string(), None) };
    let Some(sd_end) = xml[sd_start..].find("</sheetData>").map(|e| e + sd_start) else { return (xml.to_string(), None) };
    let body = &xml[sd_start..sd_end];
    let spans = rows(body);

    // pass 1: assign new row numbers. Blank rows between template rows are kept; template rows
    // that are not rendered (empty loops) leave no gap, as if deleted in Excel.
    let mut origs: Vec<i64> = Vec::with_capacity(spans.len());
    let mut prev = 0i64;
    for r in &spans {
        let o = tag_attr_num(r.attrs, "r").unwrap_or(prev + 1);
        origs.push(o);
        prev = o;
    }
    let mut numbers: Vec<i64> = Vec::with_capacity(spans.len());
    let mut cur = 0i64;
    let mut prev = 0i64;
    for &o in &origs {
        // a copy of a looped row starts right after the previous copy
        let gap = if o <= prev { 1 } else { (o - prev) - template_rows.range(prev + 1..o).count() as i64 };
        cur += gap.max(1);
        numbers.push(cur);
        prev = o;
    }
    let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut copy_of: Vec<usize> = Vec::with_capacity(spans.len());
    for (o, n) in origs.iter().zip(&numbers) {
        let list = map.entry(*o).or_default();
        copy_of.push(list.len());
        list.push(*n);
    }
    let removed: BTreeSet<i64> = template_rows.iter().copied().filter(|r| !map.contains_key(r)).collect();
    let changed = origs.iter().zip(&numbers).any(|(o, n)| o != n) || !removed.is_empty() || numbers.len() != map.len();
    if !changed {
        return (xml.to_string(), None);
    }
    let plan = shared_plan(body, &spans, &origs, &map);
    let mut sorted: Vec<i64> = map.keys().copied().collect();
    sorted.sort_unstable();
    let rm = RowMap { map, sorted, removed };

    // pass 2: rewrite rows, cells and formulas
    let mut out = String::with_capacity(xml.len() + xml.len() / 8);
    // the used-range hint is optional; drop it rather than keep a wrong one
    out.push_str(&drop_elements(&xml[..sd_start], "<dimension"));
    let mut last = 0;
    for (k, r) in spans.iter().enumerate() {
        let own = Own { orig: origs[k], new: numbers[k], copy: copy_of[k] };
        out.push_str(&body[last..r.start]);
        last = r.end;
        let close = if r.inner.is_none() { "/>" } else { ">" };
        let next_s = own.new.to_string();
        out.push_str(&set_raw_attribute(&format!("<row{}{close}", r.attrs), "r", &next_s));
        let Some(inner) = r.inner else { continue };
        let mut cl = 0;
        for (s, e, end) in cells(inner) {
            let tag = &inner[s..e];
            out.push_str(&inner[cl..s]);
            cl = end;
            let col = raw_attr(tag, "r").map(|v| v.trim_end_matches(|c: char| c.is_ascii_digit()));
            match col {
                Some(col) => out.push_str(&set_raw_attribute(tag, "r", &format!("{col}{next_s}"))),
                None => out.push_str(tag),
            }
            let cell = &inner[e..end];
            match formula_of(cell) {
                Some(f) => out.push_str(&rewrite_cell_formula(cell, f, col.map(col_number).unwrap_or(0), own, &rm, own_name, &plan)),
                None => out.push_str(cell),
            }
        }
        out.push_str(&inner[cl..]);
        out.push_str("</row>");
    }
    out.push_str(&body[last..]);
    out.push_str(&rewrite_sheet_ranges(&xml[sd_end..], &rm, own_name));
    (out, Some(rm))
}

fn tag_attr_num(attrs: &str, name: &str) -> Option<i64> {
    attr_num(&format!("<x{attrs}>"), name)
}

fn rewrite_cell_formula(
    cell: &str,
    (fs, fe, te, end): (usize, usize, usize, usize),
    col: i64,
    own: Own,
    rm: &RowMap,
    own_name: Option<&str>,
    plan: &SharedPlan,
) -> String {
    let tag = &cell[fs..fe];
    let si = raw_attr(tag, "si").filter(|si| raw_attr(tag, "t") == Some("shared") && plan.expand.contains(*si));
    let (head, text) = match si.and_then(|si| plan.masters.get(si)) {
        Some((master, mcol, mrow)) => {
            let text = if raw_attr(tag, "ref").is_some() {
                decode_xml(&cell[fe..te]).into_owned()
            } else {
                crate::model::xlsx::shift_formula(master, own.orig - mrow, col - mcol)
            };
            let mut head = "<f".to_string();
            // keep flags such as `aca`; drop the sharing
            for a in ["aca", "ca", "bx"] {
                if let Some(v) = raw_attr(tag, a) {
                    head.push_str(&format!(" {a}=\"{v}\""));
                }
            }
            head.push('>');
            (head, text)
        }
        None => {
            if te == fe {
                // `<f t="shared" si="0"/>`: no text, nothing to map
                return cell.to_string();
            }
            let head = match raw_attr(tag, "ref") {
                Some(v) => set_raw_attribute(tag, "ref", &rm.range_text(v, Some(own)).unwrap_or_else(|| v.to_string())),
                None => tag.to_string(),
            };
            (head, decode_xml(&cell[fe..te]).into_owned())
        }
    };
    let mapped = rm.formula(&text, Some(own), own_name);
    format!("{}{head}{}</f>{}", &cell[..fs], escape_xml(&mapped), &cell[end..])
}

/// Removes every `tag` element (open tag through its end).
fn drop_elements(xml: &str, tag: &str) -> String {
    rewrite_each(xml, tag, |_, _| None)
}

/// Everything after `</sheetData>`: merges, conditional formatting, data validation, hyperlinks,
/// the auto filter, page breaks, ignored errors and protected ranges.
fn rewrite_sheet_ranges(tail: &str, rm: &RowMap, own_name: Option<&str>) -> String {
    let mut out = tail.to_string();

    // merged cells: duplicate merges inside repeated rows, shift the others
    let merge_cells = tags(&out, "<mergeCells").next();
    if let Some((s, e)) = merge_cells {
        let end = element_end(&out, s, e).unwrap_or(out.len());
        let mut merges: Vec<String> = Vec::new();
        for (ms, me) in tags(&out[e..end], "<mergeCell") {
            let Some(r) = raw_attr(&out[e + ms..e + me], "ref") else { continue };
            let Some(((ca, ra), (cb, rb))) = split_range(r) else { continue };
            if let (Some(la), Some(lb)) = (rm.map.get(&ra), rm.map.get(&rb)) {
                if la.len() > 1 && rm.same_block(ra, rb) {
                    merges.extend(la.iter().zip(lb).map(|(x, y)| format!("{ca}{x}:{cb}{y}")));
                    continue;
                }
            }
            if let Some((x, y)) = rm.span(ra, rb, None) {
                merges.push(format!("{ca}{x}:{cb}{y}"));
            }
        }
        let block = if merges.is_empty() {
            String::new()
        } else {
            let inner: String = merges.iter().map(|r| format!("<mergeCell ref=\"{r}\"/>")).collect();
            format!("<mergeCells count=\"{}\">{inner}</mergeCells>", merges.len())
        };
        out.replace_range(s..end, &block);
    }

    let formulas = |body: &str| {
        let mut body = body.to_string();
        for (open, close) in FORMULA_ELEMENTS {
            body = rewrite_elements(&body, open, close, None, &|f| Some(rm.formula(f, None, own_name)));
        }
        body
    };
    // ranges (and formulas) of conditional formatting, data validation, ignored errors and
    // protected ranges; drop the ones whose rows are all gone
    for tag in ["<conditionalFormatting", "<dataValidation", "<ignoredError", "<protectedRange"] {
        out = rewrite_each(&out, tag, |head, body| {
            let Some(sqref) = raw_attr(head, "sqref") else { return Some(format!("{head}{body}")) };
            let list = rm.sqref(sqref)?;
            Some(format!("{}{}", set_raw_attribute(head, "sqref", &list), formulas(body)))
        });
    }
    out = fix_wrapper(&out, "<dataValidations", "<dataValidation", &["count"]);
    out = fix_wrapper(&out, "<ignoredErrors", "<ignoredError", &[]);
    out = fix_wrapper(&out, "<protectedRanges", "<protectedRange", &[]);

    // hyperlinks: shift, drop links on removed rows, follow links into this sheet
    out = rewrite_each(&out, "<hyperlink", |head, body| {
        let Some(r) = raw_attr(head, "ref") else { return Some(format!("{head}{body}")) };
        let mut head = set_raw_attribute(head, "ref", &rm.range_text(r, None)?);
        if let (Some(loc), Some(name)) = (raw_attr(&head, "location"), own_name) {
            let mapped = map_qualified(&decode_xml(loc), &[(name, rm)], None);
            head = set_raw_attribute(&head, "location", &escape_attr(&mapped));
        }
        Some(format!("{head}{body}"))
    });
    out = fix_wrapper(&out, "<hyperlinks", "<hyperlink", &[]);

    // the sheet's auto filter goes with its rows
    out = rewrite_each(&out, "<autoFilter", |head, body| {
        let Some(r) = raw_attr(head, "ref") else { return Some(format!("{head}{body}")) };
        Some(format!("{}{body}", set_raw_attribute(head, "ref", &rm.range_text(r, None)?)))
    });
    for t in ["<sortState", "<sortCondition"] {
        out = rewrite_attr(&out, t, "ref", &|v| rm.range_text(v, None));
    }

    // page breaks: `id` is the last row above the break
    out = rewrite_each(&out, "<rowBreaks", |head, body| {
        let body = rewrite_each(body, "<brk", |brk, rest| {
            let Some(id) = attr_num(brk, "id") else { return Some(format!("{brk}{rest}")) };
            Some(format!("{}{rest}", set_raw_attribute(brk, "id", &rm.row(id, true, None)?.to_string())))
        });
        Some(format!("{head}{body}"))
    });
    out = fix_wrapper(&out, "<rowBreaks", "<brk", &["count", "manualBreakCount"]);

    // conditional formatting and validation of newer Excel versions (x14 extensions)
    out = rewrite_elements(&out, "<xm:sqref", "</xm:sqref>", None, &|s| rm.sqref(s));
    out = rewrite_elements(&out, "<xm:f", "</xm:f>", None, &|f| Some(rm.formula(f, None, own_name)));
    out
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
        let bump = |s: &str| {
            crate::re!(r"\d+").replace_all(s, |c: &regex_lite::Captures| (c[0].parse::<i64>().unwrap() + 10).to_string()).into_owned()
        };
        let shift = |a: &str, b: Option<&str>| match b {
            Some(b) => format!("{}:{}", bump(a), bump(b)),
            None => bump(a),
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
