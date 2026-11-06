//! XLSX document model: sheets, rows, typed cells, formulas, merges.

use crate::date::DateTime;
use crate::error::{Error, Result};
use crate::formats::xlsx::{read_shared_strings, shared_strings_path, workbook_path, worksheet_parts, SharedString};
use crate::num::{js_string_to_number, js_to_string, json_number};
use crate::opc::{rel, Package};
use crate::xml::dom::{element_text, parse_fragment, parse_xml, serialize_xml, text_content, XmlElement, XmlNode};
use crate::xml::escape::escape_xml;
use serde::{Deserialize, Serialize};
use serde_json::Value as SJson;
use std::collections::{BTreeSet, HashMap};

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct XlsxDocument {
    pub sheets: Vec<XlsxSheet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct XlsxSheet {
    pub name: String,
    /// Package path of an existing sheet. Omit to create a new sheet on build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub rows: Vec<XlsxRow>,
    /// Merged ranges, e.g. `A1:C1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merges: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct XlsxRow {
    /// 1-based row number.
    pub index: u32,
    /// Custom height in points.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::num::ser_opt_f64")]
    pub height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub cells: Vec<XlsxCell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum CellType {
    /// A date-formatted number; `value` is an ISO string.
    Date,
    /// `#N/A` & co.
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct XlsxCell {
    /// A1-style reference. When omitted, the cell goes to the column after the previous one.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// string | number | boolean | null.
    #[serde(default)]
    pub value: SJson,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub cell_type: Option<CellType>,
    /// Formula without the leading `=`. `value` holds the cached result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    /// Index into the workbook cell formats (`cellXfs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<u32>,
    /// Original rich-text XML (`<r>` runs). Used when `value` still equals its plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich_text: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------------------------

pub fn column_to_index(col: &str) -> u32 {
    col.to_ascii_uppercase().bytes().fold(0u32, |n, c| n.saturating_mul(26).saturating_add((c.saturating_sub(64)) as u32))
}

pub fn index_to_column(mut n: u32) -> String {
    let mut s = Vec::new();
    while n > 0 {
        let m = (n - 1) % 26;
        s.push(b'A' + m as u8);
        n = (n - 1) / 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// `$A$1` → (col 1, row 1).
pub fn parse_ref(r: &str) -> Result<(u32, u32)> {
    let s = r.replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit()).ok_or_else(|| Error::invalid_model(format!("Invalid cell reference \"{r}\"")))?;
    let (col, row) = s.split_at(split);
    if col.is_empty()
        || !col.bytes().all(|b| b.is_ascii_alphabetic())
        || !row.bytes().all(|b| b.is_ascii_digit())
        || r.matches('$').count() > 2
    {
        return Err(Error::invalid_model(format!("Invalid cell reference \"{r}\"")));
    }
    Ok((column_to_index(col), row.parse().map_err(|_| Error::invalid_model(format!("Invalid cell reference \"{r}\"")))?))
}

// ---------------------------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------------------------

const DAY_MS: f64 = 86_400_000.0;
const EPOCH_1900: i64 = -2_209_161_600_000; // 1899-12-30
const EPOCH_1904: i64 = -2_082_844_800_000; // 1904-01-01

fn epoch(date1904: bool) -> i64 {
    if date1904 {
        EPOCH_1904
    } else {
        EPOCH_1900
    }
}

pub fn serial_to_date(serial: f64, date1904: bool) -> DateTime {
    DateTime::from_ms(epoch(date1904) + (serial * DAY_MS).round() as i64, false)
}

pub fn date_to_serial(d: &DateTime, date1904: bool) -> f64 {
    (d.ms - epoch(date1904)) as f64 / DAY_MS
}

fn to_iso_date(d: &DateTime) -> String {
    let iso = d.to_iso();
    match iso.strip_suffix("T00:00:00.000Z") {
        Some(day) => day.to_string(),
        None => iso.replace(".000Z", "Z"),
    }
}

const BUILTIN_DATE_FORMATS: [u32; 31] =
    [14, 15, 16, 17, 18, 19, 20, 21, 22, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 45, 46, 47, 50, 51, 52, 53, 54, 55, 56, 57, 58];

fn is_date_format(code: &str) -> bool {
    let cleaned = crate::re!(r#""[^"]*"|\\.|\[[^\]]*\]|_.|\*."#).replace_all(code, "");
    cleaned.chars().any(|c| matches!(c.to_ascii_lowercase(), 'd' | 'm' | 'y' | 'h' | 's'))
        && !cleaned.trim().eq_ignore_ascii_case("general")
}

struct StyleInfo {
    date_styles: BTreeSet<u32>,
    date1904: bool,
}

fn read_styles(pkg: &Package, wb: &str) -> StyleInfo {
    let date1904 = crate::re!(r#"<workbookPr\b[^>]*\sdate1904="(1|true)""#).is_match(&pkg.get_text(wb).unwrap_or_default());
    let mut date_styles = BTreeSet::new();
    let path = pkg.related_parts(wb, rel::STYLES).into_iter().next().map(|(_, p)| p);
    let Some(xml) = path.and_then(|p| pkg.get_text(&p)) else { return StyleInfo { date_styles, date1904 } };
    let Ok(doc) = parse_xml(&xml, pkg.max_xml_depth) else { return StyleInfo { date_styles, date1904 } };
    let mut custom: HashMap<u32, String> = HashMap::new();
    if let Some(f) = doc.root.first_child("numFmts") {
        for n in f.children_named("numFmt") {
            if let Some(id) = n.attr("numFmtId").and_then(|v| v.parse().ok()) {
                custom.insert(id, n.attr("formatCode").unwrap_or("").to_string());
            }
        }
    }
    if let Some(xfs) = doc.root.first_child("cellXfs") {
        for (i, xf) in xfs.children_named("xf").enumerate() {
            let id: u32 = xf.attr("numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0);
            if BUILTIN_DATE_FORMATS.contains(&id) || custom.get(&id).is_some_and(|c| is_date_format(c)) {
                date_styles.insert(i as u32);
            }
        }
    }
    StyleInfo { date_styles, date1904 }
}

// ---------------------------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------------------------

pub fn parse_xlsx(pkg: &Package) -> Result<XlsxDocument> {
    let wb = workbook_path(pkg);
    let shared = read_shared_strings(pkg, &wb);
    let styles = read_styles(pkg, &wb);
    let mut sheets = Vec::new();
    for s in worksheet_parts(pkg) {
        sheets.push(parse_sheet(pkg, &s.name, &s.path, &shared, &styles)?);
    }
    Ok(XlsxDocument { sheets })
}

struct SharedFormula {
    formula: String,
    col: u32,
    row: u32,
}

fn parse_sheet(pkg: &Package, name: &str, path: &str, shared: &[SharedString], styles: &StyleInfo) -> Result<XlsxSheet> {
    let xml = pkg.get_text(path).unwrap_or_default();
    let root = parse_xml(&xml, pkg.max_xml_depth).map_err(|e| e.with_part(path))?.root;
    let mut sheet = XlsxSheet { name: name.to_string(), path: Some(path.to_string()), rows: Vec::new(), merges: None };
    let mut shared_formulas: HashMap<String, SharedFormula> = HashMap::new();
    let mut prev_row = 0u32;
    if let Some(data) = root.first_child("sheetData") {
        for r in data.children_named("row") {
            let index = r.attr("r").and_then(|v| v.parse().ok()).unwrap_or(prev_row + 1);
            prev_row = index;
            let mut row = XlsxRow { index, height: None, hidden: None, cells: Vec::new() };
            if r.attr("customHeight") == Some("1") {
                if let Some(ht) = r.attr("ht").filter(|h| !h.is_empty()) {
                    row.height = Some(js_string_to_number(ht));
                }
            }
            if matches!(r.attr("hidden"), Some("1" | "true")) {
                row.hidden = Some(true);
            }
            let mut prev_col = 0u32;
            for c in r.children_named("c") {
                let reference = c.attr("r").map(str::to_string).unwrap_or_else(|| format!("{}{}", index_to_column(prev_col + 1), index));
                prev_col = parse_ref(&reference).map(|(col, _)| col).unwrap_or(prev_col + 1);
                if let Some(cell) = parse_cell(c, &reference, shared, styles, &mut shared_formulas) {
                    row.cells.push(cell);
                }
            }
            if !row.cells.is_empty() || row.height.is_some() || row.hidden.is_some() {
                sheet.rows.push(row);
            }
        }
    }
    if let Some(m) = root.first_child("mergeCells") {
        sheet.merges = Some(m.children_named("mergeCell").map(|x| x.attr("ref").unwrap_or("").to_string()).collect());
    }
    Ok(sheet)
}

fn parse_cell(
    c: &XmlElement,
    reference: &str,
    shared: &[SharedString],
    styles: &StyleInfo,
    shared_formulas: &mut HashMap<String, SharedFormula>,
) -> Option<XlsxCell> {
    let t = c.attr("t").unwrap_or("n");
    let v_text = c.first_child("v").map(element_text);
    let style: Option<u32> = c.attr("s").and_then(|s| s.parse().ok());
    let mut cell = XlsxCell {
        reference: Some(reference.to_string()),
        value: SJson::Null,
        cell_type: None,
        formula: None,
        style: style.filter(|s| *s != 0),
        rich_text: None,
    };

    if let Some(f) = c.first_child("f") {
        let text = element_text(f);
        match (f.attr("t"), f.attr("si")) {
            (Some("shared"), Some(si)) => {
                let (col, row) = parse_ref(reference).unwrap_or((0, 0));
                if !text.is_empty() {
                    shared_formulas.insert(si.to_string(), SharedFormula { formula: text.clone(), col, row });
                }
                if let Some(master) = shared_formulas.get(si) {
                    cell.formula = Some(if text.is_empty() {
                        shift_formula(&master.formula, row as i64 - master.row as i64, col as i64 - master.col as i64)
                    } else {
                        text
                    });
                }
            }
            _ if !text.is_empty() => cell.formula = Some(text),
            _ => {}
        }
    }

    match t {
        "s" => {
            let si = v_text.as_deref().and_then(|v| v.trim().parse::<usize>().ok()).and_then(|i| shared.get(i));
            cell.value = SJson::String(si.map(|s| s.text.clone()).unwrap_or_default());
            if let Some(si) = si {
                if crate::re!(r"<r[\s>]").is_match(&si.xml) {
                    cell.rich_text = Some(si.xml.clone());
                }
            }
        }
        "inlineStr" => {
            let is = c.first_child("is");
            cell.value = SJson::String(is.map(crate::formats::xlsx::visible_text).unwrap_or_default());
            if let Some(is) = is.filter(|is| is.first_child("r").is_some()) {
                cell.rich_text = Some(is.serialize_children());
            }
        }
        "str" => cell.value = SJson::String(v_text.unwrap_or_default()),
        "b" => cell.value = SJson::Bool(matches!(v_text.as_deref(), Some("1" | "true"))),
        "e" => {
            cell.value = SJson::String(v_text.unwrap_or_else(|| "#N/A".into()));
            cell.cell_type = Some(CellType::Error);
        }
        "d" => {
            cell.value = v_text.map(SJson::String).unwrap_or(SJson::Null);
            cell.cell_type = Some(CellType::Date);
        }
        _ => match v_text.as_deref() {
            None | Some("") => cell.value = SJson::Null,
            Some(v) => {
                let n = js_string_to_number(v);
                if style.is_some_and(|s| styles.date_styles.contains(&s)) && n.is_finite() {
                    cell.value = SJson::String(to_iso_date(&serial_to_date(n, styles.date1904)));
                    cell.cell_type = Some(CellType::Date);
                } else {
                    cell.value = json_number(n);
                }
            }
        },
    }
    if cell.value.is_null() && cell.formula.is_none() && cell.style.is_none() {
        return None;
    }
    Some(cell)
}

/// Shifts relative references of a formula (used to expand shared formulas).
pub fn shift_formula(formula: &str, d_row: i64, d_col: i64) -> String {
    let mut out = String::new();
    let mut in_str = false;
    let mut seg = String::new();
    let flush = |seg: &mut String, out: &mut String| {
        out.push_str(&shift_refs(seg, d_row, d_col));
        seg.clear();
    };
    for c in formula.chars() {
        if c == '"' {
            if in_str {
                out.push('"');
                in_str = false;
                continue;
            }
            flush(&mut seg, &mut out);
            in_str = true;
            out.push('"');
            continue;
        }
        if in_str {
            out.push(c);
        } else {
            seg.push(c);
        }
    }
    flush(&mut seg, &mut out);
    out
}

fn shift_refs(s: &str, d_row: i64, d_col: i64) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        let pre_ok = i == 0 || !matches!(b[i - 1], c if c.is_ascii_alphanumeric() || c == b'_' || c == b'.');
        if pre_ok {
            let mut k = i;
            let dc = b.get(k) == Some(&b'$');
            if dc {
                k += 1;
            }
            let ls = k;
            while k < b.len() && b[k].is_ascii_uppercase() {
                k += 1;
            }
            let le = k;
            let dr = b.get(k) == Some(&b'$');
            if dr {
                k += 1;
            }
            let ds = k;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            let ok = (1..=3).contains(&(le - ls))
                && k > ds
                && !matches!(b.get(k), Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b'(');
            if ok {
                out.push_str(&s[last..i]);
                let col = &s[ls..le];
                let row = &s[ds..k];
                let new_col = if dc { col.to_string() } else { index_to_column((column_to_index(col) as i64 + d_col).max(1) as u32) };
                let new_row = if dr { row.to_string() } else { (row.parse::<i64>().unwrap_or(0) + d_row).to_string() };
                out.push_str(&format!("{}{}{}{}", if dc { "$" } else { "" }, new_col, if dr { "$" } else { "" }, new_row));
                i = k;
                last = k;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

// ---------------------------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------------------------

const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const CT_SHEET: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
const CT_SST: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml";
const MERGE_BEFORE: &[&str] = &[
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "legacyDrawing",
    "legacyDrawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

#[derive(Default)]
struct StringTable {
    items: Vec<String>,
    index: HashMap<String, usize>,
}

impl StringTable {
    fn add(&mut self, xml: String) -> usize {
        if let Some(i) = self.index.get(&xml) {
            return *i;
        }
        let i = self.items.len();
        self.index.insert(xml.clone(), i);
        self.items.push(xml);
        i
    }
}

pub fn build_xlsx(model: &XlsxDocument, pkg: &mut Package) -> Result<()> {
    let depth = pkg.max_xml_depth;
    let wb = workbook_path(pkg);
    let wb_xml = pkg.get_text(&wb).ok_or_else(|| Error::invalid_model("The base workbook has no workbook part"))?;
    let mut wb_doc = parse_xml(&wb_xml, depth)?;
    let styles = read_styles(pkg, &wb);
    let mut sst = StringTable::default();
    let existing = worksheet_parts(pkg);
    let keep: BTreeSet<String> = model.sheets.iter().filter_map(|s| s.path.clone()).collect();
    let mut date_style: Option<u32> = None;

    // remove sheets that are not in the model
    for s in &existing {
        if keep.contains(&s.path) {
            continue;
        }
        pkg.remove(&s.path);
        pkg.remove_override(&s.path);
        let rid = s.r_id.clone();
        pkg.remove_relationships(&wb, |r| r.id == rid);
    }

    let old_sheets: Vec<XmlElement> =
        wb_doc.root.first_child("sheets").map(|s| s.children_named("sheet").cloned().collect()).unwrap_or_default();
    let mut max_id = old_sheets.iter().filter_map(|s| s.attr("sheetId").and_then(|v| v.parse::<u32>().ok())).max().unwrap_or(0);
    let mut new_sheets: Vec<XmlNode> = Vec::new();

    for sheet in &model.sheets {
        let found = sheet.path.as_ref().and_then(|p| existing.iter().find(|s| &s.path == p));
        let (path, r_id) = match found {
            Some(f) => (f.path.clone(), f.r_id.clone()),
            None => {
                let mut n = 1;
                while pkg.has(&format!("xl/worksheets/sheet{n}.xml")) {
                    n += 1;
                }
                let path = format!("xl/worksheets/sheet{n}.xml");
                pkg.set_text(
                    &path,
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheetData/></worksheet>",
                );
                pkg.ensure_override(&path, CT_SHEET);
                let target = path.strip_prefix("xl/").unwrap_or(&path).to_string();
                let rid = pkg.add_relationship(&wb, rel::WORKSHEET, &target, false);
                (path, rid)
            }
        };
        let mut sheet_el = old_sheets.iter().find(|s| s.attr("r:id") == Some(r_id.as_str())).cloned().unwrap_or_else(|| {
            max_id += 1;
            XmlElement::with_attrs("sheet", &[("name", &sheet.name), ("sheetId", &max_id.to_string()), ("r:id", &r_id)])
        });
        sheet_el.set_attr("name", &sheet.name);
        new_sheets.push(XmlNode::Element(sheet_el));
        write_sheet(pkg, &path, sheet, &mut sst, &styles, &mut date_style, &wb)?;
    }
    match wb_doc.root.first_child_mut("sheets") {
        Some(s) => s.children = new_sheets,
        None => {
            let mut s = XmlElement::new("sheets");
            s.children = new_sheets;
            wb_doc.root.children.push(XmlNode::Element(s));
        }
    }
    if wb_doc.root.attr("xmlns:r").is_none() {
        wb_doc.root.set_attr("xmlns:r", R_NS);
    }
    pkg.set_text(&wb, &serialize_xml(&wb_doc));

    // shared strings
    let sst_path = match shared_strings_path(pkg, &wb) {
        Some(p) => p,
        None => {
            let p = "xl/sharedStrings.xml".to_string();
            pkg.ensure_override(&p, CT_SST);
            pkg.add_relationship(&wb, rel::SHARED_STRINGS, "sharedStrings.xml", false);
            p
        }
    };
    let n = sst.items.len();
    let items: String = sst.items.iter().map(|x| format!("<si>{x}</si>")).collect();
    pkg.set_text(
        &sst_path,
        &format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{n}\" uniqueCount=\"{n}\">{items}</sst>"),
    );
    Ok(())
}

fn write_sheet(
    pkg: &mut Package,
    path: &str,
    sheet: &XlsxSheet,
    sst: &mut StringTable,
    styles: &StyleInfo,
    date_style: &mut Option<u32>,
    wb: &str,
) -> Result<()> {
    let depth = pkg.max_xml_depth;
    let mut doc = parse_xml(&pkg.get_text(path).unwrap_or_default(), depth).map_err(|e| e.with_part(path))?;
    let mut rows: Vec<&XlsxRow> = sheet.rows.iter().collect();
    rows.sort_by_key(|r| r.index);
    let mut rows_xml = String::new();
    for row in rows {
        let mut attrs = format!(" r=\"{}\"", row.index);
        if let Some(h) = row.height {
            attrs.push_str(&format!(" ht=\"{}\" customHeight=\"1\"", js_to_string(h)));
        }
        if row.hidden == Some(true) {
            attrs.push_str(" hidden=\"1\"");
        }
        let mut cells = String::new();
        let mut col = 0u32;
        for cell in &row.cells {
            let reference = cell.reference.clone().unwrap_or_else(|| format!("{}{}", index_to_column(col + 1), row.index));
            col = parse_ref(&reference)?.0;
            let fixed = format!("{}{}", reference.trim_end_matches(|c: char| c.is_ascii_digit()), row.index);
            cells.push_str(&write_cell(cell, &fixed, sst, styles, &mut || *date_style.get_or_insert_with(|| ensure_date_style(pkg, wb)))?);
        }
        rows_xml.push_str(&format!("<row{attrs}>{cells}</row>"));
    }

    let data_nodes = parse_fragment(&format!("<sheetData>{rows_xml}</sheetData>"), depth)
        .map_err(|e| Error::invalid_model(format!("Invalid cell XML in {path}: {}", e.message)))?;
    let data_children = match data_nodes.into_iter().next() {
        Some(XmlNode::Element(e)) => e.children,
        _ => Vec::new(),
    };
    let root = &mut doc.root;
    match root.first_child_mut("sheetData") {
        Some(sd) => sd.children = data_children,
        None => {
            let mut sd = XmlElement::new("sheetData");
            sd.children = data_children;
            root.children.push(XmlNode::Element(sd));
        }
    }
    // merged cells
    root.children.retain(|c| !matches!(c, XmlNode::Element(e) if e.name == "mergeCells"));
    if let Some(merges) = sheet.merges.as_ref().filter(|m| !m.is_empty()) {
        let mut m = XmlElement::with_attrs("mergeCells", &[("count", &merges.len().to_string())]);
        for r in merges {
            m = m.child(XmlElement::with_attrs("mergeCell", &[("ref", r)]));
        }
        let at = root.children.iter().position(|c| matches!(c, XmlNode::Element(e) if MERGE_BEFORE.contains(&e.name.as_str())));
        match at {
            Some(i) => root.children.insert(i, XmlNode::Element(m)),
            None => root.children.push(XmlNode::Element(m)),
        }
    }
    root.children.retain(|c| !matches!(c, XmlNode::Element(e) if e.name == "dimension"));
    pkg.set_text(path, &serialize_xml(&doc));
    Ok(())
}

fn write_cell(
    cell: &XlsxCell,
    reference: &str,
    sst: &mut StringTable,
    styles: &StyleInfo,
    date_style: &mut dyn FnMut() -> u32,
) -> Result<String> {
    let mut style = cell.style;
    let mut value = cell.value.clone();
    let mut t = "";
    let v: Option<String>;

    if cell.cell_type == Some(CellType::Date) {
        if let SJson::String(s) = &value {
            if let Some(d) = DateTime::parse(s) {
                value = json_number(date_to_serial(&d, styles.date1904));
                if !style.is_some_and(|s| styles.date_styles.contains(&s)) {
                    style = Some(date_style());
                }
            }
        }
    }

    match &value {
        SJson::Null => v = None,
        _ if cell.cell_type == Some(CellType::Error) => {
            t = "e";
            v = Some(escape_xml(&super::props::json_text(&value)));
        }
        SJson::Number(n) => v = n.as_f64().filter(|x| x.is_finite()).map(js_to_string),
        SJson::Bool(b) => {
            t = "b";
            v = Some(if *b { "1".into() } else { "0".into() });
        }
        _ if cell.formula.is_some() => {
            t = "str";
            v = Some(escape_xml(&super::props::json_text(&value)));
        }
        SJson::String(text) => {
            let rich = cell.rich_text.as_ref().filter(|r| rich_plain_text(r) == *text);
            let xml = rich.cloned().unwrap_or_else(|| format!("<t xml:space=\"preserve\">{}</t>", escape_xml(text)));
            t = "s";
            v = Some(sst.add(xml).to_string());
        }
        other => return Err(Error::invalid_model(format!("Cell {reference}: unsupported value {other}"))),
    }

    let s = style.filter(|s| *s != 0).map(|s| format!(" s=\"{s}\"")).unwrap_or_default();
    let t_attr = if t.is_empty() { String::new() } else { format!(" t=\"{t}\"") };
    let f = cell.formula.as_ref().map(|f| format!("<f>{}</f>", escape_xml(f.strip_prefix('=').unwrap_or(f)))).unwrap_or_default();
    if f.is_empty() && v.is_none() {
        return Ok(if s.is_empty() { String::new() } else { format!("<c r=\"{reference}\"{s}/>") });
    }
    let v = v.map(|v| format!("<v>{v}</v>")).unwrap_or_default();
    Ok(format!("<c r=\"{reference}\"{s}{t_attr}>{f}{v}</c>"))
}

fn rich_plain_text(xml: &str) -> String {
    parse_fragment(xml, 256)
        .map(|nodes| nodes.iter().filter(|n| !matches!(n, XmlNode::Element(e) if e.name == "rPh")).map(text_content).collect())
        .unwrap_or_default()
}

/// Finds or adds a cell format with the built-in date format (numFmtId 14).
fn ensure_date_style(pkg: &mut Package, wb: &str) -> u32 {
    let Some(path) = pkg.related_parts(wb, rel::STYLES).into_iter().next().map(|(_, p)| p) else { return 0 };
    let Some(xml) = pkg.get_text(&path) else { return 0 };
    let Ok(mut doc) = parse_xml(&xml, pkg.max_xml_depth) else { return 0 };
    if doc.root.first_child("cellXfs").is_none() {
        let xfs = XmlElement::with_attrs("cellXfs", &[("count", "1")])
            .child(XmlElement::with_attrs("xf", &[("numFmtId", "0"), ("fontId", "0"), ("fillId", "0"), ("borderId", "0"), ("xfId", "0")]));
        doc.root.children.push(XmlNode::Element(xfs));
    }
    let Some(xfs) = doc.root.first_child_mut("cellXfs") else { return 0 };
    let list: Vec<&XmlElement> = xfs.children_named("xf").collect();
    let nonzero = |v: Option<&str>| v.is_some_and(|s| s.bytes().any(|b| (b'1'..=b'9').contains(&b)));
    if let Some(found) =
        list.iter().position(|x| x.attr("numFmtId") == Some("14") && !nonzero(x.attr("fontId")) && !nonzero(x.attr("fillId")))
    {
        return found as u32;
    }
    let n = list.len();
    xfs.children.push(XmlNode::Element(XmlElement::with_attrs(
        "xf",
        &[("numFmtId", "14"), ("fontId", "0"), ("fillId", "0"), ("borderId", "0"), ("xfId", "0"), ("applyNumberFormat", "1")],
    )));
    xfs.set_attr("count", &(n + 1).to_string());
    pkg.set_text(&path, &serialize_xml(&doc));
    n as u32
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// Sheet → 2D array of values (`null` for empty cells).
pub fn sheet_to_arrays(sheet: &XlsxSheet) -> Vec<Vec<SJson>> {
    let max_row = sheet.rows.iter().map(|r| r.index).max().unwrap_or(0) as usize;
    let mut max_col = 0usize;
    for r in &sheet.rows {
        let mut col = 0usize;
        for c in &r.cells {
            col = c.reference.as_deref().and_then(|x| parse_ref(x).ok()).map(|(c, _)| c as usize).unwrap_or(col + 1);
            max_col = max_col.max(col);
        }
    }
    let max_col = max_col.min(16_384);
    let mut out = vec![vec![SJson::Null; max_col]; max_row.min(1_048_576)];
    for r in &sheet.rows {
        let mut col = 0usize;
        for c in &r.cells {
            col = c.reference.as_deref().and_then(|x| parse_ref(x).ok()).map(|(c, _)| c as usize).unwrap_or(col + 1);
            if let Some(row) = out.get_mut(r.index as usize - 1) {
                if col >= 1 && col <= row.len() {
                    row[col - 1] = c.value.clone();
                }
            }
        }
    }
    out
}

pub fn xlsx_text(model: &XlsxDocument) -> String {
    model
        .sheets
        .iter()
        .map(|s| {
            let rows: Vec<String> = sheet_to_arrays(s)
                .iter()
                .map(|r| {
                    r.iter().map(|v| if v.is_null() { String::new() } else { super::props::json_text(v) }).collect::<Vec<_>>().join("\t")
                })
                .collect();
            format!("{}\n{}", s.name, rows.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
