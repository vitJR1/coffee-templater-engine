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
    /// Sheet part → what rendering needs to know about the template sheet.
    pub sheets: HashMap<String, SheetInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct SheetInfo {
    name: String,
    /// Row numbers present in the template sheet.
    rows: BTreeSet<i64>,
    /// Shared formula masters: si → (formula, column, row).
    masters: HashMap<String, (String, i64, i64)>,
    /// First and last data row of each table on the sheet.
    table_data: Vec<(i64, i64)>,
}

fn tag_attr(attrs: &str, name: &str) -> Option<String> {
    raw_attr(&format!("<x{attrs}>"), name).map(str::to_string)
}

/// A row number from the document: 1..=1048576, anything else is treated as missing.
fn row_num(v: &str) -> Option<i64> {
    v.trim().parse::<i64>().ok().filter(|r| (1..=MAX_ROW).contains(r))
}

/// A 0-based row index from the document (drawing anchors, VML).
fn row_index(v: &str) -> Option<i64> {
    v.trim().parse::<i64>().ok().filter(|r| (0..MAX_ROW).contains(r))
}

/// Column number of `A`..`XFD` (1-based); 0 for anything else.
fn col_number(letters: &str) -> i64 {
    let l = letters.trim_matches('$');
    if l.is_empty() || l.len() > 3 || !l.bytes().all(|c| c.is_ascii_uppercase()) {
        return 0;
    }
    i64::from(crate::model::xlsx::column_to_index(l))
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
    // 2. Remember the template rows; give every row a valid explicit number, so copies made by
    //    loops can be told apart from the rows that follow them.
    let mut info = SheetInfo {
        name: worksheet_parts(pkg).into_iter().find(|s| s.path == part).map(|s| s.name).unwrap_or_default(),
        ..Default::default()
    };
    let mut out = String::with_capacity(xml.len() + 64);
    let mut last = 0;
    let mut prev = 0i64;
    for (s, e) in tags(&xml, "<row") {
        let tag = &xml[s..e];
        let r = match raw_attr(tag, "r").and_then(row_num).filter(|r| *r > prev) {
            Some(r) => r,
            None => {
                let r = (prev + 1).min(MAX_ROW);
                out.push_str(&xml[last..s]);
                out.push_str(&set_raw_attribute(tag, "r", &r.to_string()));
                last = e;
                r
            }
        };
        info.rows.insert(r);
        prev = r;
    }
    out.push_str(&xml[last..]);
    // 3. Shared formula masters: groups touched by loops are written out when rendering, also
    //    when the master's own row is gone.
    for r in rows(&out) {
        let (Some(inner), Some(row)) = (r.inner, raw_attr(r.attrs, "r").and_then(row_num)) else { continue };
        for (s, e, end) in cells(inner) {
            let cell = &inner[s..end];
            let Some((fs, fe, te, _)) = formula_of(cell) else { continue };
            let tag = &cell[fs..fe];
            if raw_attr(tag, "t") != Some("shared") || raw_attr(tag, "ref").is_none() {
                continue;
            }
            let (Some(si), Some(col)) = (raw_attr(tag, "si"), raw_attr(&inner[s..e], "r")) else { continue };
            let col = col_number(col.trim_end_matches(|c: char| c.is_ascii_digit()));
            info.masters.entry(si.to_string()).or_insert_with(|| (decode_xml(&cell[fe..te]).into_owned(), col, row));
        }
    }
    // 4. Tables: one data row is kept when a loop removes all of them.
    for (_, table) in pkg.related_parts(part, REL_TABLE) {
        let Some(t) = pkg.get_text(&table) else { continue };
        let Some((s, e)) = tags(&t, "<table").next() else { continue };
        let tag = &t[s..e];
        let Some(((_, y1), (_, y2))) = raw_attr(tag, "ref").and_then(split_range) else { continue };
        let count = |n: &str, d: i64| raw_attr(tag, n).and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(d).clamp(0, MAX_ROW);
        let (first, last) = (y1 + count("headerRowCount", 1), y2 - count("totalsRowCount", 0));
        if first <= last {
            info.table_data.push((first, last));
        }
    }
    state.sheets.insert(part.to_string(), info);
    out
}

/// Resolves typed scalars and renumbers rows. Returns the row map when rows moved; `finalize`
/// uses it to update references to this sheet from tables, defined names and other sheets.
pub fn postprocess(xml: &str, part: &str, state: &XlsxState) -> (String, Option<RowMap>) {
    let xml = resolve_scalar_markers(xml);
    match state.sheets.get(part) {
        Some(info) => renumber_rows(&xml, info),
        None => (xml, None),
    }
}

const REL_TABLE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";
const REL_COMMENTS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const REL_THREADED: &str = "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";
const REL_VML: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
const REL_DRAWING: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing";
const REL_CTRL_PROP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/ctrlProp";
const REL_PIVOT_TABLE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable";

/// Last row of a worksheet.
const MAX_ROW: i64 = 1_048_576;

fn update_text(pkg: &mut Package, path: &str, f: impl FnOnce(String) -> String) {
    let Some(xml) = pkg.get_text(path) else { return };
    let next = f(xml.clone());
    if next != xml {
        pkg.set_text(path, &next);
    }
}

/// Internal relationships of a part: (type, id, target path). One read of the `.rels` file.
fn rel_targets(pkg: &Package, part: &str) -> Vec<(String, String, String)> {
    pkg.relationships(part)
        .into_iter()
        .filter(|r| r.target_mode.as_deref() != Some("External"))
        .map(|r| {
            let p = Package::resolve_target(part, &r.target);
            (r.rel_type, r.id, p)
        })
        .collect()
}

fn targets<'a>(rels: &'a [(String, String, String)], rel_type: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
    rels.iter().filter(move |(t, _, _)| t == rel_type).map(|(_, id, p)| (id.as_str(), p.as_str()))
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
                update_text(pkg, &s.path, |mut xml| {
                    if !xml.contains('!') {
                        return xml;
                    }
                    for (open, close) in FORMULA_ELEMENTS {
                        xml = rewrite_elements(xml, open, close, &|f| qualified(f, &others));
                    }
                    rewrite_attr(xml, "<hyperlink", "location", &|v| qualified(v, &others))
                });
            }
        }
        // chart series (embedded charts, chart sheets, chartEx); the series point to sheets by name
        let charts: Vec<String> = pkg
            .names()
            .filter(|n| n.starts_with("xl/charts/") && n.ends_with(".xml") && !n.contains("/_rels/"))
            .map(str::to_string)
            .collect();
        for chart in charts {
            update_text(pkg, &chart, |mut xml| {
                for (open, close) in [("<c:f", "</c:f>"), ("<cx:f", "</cx:f>"), ("<c15:f", "</c15:f>"), ("<c15:sqref", "</c15:sqref>")] {
                    xml = rewrite_elements(xml, open, close, &|f| qualified(f, &named));
                }
                xml
            });
        }
        // pivot cache sources on rendered sheets
        let caches: Vec<String> = pkg.names().filter(|n| n.starts_with("xl/pivotCache/pivotCacheDefinition")).map(str::to_string).collect();
        for cache in caches {
            update_text(pkg, &cache, |xml| {
                rewrite_each(xml, "<worksheetSource", |tag, body| {
                    let (Some(sheet), Some(r)) = (raw_attr(tag, "sheet"), raw_attr(tag, "ref")) else { return Edit::Keep };
                    let sheet = decode_xml(sheet);
                    let Some((_, m)) = named.iter().find(|(n, _)| n.eq_ignore_ascii_case(&sheet)) else { return Edit::Keep };
                    match m.range_text(r, None) {
                        Some(n) => Edit::Replace(format!("{}{body}", set_raw_attribute(tag, "ref", &n))),
                        None => Edit::Keep,
                    }
                })
            });
        }
        // defined names (print areas, named ranges)
        update_text(pkg, &wb, |xml| rewrite_elements(xml, "<definedName", "</definedName>", &|f| qualified(f, &named)));
        for (part, map) in maps {
            let name = sheets.iter().find(|s| s.path == *part).map(|s| s.name.as_str()).unwrap_or("");
            finalize_sheet_parts(pkg, part, name, map, &named);
        }
    }
    finalize_calc(pkg, &wb);
}

/// Parts that belong to one rendered sheet: tables, comments, drawings, form controls, pivot tables.
fn finalize_sheet_parts(pkg: &mut Package, part: &str, name: &str, map: &RowMap, named: &[(&str, &RowMap)]) {
    let rels = rel_targets(pkg, part);
    for (id, table) in targets(&rels, REL_TABLE) {
        finalize_table(pkg, part, id, table, map);
    }
    // comments on removed rows go, the others follow their cells
    for rel_type in [REL_COMMENTS, REL_THREADED] {
        for (_, comments) in targets(&rels, rel_type) {
            let tag = if rel_type == REL_COMMENTS { "<comment" } else { "<threadedComment" };
            update_text(pkg, comments, |xml| {
                rewrite_each(xml, tag, |head, body| {
                    let Some(r) = raw_attr(head, "ref") else { return Edit::Keep };
                    match map.range_text(r, None) {
                        Some(n) => Edit::Replace(format!("{}{body}", set_raw_attribute(head, "ref", &n))),
                        None => Edit::Drop,
                    }
                })
            });
        }
    }
    let formula = |f: &str| {
        let own = map.formula(f, None, Some(name));
        Some(if own.contains('!') { map_qualified(&own, named, None) } else { own })
    };
    for (_, vml) in targets(&rels, REL_VML) {
        update_text(pkg, vml, |xml| rewrite_each(xml, "<v:shape", |tag, body| vml_shape(tag, body, map, &formula)));
    }
    for (_, ctrl) in targets(&rels, REL_CTRL_PROP) {
        update_text(pkg, ctrl, |mut xml| {
            for a in ["fmlaLink", "fmlaRange", "fmlaTxbx", "fmlaGroup"] {
                xml = rewrite_attr(xml, "<formControlPr", a, &formula);
            }
            xml
        });
    }
    for (_, drawing) in targets(&rels, REL_DRAWING) {
        update_text(pkg, drawing, |mut xml| {
            for anchor in ["<xdr:twoCellAnchor", "<xdr:oneCellAnchor"] {
                xml = rewrite_each(xml, anchor, |tag, body| move_anchor(tag, body, "xdr:", map));
            }
            xml
        });
    }
    for (_, pivot) in targets(&rels, REL_PIVOT_TABLE) {
        update_text(pkg, pivot, |xml| rewrite_attr(xml, "<location", "ref", &|v| map.range_text(v, None)));
    }
}

/// A drawing or control anchor: `from` follows its row; `to` follows its row only for anchors that
/// resize with cells (`twoCell`), otherwise the picture keeps its size. `section` is the prefix of
/// the `from`/`to` elements (`xdr:` in drawings, none for controls in the sheet).
fn move_anchor(tag: &str, body: &str, section: &str, map: &RowMap) -> Edit {
    let row_in = |sub: &str| -> Option<(usize, usize, i64)> {
        let (_, e) = tags(sub, "<xdr:row").next()?;
        let c = sub[e..].find("</xdr:row>")? + e;
        Some((e, c, row_index(&sub[e..c])?))
    };
    let section = |name: &str| -> Option<(usize, usize)> {
        let open = format!("<{section}{name}");
        let (s, e) = tags(body, &open).next()?;
        Some((s, element_end(body, s, e)?))
    };
    let resize = match raw_attr(tag, "editAs") {
        Some("oneCell" | "absolute") => false,
        Some(_) => true,
        // form controls: <anchor sizeWithCells="1">
        None => tag.starts_with("<xdr:") || matches!(raw_attr(tag, "sizeWithCells"), Some("1" | "true")),
    };
    let Some((fs, fe)) = section("from") else { return Edit::Keep };
    let Some((rs, re, from)) = row_in(&body[fs..fe]) else { return Edit::Keep };
    let new_from = map.row_or_shift(from + 1) - 1;
    let mut edits = vec![(fs + rs, fs + re, new_from)];
    if let Some((ts, te)) = section("to") {
        if let Some((rs, re, to)) = row_in(&body[ts..te]) {
            let new_to = if resize { (map.row_or_shift(to + 1) - 1).max(new_from) } else { (to + new_from - from).clamp(0, MAX_ROW - 1) };
            edits.push((ts + rs, ts + re, new_to));
        }
    }
    let mut out = body.to_string();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (s, e, n) in edits {
        out.replace_range(s..e, &n.to_string());
    }
    Edit::Replace(format!("{tag}{out}"))
}

/// A legacy VML shape: notes (`x:Row`) and form controls follow their rows; notes of removed
/// rows go. Linked cells and ranges are mapped as formulas.
fn vml_shape(tag: &str, body: &str, map: &RowMap, formula: &dyn Fn(&str) -> Option<String>) -> Edit {
    if !body.contains("<x:ClientData") {
        return Edit::Keep;
    }
    let text_of = |b: &str, name: &str| -> Option<(usize, usize)> {
        let open = format!("<{name}>");
        let s = b.find(&open)? + open.len();
        Some((s, b[s..].find(&format!("</{name}>"))? + s))
    };
    let mut body = body.to_string();
    // the shape's top row decides how far it moves
    let anchor = text_of(&body, "x:Anchor");
    let vals: Vec<String> = anchor.map(|(s, e)| body[s..e].split(',').map(|v| v.trim().to_string()).collect()).unwrap_or_default();
    let delta = match text_of(&body, "x:Row").and_then(|(s, e)| Some((s, e, row_index(&body[s..e])?))) {
        Some((s, e, old)) => {
            // a note: it goes with its cell
            let Some(new) = map.row(old + 1, false, None) else { return Edit::Drop };
            body.replace_range(s..e, &(new - 1).to_string());
            new - 1 - old
        }
        None => match vals.get(2).and_then(|v| row_index(v)) {
            Some(top) => map.row_or_shift(top + 1) - 1 - top,
            None => 0,
        },
    };
    if let (Some(_), true) = (anchor, delta != 0 && vals.len() == 8) {
        let moved: Vec<String> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| match (i, row_index(v)) {
                (2 | 6, Some(n)) => (n + delta).clamp(0, MAX_ROW - 1).to_string(),
                _ => v.clone(),
            })
            .collect();
        if let Some((s, e)) = text_of(&body, "x:Anchor") {
            body.replace_range(s..e, &moved.join(", "));
        }
    }
    for name in ["x:FmlaLink", "x:FmlaRange", "x:FmlaGroup", "x:FmlaTxbx"] {
        if let Some((s, e)) = text_of(&body, name) {
            if let Some(f) = formula(&decode_xml(&body[s..e])) {
                body.replace_range(s..e, &escape_xml(&f));
            }
        }
    }
    Edit::Replace(format!("{tag}{body}"))
}

/// A table on a rendered sheet: its ranges follow the rows (a data row emptied by a loop was kept
/// by `renumber_rows`); a table whose rows are all gone is removed.
fn finalize_table(pkg: &mut Package, part: &str, id: &str, table: &str, map: &RowMap) {
    let Some(xml) = pkg.get_text(table) else { return };
    let Some((s, e)) = tags(&xml, "<table").next() else { return };
    let tag = &xml[s..e];
    let Some(old) = raw_attr(tag, "ref") else { return };
    let Some(new) = map.range_text(old, None) else {
        pkg.remove(table);
        pkg.remove_override(table);
        pkg.remove_relationships(part, |x| x.id == id);
        update_text(pkg, part, |sheet| {
            let sheet = rewrite_each(sheet, "<tablePart", |t, _| if raw_attr(t, "r:id") == Some(id) { Edit::Drop } else { Edit::Keep });
            fix_wrapper(sheet, "<tableParts", "<tablePart", &["count"])
        });
        return;
    };
    let out = format!("{}{}{}", &xml[..s], set_raw_attribute(tag, "ref", &new), &xml[e..]);
    let out = rewrite_attr(out, "<autoFilter", "ref", &|v| map.range_text(v, None));
    let out = map_sort_state(out, map);
    if out != xml {
        pkg.set_text(table, &out);
    }
}

/// Sort state follows its rows; conditions on removed rows go, and so does a sort state left
/// without a range or conditions.
fn map_sort_state(xml: String, map: &RowMap) -> String {
    rewrite_each(xml, "<sortState", |tag, body| {
        let Some(r) = raw_attr(tag, "ref") else { return Edit::Keep };
        let Some(n) = map.range_text(r, None) else { return Edit::Drop };
        let body = rewrite_each(body.to_string(), "<sortCondition", |c, rest| match raw_attr(c, "ref") {
            None => Edit::Keep,
            Some(r) => match map.range_text(r, None) {
                Some(n) => Edit::Replace(format!("{}{rest}", set_raw_attribute(c, "ref", &n))),
                None => Edit::Drop,
            },
        });
        if tags(&body, "<sortCondition").next().is_none() {
            return Edit::Drop;
        }
        Edit::Replace(format!("{}{body}", set_raw_attribute(tag, "ref", &n)))
    })
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

/// What to do with an element.
enum Edit {
    Keep,
    Replace(String),
    Drop,
}

/// Rewrites the text of every `<open ...>text</close>` element. `text` gets the decoded text and
/// returns `None` to keep it as it is.
fn rewrite_elements(xml: String, open: &str, close: &str, text: &dyn Fn(&str) -> Option<String>) -> String {
    if !xml.contains(open) {
        return xml;
    }
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    let mut skip_to = 0;
    for (s, e) in tags(&xml, open) {
        if s < skip_to || xml[..e].ends_with("/>") {
            continue;
        }
        let Some(c) = xml[e..].find(close).map(|c| c + e) else { break };
        if let Some(t) = text(&decode_xml(&xml[e..c])) {
            out.push_str(&xml[last..e]);
            out.push_str(&escape_xml(&t));
            last = c;
        }
        skip_to = c;
    }
    if last == 0 {
        return xml;
    }
    out.push_str(&xml[last..]);
    out
}

/// Rewrites attribute `name` of every `tag`; `map` gets the decoded value and returns `None` to
/// keep it.
fn rewrite_attr(xml: String, tag: &str, name: &str, map: &dyn Fn(&str) -> Option<String>) -> String {
    if !xml.contains(tag) {
        return xml;
    }
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    for (s, e) in tags(&xml, tag) {
        let t = &xml[s..e];
        let Some(v) = raw_attr(t, name) else { continue };
        let Some(n) = map(&decode_xml(v)) else { continue };
        out.push_str(&xml[last..s]);
        out.push_str(&set_raw_attribute(t, name, &escape_attr(&n)));
        last = e;
    }
    if last == 0 {
        return xml;
    }
    out.push_str(&xml[last..]);
    out
}

/// Calls `f(open_tag, rest_of_element)` for every `tag` element and applies the edit.
fn rewrite_each(xml: String, tag: &str, mut f: impl FnMut(&str, &str) -> Edit) -> String {
    if !xml.contains(tag) {
        return xml;
    }
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    let mut skip_to = 0;
    let mut changed = false;
    for (s, e) in tags(&xml, tag) {
        if s < skip_to {
            continue;
        }
        let Some(end) = element_end(&xml, s, e) else { break };
        skip_to = end;
        let edit = f(&xml[s..e], &xml[e..end]);
        if matches!(edit, Edit::Keep) {
            continue;
        }
        out.push_str(&xml[last..s]);
        if let Edit::Replace(x) = edit {
            out.push_str(&x);
        }
        last = end;
        changed = true;
    }
    if !changed {
        return xml;
    }
    out.push_str(&xml[last..]);
    out
}

/// Drops every `wrapper` element left without `child` elements (the schema requires at least
/// one), and updates the count attributes of the others.
fn fix_wrapper(xml: String, wrapper: &str, child: &str, counts: &[&str]) -> String {
    rewrite_each(xml, wrapper, |head, inner| {
        let n = tags(inner, child).count();
        if n == 0 {
            return Edit::Drop;
        }
        let mut h = head.to_string();
        for c in counts {
            if raw_attr(&h, c).is_some() {
                let v = if *c == "manualBreakCount" {
                    tags(inner, child).filter(|(a, b)| matches!(raw_attr(&inner[*a..*b], "man"), Some("1" | "true"))).count()
                } else {
                    n
                };
                h = set_raw_attribute(&h, c, &v.to_string());
            }
        }
        if h == head {
            Edit::Keep
        } else {
            Edit::Replace(format!("{h}{inner}"))
        }
    })
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

fn is_cell(r: &str) -> bool {
    scan_cell(r.as_bytes(), 0) == Some(r.len())
}

/// A reference split into its column part and row: (`$A$`, 12).
type RefParts<'a> = (&'a str, i64);

/// Splits `A2:C5` (or `A2`) into its two ends.
fn split_range(r: &str) -> Option<(RefParts<'_>, RefParts<'_>)> {
    let (a, b) = r.split_once(':').unwrap_or((r, r));
    Some((split_ref(a)?, split_ref(b)?))
}

// ------------------------------------------------------------------------------------------
// Formulas
// ------------------------------------------------------------------------------------------

/// What a reference covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `A1` or `A1:B2`
    Cells,
    /// `5:7`
    Rows,
    /// `A:C`
    Cols,
}

/// `$?[A-Z]{1,3}$?[0-9]+` (row 1..=1048576) at `i`; returns the end offset.
fn scan_cell(b: &[u8], i: usize) -> Option<usize> {
    let k = scan_col(b, i)?;
    scan_row(b, k)
}

/// `$?[A-Z]{1,3}` at `i`; returns the end offset.
fn scan_col(b: &[u8], i: usize) -> Option<usize> {
    let mut k = i;
    if b.get(k) == Some(&b'$') {
        k += 1;
    }
    let ls = k;
    while k < b.len() && b[k].is_ascii_uppercase() {
        k += 1;
    }
    (k > ls && k - ls <= 3).then_some(k)
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

/// A cell, cell range, row range (`5:7`) or column range (`A:C`) at `i`: (kind, end of the first
/// part, end of the second part).
fn scan_ref_token(b: &[u8], i: usize) -> Option<(Kind, usize, Option<usize>)> {
    if let Some(r1) = scan_cell(b, i) {
        if b.get(r1) == Some(&b':') {
            if let Some(r2) = scan_cell(b, r1 + 1).filter(|e| ref_boundary_ok(b, *e)) {
                return Some((Kind::Cells, r1, Some(r2)));
            }
        }
        return ref_boundary_ok(b, r1).then_some((Kind::Cells, r1, None));
    }
    if let Some(r1) = scan_row(b, i) {
        let r2 = (b.get(r1) == Some(&b':')).then(|| scan_row(b, r1 + 1)).flatten().filter(|e| ref_boundary_ok(b, *e))?;
        return Some((Kind::Rows, r1, Some(r2)));
    }
    let c1 = scan_col(b, i)?;
    let c2 = (b.get(c1) == Some(&b':')).then(|| scan_col(b, c1 + 1)).flatten().filter(|e| ref_boundary_ok(b, *e))?;
    Some((Kind::Cols, c1, Some(c2)))
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

fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c >= 0x80
}

fn name_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && name_char(b[i]) {
        i += 1;
    }
    i
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

/// Rewrites the references of a formula. `f(sheet, kind, a, b)` gets the sheet name of a
/// qualified reference (`Sheet2!A1`, `'My sheet'!A:A`), what the reference covers and its one or
/// two ends; it returns the new reference, or `None` to keep it. A qualified reference replaced by
/// `#REF!` loses its sheet. String literals, structured references (`Table1[Col]`), names,
/// references to other workbooks (`[1]Sheet!A1`) and 3D references (`S1:S3!A1`) are skipped.
/// `(sheet, kind, first end, second end) -> replacement` for [`walk_refs`].
type RefFn<'a> = dyn FnMut(Option<&str>, Kind, &str, Option<&str>) -> Option<String> + 'a;

fn walk_refs(src: &str, f: &mut RefFn) -> String {
    let b = src.as_bytes();
    let mut out = String::new();
    let mut last = 0;
    let mut i = 0;
    let mut emit = |out: &mut String, last: &mut usize, sheet: Option<&str>, start: usize, at: usize| -> usize {
        let Some((kind, r1, r2)) = scan_ref_token(b, at) else { return token_end(b, at).max(at) };
        let end = r2.unwrap_or(r1);
        if let Some(new) = f(sheet, kind, &src[at..r1], r2.map(|r2| &src[r1 + 1..r2])) {
            let from = if sheet.is_some() && new == "#REF!" { start } else { at };
            out.push_str(&src[*last..from]);
            out.push_str(&new);
            *last = end;
        }
        end
    };
    while i < b.len() {
        let c = b[i];
        let at_start = i == 0 || !name_char(b[i - 1]);
        match c {
            b'"' => i = string_end(b, i, b'"').unwrap_or(b.len()),
            b'[' => {
                i = bracket_end(b, i);
                // the workbook of an external reference: skip its sheet and reference
                let k = name_end(b, i);
                if k > i && b.get(k) == Some(&b'!') {
                    i = token_end(b, k + 1);
                }
            }
            b'\'' => match string_end(b, i, b'\'') {
                Some(e) if b.get(e) == Some(&b'!') => {
                    let name = src[i + 1..e - 1].replace("''", "'");
                    i = if name.starts_with('[') || name.contains(':') {
                        token_end(b, e + 1) // another workbook, or a 3D reference
                    } else {
                        emit(&mut out, &mut last, Some(&name), i, e + 1)
                    };
                }
                Some(e) => i = e,
                None => i = b.len(),
            },
            b'$' if at_start => {
                let next = emit(&mut out, &mut last, None, i, i);
                i = if next > i { next } else { i + 1 };
            }
            c if name_char(c) && at_start => {
                let e = name_end(b, i);
                if b.get(e) == Some(&b'!') {
                    i = emit(&mut out, &mut last, Some(&src[i..e]), i, e + 1);
                    continue;
                }
                if b.get(e) == Some(&b':') {
                    let k = name_end(b, e + 1);
                    if k > e + 1 && b.get(k) == Some(&b'!') {
                        i = token_end(b, k + 1); // 3D reference
                        continue;
                    }
                }
                i = match scan_ref_token(b, i) {
                    Some(_) => emit(&mut out, &mut last, None, i, i),
                    None => e,
                };
            }
            _ => i += 1,
        }
    }
    if last == 0 {
        return src.to_string();
    }
    out.push_str(&src[last..]);
    out
}

/// Rewrites unqualified cell and row references of a formula (`A1`, `$B$2:C5`, `3:5`). `map`
/// receives a reference or both ends of a range and returns the replacement.
pub fn map_formula(f: &str, map: &dyn Fn(&str, Option<&str>) -> String) -> String {
    walk_refs(f, &mut |sheet, kind, a, b| (sheet.is_none() && kind != Kind::Cols).then(|| map(a, b)))
}

/// Rewrites references qualified with one of `sheets` (`Sheet1!A5`, `'My sheet'!A1:B2`,
/// `Data!$3:$4`).
fn map_qualified(f: &str, sheets: &[(&str, &RowMap)], own: Option<Own>) -> String {
    walk_refs(f, &mut |sheet, kind, a, b| {
        let name = sheet?;
        let (_, m) = sheets.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))?;
        (kind != Kind::Cols).then(|| m.ref_text(a, b, own).unwrap_or_else(|| "#REF!".into()))
    })
}

/// Shifts one reference end by (`dr`, `dc`); `$` parts stay. `None` when it leaves the sheet.
fn shift_end(r: &str, dr: i64, dc: i64) -> Option<String> {
    let b = r.as_bytes();
    let mut out = String::with_capacity(r.len() + 2);
    let mut i = 0;
    let abs_col = b.first() == Some(&b'$');
    if abs_col {
        i = 1;
    }
    let ls = i;
    while i < b.len() && b[i].is_ascii_uppercase() {
        i += 1;
    }
    if i == ls {
        i = 0; // no column: a leading `$` belongs to the row
    } else {
        let col = col_number(&r[ls..i]);
        let col = if abs_col { col } else { col + dc };
        if !(1..=16_384).contains(&col) {
            return None;
        }
        out.push_str(if abs_col { "$" } else { "" });
        out.push_str(&crate::model::xlsx::index_to_column(col as u32));
    }
    if i < b.len() {
        let abs_row = b[i] == b'$';
        let row: i64 = r[i + usize::from(abs_row)..].parse().ok()?;
        let row = if abs_row { row } else { row + dr };
        if !(1..=MAX_ROW).contains(&row) {
            return None;
        }
        out.push_str(if abs_row { "$" } else { "" });
        out.push_str(&row.to_string());
    }
    Some(out)
}

/// Shifts the relative references of a formula by (`d_row`, `d_col`), as Excel does when it
/// copies a formula (shared formulas). References to other sheets shift too; references that
/// leave the sheet become `#REF!`.
pub fn shift_formula(formula: &str, d_row: i64, d_col: i64) -> String {
    if d_row == 0 && d_col == 0 {
        return formula.to_string();
    }
    walk_refs(formula, &mut |_, _, a, b| {
        let one = shift_end(a, d_row, d_col);
        Some(match (one, b.map(|b| shift_end(b, d_row, d_col))) {
            (Some(x), None) => x,
            (Some(x), Some(Some(y))) => format!("{x}:{y}"),
            _ => "#REF!".into(),
        })
    })
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
    /// Template rows removed by empty loops, as sorted runs `(first, last)`.
    removed: Vec<(i64, i64)>,
    /// `prefix[i]`: removed rows in `removed[..i]`.
    prefix: Vec<i64>,
}

/// The row a formula lives in: its template row, its new number and which loop copy it is.
#[derive(Debug, Clone, Copy)]
struct Own {
    orig: i64,
    new: i64,
    copy: usize,
}

impl RowMap {
    fn new(map: HashMap<i64, Vec<i64>>, removed: &BTreeSet<i64>) -> RowMap {
        let mut sorted: Vec<i64> = map.keys().copied().collect();
        sorted.sort_unstable();
        let mut runs: Vec<(i64, i64)> = Vec::new();
        for &r in removed {
            match runs.last_mut() {
                Some(last) if last.1 + 1 == r => last.1 = r,
                _ => runs.push((r, r)),
            }
        }
        let mut prefix = vec![0];
        for (a, b) in &runs {
            prefix.push(prefix.last().copied().unwrap_or(0) + b - a + 1);
        }
        RowMap { map, sorted, removed: runs, prefix }
    }

    /// The run of removed rows containing `r`.
    fn removed_run(&self, r: i64) -> Option<(i64, i64)> {
        let i = self.removed.partition_point(|(a, _)| *a <= r);
        (i > 0 && self.removed[i - 1].1 >= r).then(|| self.removed[i - 1])
    }

    /// Removed rows below `x`.
    fn removed_below(&self, x: i64) -> i64 {
        let i = self.removed.partition_point(|(a, _)| *a < x);
        let mut n = self.prefix[i];
        if i > 0 && self.removed[i - 1].1 >= x {
            n -= self.removed[i - 1].1 - x + 1;
        }
        n
    }

    fn removed_between(&self, lo: i64, hi: i64) -> i64 {
        if hi <= lo + 1 {
            return 0;
        }
        self.removed_below(hi) - self.removed_below(lo + 1)
    }

    /// True when `a` and `b` are rows of the same repeated block (a loop over several rows):
    /// their copies interleave one to one.
    fn same_block(&self, a: i64, b: i64) -> bool {
        if a == b {
            return true;
        }
        let (Some(x), Some(y)) = (self.map.get(&a), self.map.get(&b)) else { return false };
        let n = x.len();
        let apart = |k: usize| x[k].max(y[k]) < x[k + 1].min(y[k + 1]);
        n > 1 && n == y.len() && apart(0) && apart(n - 2)
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
            (_, None) if self.removed_run(r).is_some() => return None,
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
        if let Some((_, last)) = self.removed_run(s) {
            s = last + 1;
        }
        if let Some((first, _)) = self.removed_run(e) {
            e = first - 1;
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
    /// cell of a repeated row covers every copy; whole columns stay as they are. `None` when
    /// nothing is left.
    fn sqref(&self, list: &str) -> Option<String> {
        let out: Vec<String> = list
            .split_whitespace()
            .filter_map(|r| {
                let (a, b) = r.split_once(':').unwrap_or((r, r));
                let t = self.ref_text(a, Some(b), None)?;
                Some(match t.split_once(':') {
                    Some((x, y)) if x == y && is_cell(x) => x.to_string(),
                    _ => t,
                })
            })
            .collect();
        (!out.is_empty()).then(|| out.join(" "))
    }

    /// Maps a formula of this sheet: unqualified references and references qualified with the
    /// sheet's own name (`own_name`).
    fn formula(&self, f: &str, own: Option<Own>, own_name: Option<&str>) -> String {
        walk_refs(f, &mut |sheet, kind, a, b| {
            if kind == Kind::Cols || sheet.is_some_and(|s| !own_name.is_some_and(|n| n.eq_ignore_ascii_case(s))) {
                return None;
            }
            Some(self.ref_text(a, b, own).unwrap_or_else(|| "#REF!".into()))
        })
    }

    /// Maps a formula of conditional formatting or data validation. Its relative references are
    /// relative to the top-left cell of the range (`old_top` → `new_top`): a relative reference to
    /// a removed row moves with that corner instead of becoming `#REF!`.
    fn range_formula(&self, f: &str, old_top: i64, new_top: i64, own_name: Option<&str>) -> String {
        let end = |r: &str, last: bool| -> Option<String> {
            let (p, row) = split_ref(r)?;
            match self.row(row, last, None) {
                Some(n) => Some(format!("{p}{n}")),
                None if !p.ends_with('$') => {
                    let n = new_top + (row - old_top);
                    (1..=MAX_ROW).contains(&n).then(|| format!("{p}{n}"))
                }
                None => None,
            }
        };
        walk_refs(f, &mut |sheet, kind, a, b| {
            if kind == Kind::Cols || sheet.is_some_and(|s| !own_name.is_some_and(|n| n.eq_ignore_ascii_case(s))) {
                return None;
            }
            let both_abs =
                split_ref(a).is_some_and(|(p, _)| p.ends_with('$')) && b.and_then(split_ref).is_some_and(|(p, _)| p.ends_with('$'));
            Some(
                match b {
                    Some(_) if both_abs => self.ref_text(a, b, None),
                    None => end(a, false),
                    Some(b) => end(a, false).zip(end(b, true)).map(|(x, y)| format!("{x}:{y}")),
                }
                .unwrap_or_else(|| "#REF!".into()),
            )
        })
    }
}

/// Top row of a `sqref` list (ranges of whole columns have none).
fn sqref_top(list: &str) -> Option<i64> {
    list.split_whitespace().filter_map(|r| split_ref(r.split(':').next()?).map(|(_, n)| n)).min()
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

/// Shared formula groups to write out as ordinary formulas: groups with a cell in a repeated row
/// (copies of a master would repeat its `si` and overlap its range) and groups whose master's
/// row was removed.
fn expanded_groups(body: &str, spans: &[RowSpan], origs: &[i64], rm: &RowMap, info: &SheetInfo) -> BTreeSet<String> {
    let mut expand = BTreeSet::new();
    if info.masters.is_empty() {
        return expand;
    }
    for (si, (_, _, row)) in &info.masters {
        if !rm.map.get(row).is_some_and(|l| l.len() == 1) {
            expand.insert(si.clone());
        }
    }
    if body.contains("t=\"shared\"") && rm.map.values().any(|l| l.len() > 1) {
        for (r, orig) in spans.iter().zip(origs) {
            let Some(inner) = r.inner else { continue };
            if !rm.map.get(orig).is_some_and(|l| l.len() > 1) {
                continue;
            }
            for (s, _, end) in cells(inner) {
                let cell = &inner[s..end];
                let Some((fs, fe, _, _)) = formula_of(cell) else { continue };
                let tag = &cell[fs..fe];
                if raw_attr(tag, "t") == Some("shared") {
                    if let Some(si) = raw_attr(tag, "si") {
                        expand.insert(si.to_string());
                    }
                }
            }
        }
    }
    expand
}

/// One row of the output: a template row (or its copy), or an empty row kept for a table.
struct OutRow {
    orig: i64,
    span: Option<usize>,
}

fn renumber_rows(xml: &str, info: &SheetInfo) -> (String, Option<RowMap>) {
    let Some(sd_start) = xml.find("<sheetData") else { return (xml.to_string(), None) };
    let Some(sd_end) = xml[sd_start..].find("</sheetData>").map(|e| e + sd_start) else { return (xml.to_string(), None) };
    let body = &xml[sd_start..sd_end];
    let spans = rows(body);
    let template_rows = &info.rows;

    // template rows of each output row
    let mut out_rows: Vec<OutRow> = Vec::with_capacity(spans.len());
    let mut prev = 0i64;
    for (k, r) in spans.iter().enumerate() {
        let o = raw_attr(r.attrs, "r").and_then(row_num).unwrap_or((prev + 1).min(MAX_ROW));
        out_rows.push(OutRow { orig: o, span: Some(k) });
        prev = o;
    }
    let present: BTreeSet<i64> = out_rows.iter().map(|r| r.orig).collect();
    let mut removed: BTreeSet<i64> = template_rows.difference(&present).copied().collect();
    // a table whose data rows were all removed keeps its first one, empty (Excel needs a data row)
    for &(d1, d2) in &info.table_data {
        if removed.range(d1..=d2).count() as i64 == d2 - d1 + 1 {
            removed.remove(&d1);
            let at = out_rows.iter().position(|r| r.orig > d1).unwrap_or(out_rows.len());
            out_rows.insert(at, OutRow { orig: d1, span: None });
        }
    }

    // pass 1: assign new row numbers. Blank rows between template rows are kept; template rows
    // that are not rendered (empty loops) leave no gap, as if deleted in Excel.
    let mut numbers: Vec<i64> = Vec::with_capacity(out_rows.len());
    let mut copy_of: Vec<usize> = Vec::with_capacity(out_rows.len());
    let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
    let (mut cur, mut prev) = (0i64, 0i64);
    for r in &out_rows {
        let o = r.orig;
        // a copy of a looped row starts right after the previous copy
        let gap = if o <= prev { 1 } else { (o - prev) - template_rows.range(prev + 1..o).count() as i64 };
        cur = (cur + gap.max(1)).min(MAX_ROW);
        let list = map.entry(o).or_default();
        copy_of.push(list.len());
        list.push(cur);
        numbers.push(cur);
        prev = o;
    }
    let changed =
        out_rows.iter().zip(&numbers).any(|(r, n)| r.orig != *n || r.span.is_none()) || !removed.is_empty() || numbers.len() != map.len();
    if !changed {
        return (xml.to_string(), None);
    }
    let rm = RowMap::new(map, &removed);
    let origs: Vec<i64> = out_rows.iter().filter(|r| r.span.is_some()).map(|r| r.orig).collect();
    let expand = expanded_groups(body, &spans, &origs, &rm, info);
    let own_name = Some(info.name.as_str()).filter(|n| !n.is_empty());

    // pass 2: rewrite rows, cells and formulas
    let mut out = String::with_capacity(xml.len() + xml.len() / 8);
    // the used-range hint is optional; drop it rather than keep a wrong one
    out.push_str(&rewrite_each(xml[..sd_start].to_string(), "<dimension", |_, _| Edit::Drop));
    let mut last = 0;
    for (k, r) in out_rows.iter().enumerate() {
        let own = Own { orig: r.orig, new: numbers[k], copy: copy_of[k] };
        let next_s = own.new.to_string();
        let Some(span) = r.span.map(|i| &spans[i]) else {
            out.push_str(&format!("<row r=\"{next_s}\"/>"));
            continue;
        };
        out.push_str(&body[last..span.start]);
        last = span.end;
        let close = if span.inner.is_none() { "/>" } else { ">" };
        out.push_str(&set_raw_attribute(&format!("<row{}{close}", span.attrs), "r", &next_s));
        let Some(inner) = span.inner else { continue };
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
                Some(f) => {
                    out.push_str(&rewrite_cell_formula(cell, f, col.map(col_number).unwrap_or(0), own, &rm, own_name, info, &expand))
                }
                None => out.push_str(cell),
            }
        }
        out.push_str(&inner[cl..]);
        out.push_str("</row>");
    }
    out.push_str(&body[last..]);
    out.push_str(&rewrite_sheet_ranges(xml[sd_end..].to_string(), &rm, own_name));
    (out, Some(rm))
}

#[allow(clippy::too_many_arguments)]
fn rewrite_cell_formula(
    cell: &str,
    (fs, fe, te, end): (usize, usize, usize, usize),
    col: i64,
    own: Own,
    rm: &RowMap,
    own_name: Option<&str>,
    info: &SheetInfo,
    expand: &BTreeSet<String>,
) -> String {
    let tag = &cell[fs..fe];
    let shared = raw_attr(tag, "t") == Some("shared");
    let master = raw_attr(tag, "si").filter(|si| shared && expand.contains(*si)).and_then(|si| info.masters.get(si));
    let (head, text) = match master {
        Some((master, mcol, mrow)) => {
            let text = if raw_attr(tag, "ref").is_some() {
                decode_xml(&cell[fe..te]).into_owned()
            } else {
                shift_formula(master, own.orig - mrow, col - mcol)
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
            let mut head = tag.to_string();
            for a in ["ref", "r1", "r2"] {
                if let Some(v) = raw_attr(&head, a) {
                    let n = rm.range_text(v, Some(own)).unwrap_or_else(|| v.to_string());
                    head = set_raw_attribute(&head, a, &n);
                }
            }
            if te == fe {
                // no text: a shared formula's dependent cell, or a data table
                return format!("{}{head}{}", &cell[..fs], &cell[end..]);
            }
            (head, decode_xml(&cell[fe..te]).into_owned())
        }
    };
    let mapped = rm.formula(&text, Some(own), own_name);
    format!("{}{head}{}</f>{}", &cell[..fs], escape_xml(&mapped), &cell[end..])
}

/// Everything after `</sheetData>`: merges, conditional formatting, data validation, hyperlinks,
/// the auto filter, page breaks, ignored errors, protected ranges, controls and x14 extensions.
fn rewrite_sheet_ranges(tail: String, rm: &RowMap, own_name: Option<&str>) -> String {
    let mut out = tail;

    // merged cells: duplicate merges inside repeated rows, shift the others, drop the ones left
    // with a single cell
    let merge_cells = tags(&out, "<mergeCells").next();
    if let Some((s, e)) = merge_cells {
        let end = element_end(&out, s, e).unwrap_or(out.len());
        let mut merges: Vec<String> = Vec::new();
        for (ms, me) in tags(&out[e..end], "<mergeCell") {
            let Some(r) = raw_attr(&out[e + ms..e + me], "ref") else { continue };
            let Some(((ca, ra), (cb, rb))) = split_range(r) else { continue };
            let one_col = ca.trim_matches('$') == cb.trim_matches('$');
            let mut push = |x: i64, y: i64| {
                if !(one_col && x == y) {
                    merges.push(format!("{ca}{x}:{cb}{y}"));
                }
            };
            if let (Some(la), Some(lb)) = (rm.map.get(&ra), rm.map.get(&rb)) {
                if la.len() > 1 && rm.same_block(ra, rb) {
                    la.iter().zip(lb).for_each(|(x, y)| push(*x, *y));
                    continue;
                }
            }
            if let Some((x, y)) = rm.span(ra, rb, None) {
                push(x, y);
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

    // ranges and formulas of conditional formatting and data validation, ranges of ignored errors
    // and protected ranges; drop the ones whose rows are all gone
    let range_formulas = |body: &str, old: &str, new: &str| {
        let (Some(old_top), Some(new_top)) = (sqref_top(old), sqref_top(new)) else { return body.to_string() };
        let mut body = body.to_string();
        for (open, close) in FORMULA_ELEMENTS {
            body = rewrite_elements(body, open, close, &|f| Some(rm.range_formula(f, old_top, new_top, own_name)));
        }
        body
    };
    for tag in ["<conditionalFormatting", "<dataValidation", "<ignoredError", "<protectedRange"] {
        out = rewrite_each(out, tag, |head, body| {
            let Some(sqref) = raw_attr(head, "sqref") else { return Edit::Keep };
            let Some(list) = rm.sqref(sqref) else { return Edit::Drop };
            Edit::Replace(format!("{}{}", set_raw_attribute(head, "sqref", &list), range_formulas(body, sqref, &list)))
        });
    }
    out = fix_wrapper(out, "<dataValidations", "<dataValidation", &["count"]);
    out = fix_wrapper(out, "<ignoredErrors", "<ignoredError", &[]);
    out = fix_wrapper(out, "<protectedRanges", "<protectedRange", &[]);

    // hyperlinks: shift, drop links on removed rows, follow links into this sheet
    out = rewrite_each(out, "<hyperlink", |head, body| {
        let Some(r) = raw_attr(head, "ref") else { return Edit::Keep };
        let Some(n) = rm.range_text(r, None) else { return Edit::Drop };
        let mut head = set_raw_attribute(head, "ref", &n);
        if let (Some(loc), Some(name)) = (raw_attr(&head, "location"), own_name) {
            let mapped = map_qualified(&decode_xml(loc), &[(name, rm)], None);
            head = set_raw_attribute(&head, "location", &escape_attr(&mapped));
        }
        Edit::Replace(format!("{head}{body}"))
    });
    out = fix_wrapper(out, "<hyperlinks", "<hyperlink", &[]);

    // the sheet's auto filter and sort state go with their rows
    out = rewrite_each(out, "<autoFilter", |head, body| {
        let Some(r) = raw_attr(head, "ref") else { return Edit::Keep };
        match rm.range_text(r, None) {
            Some(n) => Edit::Replace(format!("{}{body}", set_raw_attribute(head, "ref", &n))),
            None => Edit::Drop,
        }
    });
    out = map_sort_state(out, rm);

    // page breaks: `id` is the last row above the break (column breaks stay)
    out = rewrite_each(out, "<rowBreaks", |head, body| {
        let body = rewrite_each(body.to_string(), "<brk", |brk, rest| {
            let Some(id) = raw_attr(brk, "id").and_then(row_num) else { return Edit::Keep };
            match rm.row(id, true, None) {
                Some(n) => Edit::Replace(format!("{}{rest}", set_raw_attribute(brk, "id", &n.to_string()))),
                None => Edit::Drop,
            }
        });
        Edit::Replace(format!("{head}{body}"))
    });
    out = fix_wrapper(out, "<rowBreaks", "<brk", &["count", "manualBreakCount"]);

    // form controls and embedded objects
    out = rewrite_each(out, "<anchor", |tag, body| move_anchor(tag, body, "", rm));

    // x14 extensions: the range is the `<xm:sqref>` child
    let sqref_of = |body: &str| -> Option<(usize, usize)> {
        let s = body.find("<xm:sqref>")? + 10;
        Some((s, body[s..].find("</xm:sqref>")? + s))
    };
    for tag in ["<x14:conditionalFormatting", "<x14:dataValidation"] {
        out = rewrite_each(out, tag, |head, body| {
            let Some((s, e)) = sqref_of(body) else { return Edit::Keep };
            let old = decode_xml(&body[s..e]).into_owned();
            let Some(new) = rm.sqref(&old) else { return Edit::Drop };
            let body = format!("{}{}{}", &body[..s], escape_xml(&new), &body[e..]);
            Edit::Replace(format!("{head}{}", range_formulas(&body, &old, &new)))
        });
    }
    // sparklines are one cell each: a sparkline in a repeated row is repeated with it
    out = rewrite_each(out, "<x14:sparkline", |head, body| {
        let Some((s, e)) = sqref_of(body) else { return Edit::Keep };
        let Some((col, row)) = split_ref(body[s..e].trim()) else { return Edit::Keep };
        let copies: Vec<Own> = match rm.map.get(&row) {
            Some(list) => list.iter().enumerate().map(|(k, n)| Own { orig: row, new: *n, copy: k }).collect(),
            None => match rm.row(row, false, None) {
                Some(n) => vec![Own { orig: row, new: n, copy: 0 }],
                None => return Edit::Drop,
            },
        };
        let mut x = String::new();
        for own in copies {
            let b = format!("{}{col}{}{}", &body[..s], own.new, &body[e..]);
            let b = rewrite_elements(b, "<xm:f", "</xm:f>", &|f| Some(rm.formula(f, Some(own), own_name)));
            x.push_str(head);
            x.push_str(&b);
        }
        Edit::Replace(x)
    });
    out = fix_wrapper(out, "<x14:conditionalFormattings", "<x14:conditionalFormatting", &[]);
    out = fix_wrapper(out, "<x14:dataValidations", "<x14:dataValidation", &["count"]);
    out = fix_wrapper(out, "<x14:sparklineGroup", "<x14:sparkline", &[]);
    out = fix_wrapper(out, "<x14:sparklineGroups", "<x14:sparklineGroup", &[]);
    if out.contains("<extLst") {
        out = rewrite_each(out, "<ext", |_, body| if body.trim() == "</ext>" { Edit::Drop } else { Edit::Keep });
        out = rewrite_each(out, "<extLst", |_, body| if body.trim() == "</extLst>" { Edit::Drop } else { Edit::Keep });
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
