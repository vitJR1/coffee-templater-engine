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

/// Resolves typed scalars and renumbers rows. Returns the row map when rows moved; `finalize`
/// uses it to update references to this sheet from tables, defined names and other sheets.
pub fn postprocess(xml: &str, part: &str, state: &XlsxState) -> (String, Option<RowMap>) {
    let xml = resolve_scalar_markers(xml);
    let empty = BTreeSet::new();
    renumber_rows(&xml, state.template_rows.get(part).unwrap_or(&empty))
}

const REL_TABLE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";

/// Updates what lives outside the rendered sheets (`maps`: sheet part → row map), then drops the
/// calculation chain.
pub fn finalize(pkg: &mut Package, maps: &[(String, RowMap)]) {
    let wb = workbook_path(pkg);
    if !maps.is_empty() {
        let sheets = worksheet_parts(pkg);
        let named: Vec<(&str, &RowMap)> =
            sheets.iter().filter_map(|s| maps.iter().find(|(p, _)| *p == s.path).map(|(_, m)| (s.name.as_str(), m))).collect();
        // references to rendered sheets from any sheet (`Sheet1!A5`, `'My sheet'!A5`)
        for s in &sheets {
            let Some(xml) = pkg.get_text(&s.path) else { continue };
            let next = rewrite_elements(&xml, "<f", "</f>", None, &|f| map_qualified(f, &named));
            if next != xml {
                pkg.set_text(&s.path, &next);
            }
        }
        // defined names (print areas, named ranges)
        if let Some(xml) = pkg.get_text(&wb) {
            let next = rewrite_elements(&xml, "<definedName", "</definedName>", None, &|f| map_qualified(f, &named));
            if next != xml {
                pkg.set_text(&wb, &next);
            }
        }
        // tables on rendered sheets
        for (part, map) in maps {
            for (_, table) in pkg.related_parts(part, REL_TABLE) {
                let Some(xml) = pkg.get_text(&table) else { continue };
                let next = rewrite_ref_attrs(&xml, &["<table", "<autoFilter"], map);
                if next != xml {
                    pkg.set_text(&table, &next);
                }
            }
        }
    }
    finalize_calc(pkg, &wb);
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

/// Byte ranges `(start, end)` of every `<tag ...>` / `<tag .../>` (name followed by space, `>` or `/`).
fn tags<'a>(xml: &'a str, tag: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    let b = xml.as_bytes();
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(p) = xml[from..].find(tag).map(|p| p + from) {
            from = p + tag.len();
            if !matches!(b.get(p + tag.len()), Some(c) if c.is_ascii_whitespace() || *c == b'>' || *c == b'/') {
                continue;
            }
            let end = xml[p..].find('>')? + p + 1;
            from = end;
            return Some((p, end));
        }
        None
    })
}

/// Rewrites the text of every `<open ...>text</close>` element (and optionally its open tag).
fn rewrite_elements(xml: &str, open: &str, close: &str, head: Option<&dyn Fn(&str) -> String>, text: &dyn Fn(&str) -> String) -> String {
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
        out.push_str(&escape_xml(&text(&decode_xml(&xml[e..c]))));
        last = c;
        skip_to = c;
    }
    out.push_str(&xml[last..]);
    out
}

/// Maps the `ref` attribute of the given tags.
fn rewrite_ref_attrs(xml: &str, names: &[&str], map: &RowMap) -> String {
    let mut out = xml.to_string();
    for name in names {
        let mut next = String::with_capacity(out.len());
        let mut last = 0;
        for (s, e) in tags(&out, name) {
            let tag = &out[s..e];
            next.push_str(&out[last..s]);
            match raw_attr(tag, "ref") {
                Some(r) => next.push_str(&set_raw_attribute(tag, "ref", &map.range_text(r, None).unwrap_or_else(|| r.to_string()))),
                None => next.push_str(tag),
            }
            last = e;
        }
        next.push_str(&out[last..]);
        out = next;
    }
    out
}

/// Splits `$A$12` into (`$A$`, 12).
fn split_ref(r: &str) -> Option<(&str, i64)> {
    let b = r.as_bytes();
    let end = scan_ref(b, 0)?;
    if end != b.len() {
        return None;
    }
    let digits = b.iter().position(u8::is_ascii_digit)?;
    Some((&r[..digits], r[digits..].parse().ok()?))
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
        let gt = rest.find('>')?;
        if rest[..gt].ends_with('/') {
            return None;
        }
        let close = rest.find("</f>")?;
        if rest[gt + 1..close].contains('<') {
            return None;
        }
        formula = &rest[..close + 4];
        rest = skip_ws(&rest[close + 4..]);
    }
    rest = skip_ws(rest.strip_prefix("<is>")?);
    if !rest.starts_with("<t") || !matches!(rest.as_bytes().get(2), Some(c) if c.is_ascii_whitespace() || *c == b'>') {
        return None;
    }
    rest = &rest[rest.find('>')? + 1..];
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

fn strip_gaps(xml: &str) -> String {
    let needle = " data-ct-gap=\"";
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(p) = rest.find(needle) {
        out.push_str(&rest[..p]);
        let after = &rest[p + needle.len()..];
        rest = after.find('"').map(|q| &after[q + 1..]).unwrap_or(after);
    }
    out.push_str(rest);
    out
}

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

impl RowMap {
    /// Maps an original row to its new number; `end` = last copy (for range ends).
    /// `None` when the row was removed by an empty loop.
    fn row(&self, r: i64, end: bool, own: Option<(i64, i64)>) -> Option<i64> {
        if let Some((o, n)) = own {
            if r == o {
                return Some(n);
            }
        }
        if let Some(list) = self.map.get(&r) {
            return Some(if end { *list.last()? } else { list[0] });
        }
        if self.removed.contains(&r) {
            return None;
        }
        // A row that is not in the template (empty row): shift by the delta of the closest row above.
        let at = self.sorted.partition_point(|o| *o < r);
        if at == 0 {
            return Some(r);
        }
        let base = self.sorted[at - 1];
        let l = &self.map[&base];
        Some(r + (l[l.len() - 1] - base))
    }

    /// Maps a row span, clamping ends that fall on removed rows. `None` when every row is gone.
    fn span(&self, a: i64, b: i64, own: Option<(i64, i64)>) -> Option<(i64, i64)> {
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

    /// Maps `A1` or `A1:B5` text. `None` when the referenced rows were removed.
    fn ref_text(&self, a: &str, b: Option<&str>, own: Option<(i64, i64)>) -> Option<String> {
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

    fn range_text(&self, r: &str, own: Option<(i64, i64)>) -> Option<String> {
        match r.split_once(':') {
            None => self.ref_text(r, None, own),
            Some((a, b)) => self.ref_text(a, Some(b), own),
        }
    }

    fn formula(&self, f: &str, own: (i64, i64)) -> String {
        map_formula(f, &|a, b| self.ref_text(a, b, Some(own)).unwrap_or_else(|| "#REF!".into()))
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

/// Rewrites unqualified cell references of a formula (`A1`, `$B$2:C5`). String literals and
/// references to sheets (`Sheet2!A1`, `'My sheet'!A1`) are left untouched. `map` receives a
/// reference or both ends of a range and returns the replacement.
pub fn map_formula(f: &str, map: &dyn Fn(&str, Option<&str>) -> String) -> String {
    let b = f.as_bytes();
    let mut out = String::with_capacity(f.len());
    let mut seg = 0;
    let mut i = 0;
    while i < b.len() {
        let skip_to = match b[i] {
            b'"' => string_end(b, i, b'"'),
            b'\'' => string_end(b, i, b'\'').and_then(|e| {
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
        if !(b[i] == b'$' || b[i].is_ascii_uppercase()) {
            i += 1;
            continue;
        }
        let prev = if i == 0 { None } else { Some(b[i - 1]) };
        if matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'.') {
            i += 1;
            continue;
        }
        let Some(r1) = scan_ref(b, i) else {
            i += 1;
            continue;
        };
        let r2 = if b.get(r1) == Some(&b':') { scan_ref(b, r1 + 1).filter(|e| ref_boundary_ok(b, *e)) } else { None };
        if prev == Some(b'!') {
            // qualified reference (Sheet2!A1:B2): another sheet's rows, skip it whole
            i = r2.unwrap_or(r1);
            continue;
        }
        let end = match r2 {
            Some(r2) => r2,
            None if ref_boundary_ok(b, r1) => r1,
            None => {
                i += 1;
                continue;
            }
        };
        out.push_str(&s[last..i]);
        out.push_str(&map(&s[i..r1], r2.map(|r2| &s[r1 + 1..r2])));
        i = end;
        last = end;
    }
    out.push_str(&s[last..]);
}

/// Rewrites references qualified with one of `sheets` (`Sheet1!A5`, `'My sheet'!A1:B2`).
fn map_qualified(f: &str, sheets: &[(&str, &RowMap)]) -> String {
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
            i = bang + 1;
            continue;
        };
        let Some(r1) = scan_ref(b, bang + 1) else {
            i = bang + 1;
            continue;
        };
        let r2 = if b.get(r1) == Some(&b':') { scan_ref(b, r1 + 1) } else { None };
        let end = r2.unwrap_or(r1);
        let mapped = map.ref_text(&f[bang + 1..r1], r2.map(|r2| &f[r1 + 1..r2]), None);
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

fn tag_attr_num(attrs: &str, name: &str) -> Option<i64> {
    raw_attr(&format!("<x{attrs}>"), name).and_then(|v| v.parse().ok())
}

fn renumber_rows(xml: &str, template_rows: &BTreeSet<i64>) -> (String, Option<RowMap>) {
    let Some(sd_start) = xml.find("<sheetData") else { return (xml.to_string(), None) };
    let Some(sd_end) = xml.find("</sheetData>").filter(|e| *e >= sd_start) else { return (strip_gaps(xml), None) };
    let body = &xml[sd_start..sd_end];
    let spans = rows(body);

    // pass 1: assign new row numbers
    let mut numbers: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    let mut cur = 0i64;
    let mut prev_orig = 0i64;
    for r in &spans {
        let orig = tag_attr_num(r.attrs, "r").unwrap_or(prev_orig + 1);
        // a copy of a looped row starts right after the previous copy
        let gap = if orig <= prev_orig { 1 } else { tag_attr_num(r.attrs, GAP_ATTR).unwrap_or(1) };
        cur += gap;
        numbers.push((orig, cur));
        prev_orig = orig;
    }
    let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
    for (o, n) in &numbers {
        map.entry(*o).or_default().push(*n);
    }
    let removed: BTreeSet<i64> = template_rows.iter().copied().filter(|r| !map.contains_key(r)).collect();
    let changed = numbers.iter().any(|(o, n)| o != n) || !removed.is_empty() || numbers.len() != map.len();
    if !changed {
        return (strip_gaps(xml), None);
    }
    let mut sorted: Vec<i64> = map.keys().copied().collect();
    sorted.sort_unstable();
    let rm = RowMap { map, sorted, removed };

    // pass 2: rewrite rows, cells and formulas
    let mut out = String::with_capacity(xml.len() + xml.len() / 8);
    out.push_str(&xml[..sd_start]);
    let mut last = 0;
    for (r, &(orig, next)) in spans.iter().zip(&numbers) {
        out.push_str(&body[last..r.start]);
        last = r.end;
        let close = if r.inner.is_none() { "/>" } else { ">" };
        let head = remove_raw_attribute(&format!("<row{}{close}", r.attrs), GAP_ATTR);
        out.push_str(&set_raw_attribute(&head, "r", &next.to_string()));
        let Some(inner) = r.inner else { continue };
        let next_s = next.to_string();
        let mut cells = String::with_capacity(inner.len() + 16);
        let mut cl = 0;
        for (s, e) in tags(inner, "<c") {
            let tag = &inner[s..e];
            cells.push_str(&inner[cl..s]);
            cl = e;
            match raw_attr(tag, "r") {
                Some(v) => {
                    let col = v.trim_end_matches(|c: char| c.is_ascii_digit());
                    cells.push_str(&set_raw_attribute(tag, "r", &format!("{col}{next_s}")));
                }
                None => cells.push_str(tag),
            }
        }
        cells.push_str(&inner[cl..]);
        if cells.contains("<f") {
            let own = (orig, next);
            let ref_head = |tag: &str| match raw_attr(tag, "ref") {
                Some(v) => set_raw_attribute(tag, "ref", &rm.range_text(v, Some(own)).unwrap_or_else(|| v.to_string())),
                None => tag.to_string(),
            };
            cells = rewrite_elements(&cells, "<f", "</f>", Some(&ref_head), &|f| rm.formula(f, own));
        }
        out.push_str(&cells);
        out.push_str("</row>");
    }
    out.push_str(&body[last..]);
    out.push_str(&xml[sd_end..]);

    let out = rewrite_sheet_ranges(&out, &rm);
    (out, Some(rm))
}

/// Merges, conditional formatting, data validation, hyperlinks and the auto filter.
fn rewrite_sheet_ranges(xml: &str, rm: &RowMap) -> String {
    let mut out = xml.to_string();

    // merged cells: duplicate merges inside repeated rows, shift the others
    if let (Some(s), Some(e)) = (out.find("<mergeCells"), out.find("</mergeCells>")) {
        let mut merges: Vec<String> = Vec::new();
        for (ms, me) in tags(&out[s..e], "<mergeCell") {
            let Some(r) = raw_attr(&out[s + ms..s + me], "ref") else { continue };
            let (a, b) = r.split_once(':').unwrap_or((r, r));
            let (Some((ca, ra)), Some((cb, rb))) = (split_ref(a), split_ref(b)) else { continue };
            if ra == rb {
                if let Some(list) = rm.map.get(&ra) {
                    merges.extend(list.iter().map(|n| format!("{ca}{n}:{cb}{n}")));
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
        out = format!("{}{}{}", &out[..s], block, &out[e + "</mergeCells>".len()..]);
    }

    // ranges in conditional formatting and data validation; drop the ones whose rows are all gone
    for (name, close) in [("<conditionalFormatting", "</conditionalFormatting>"), ("<dataValidation", "</dataValidation>")] {
        let mut next = String::with_capacity(out.len());
        let mut last = 0;
        let mut skip_to = 0;
        for (s, e) in tags(&out, name) {
            if s < skip_to {
                continue;
            }
            let tag = &out[s..e];
            let Some(sqref) = raw_attr(tag, "sqref") else { continue };
            let list: Vec<String> = sqref.split_whitespace().filter_map(|r| rm.range_text(r, None)).collect();
            let elem_end = if tag.ends_with("/>") { e } else { out[e..].find(close).map(|c| e + c + close.len()).unwrap_or(e) };
            next.push_str(&out[last..s]);
            if !list.is_empty() {
                next.push_str(&set_raw_attribute(tag, "sqref", &list.join(" ")));
                next.push_str(&out[e..elem_end]);
            }
            last = elem_end;
            skip_to = elem_end;
        }
        next.push_str(&out[last..]);
        out = next;
    }

    // hyperlinks: shift, drop links to removed rows
    let mut next = String::with_capacity(out.len());
    let mut last = 0;
    for (s, e) in tags(&out, "<hyperlink") {
        let tag = &out[s..e];
        let Some(r) = raw_attr(tag, "ref") else { continue };
        next.push_str(&out[last..s]);
        if let Some(m) = rm.range_text(r, None) {
            next.push_str(&set_raw_attribute(tag, "ref", &m));
        }
        last = e;
    }
    next.push_str(&out[last..]);
    out = rewrite_ref_attrs(&next, &["<autoFilter"], rm);

    // the used-range hint is optional; drop it rather than keep a wrong one
    let dimension = tags(&out, "<dimension").next();
    if let Some((s, e)) = dimension {
        out.replace_range(s..e, "");
    }
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
