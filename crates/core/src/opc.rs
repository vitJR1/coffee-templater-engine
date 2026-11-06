//! Open Packaging Convention container (the ZIP inside every .docx/.xlsx/.pptx).

use crate::error::{Error, Result};
use crate::limits::{exceeds, Limits};
use crate::xml::dom::parse_xml;
use crate::xml::escape::escape_attr;
use crate::zip;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::rc::Rc;

pub mod rel {
    pub const OFFICE_DOCUMENT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    pub const IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    pub const HEADER: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    pub const FOOTER: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    pub const FOOTNOTES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
    pub const ENDNOTES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes";
    pub const WORKSHEET: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
    pub const SHARED_STRINGS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
    pub const CALC_CHAIN: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain";
    pub const SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
    pub const NOTES_SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
    pub const STYLES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
    pub const HYPERLINK: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
    pub const SLIDE_LAYOUT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
}

pub const CONTENT_TYPES: &str = "[Content_Types].xml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    pub rel_type: String,
    pub target: String,
    pub target_mode: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Package {
    files: IndexMap<String, Rc<[u8]>>,
    pub max_xml_depth: u64,
    /// (crc32, len, media dir, extension) → media path of images added by `add_image`.
    media_index: HashMap<(u32, usize, String, String), String>,
    /// Media added from a given buffer (address of the `Rc` data); checked with `Rc::ptr_eq`.
    media_by_ptr: HashMap<(usize, String, String), String>,
}

fn image_mime(ext: &str) -> String {
    match ext {
        "png" => "image/png".into(),
        "jpeg" | "jpg" => "image/jpeg".into(),
        "gif" => "image/gif".into(),
        "bmp" => "image/bmp".into(),
        "tiff" | "tif" => "image/tiff".into(),
        "ico" => "image/x-icon".into(),
        "svg" => "image/svg+xml".into(),
        "webp" => "image/webp".into(),
        "emf" => "image/x-emf".into(),
        "wmf" => "image/x-wmf".into(),
        other => format!("image/{other}"),
    }
}

use crate::xml::tokenizer::find_tags;

/// Value of `name="..."` inside a raw tag, where `name` is preceded by a non-word character.
fn attr_in(raw: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let mut from = 0;
    while let Some(p) = raw[from..].find(&pat).map(|p| p + from) {
        from = p + 1;
        let prev = if p == 0 { None } else { raw.as_bytes().get(p - 1).copied() };
        if matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            continue;
        }
        let vs = p + pat.len();
        let ve = raw[vs..].find('"')? + vs;
        return Some(raw[vs..ve].to_string());
    }
    None
}

impl Package {
    pub fn new(files: IndexMap<String, Rc<[u8]>>) -> Self {
        Package { files, max_xml_depth: Limits::default().max_xml_depth, media_index: HashMap::new(), media_by_ptr: HashMap::new() }
    }

    pub fn load(input: &[u8], limits: &Limits) -> Result<Package> {
        if exceeds(input.len() as u64, limits.max_input_bytes) {
            return Err(Error::limit("maxInputBytes", limits.max_input_bytes));
        }
        let entries = zip::read(input, limits)?;
        let mut files = IndexMap::with_capacity(entries.len());
        for (name, data) in entries {
            files.insert(name, Rc::from(data));
        }
        if !files.contains_key(CONTENT_TYPES) {
            return Err(Error::invalid_document("Input is not an OOXML package: [Content_Types].xml is missing"));
        }
        Ok(Package { files, max_xml_depth: limits.max_xml_depth, media_index: HashMap::new(), media_by_ptr: HashMap::new() })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn has(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|b| &b[..])
    }

    pub fn get_text(&self, path: &str) -> Option<String> {
        self.files.get(path).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn set_text(&mut self, path: &str, value: &str) {
        self.set_binary(path, value.as_bytes().to_vec());
    }

    pub fn set_binary(&mut self, path: &str, data: Vec<u8>) {
        let data: Rc<[u8]> = Rc::from(data);
        if let Some(slot) = self.files.get_mut(path) {
            *slot = data;
        } else {
            self.files.insert(path.to_string(), data);
        }
    }

    pub fn remove(&mut self, path: &str) {
        self.files.shift_remove(path);
    }

    /// Serializes the package. Entries keep their input order; new entries follow in creation order.
    pub fn to_bytes(&self, limits: &Limits) -> Result<Vec<u8>> {
        let total: u64 = self.files.values().map(|b| b.len() as u64).sum();
        if exceeds(total, limits.max_output_bytes) {
            return Err(Error::limit("maxOutputBytes", limits.max_output_bytes));
        }
        zip::write(self.files.iter().map(|(k, v)| (k.as_str(), &v[..])))
    }

    // ---------- relationships ----------

    pub fn rels_path_for(part: &str) -> String {
        match part.rfind('/') {
            Some(i) => format!("{}_rels/{}.rels", &part[..=i], &part[i + 1..]),
            None => format!("_rels/{part}.rels"),
        }
    }

    /// Relationships of a part (use "" for package-level rels).
    pub fn relationships(&self, part: &str) -> Vec<Relationship> {
        let path = if part.is_empty() { "_rels/.rels".to_string() } else { Package::rels_path_for(part) };
        let Some(xml) = self.get_text(&path) else { return Vec::new() };
        let Ok(doc) = parse_xml(&xml, self.max_xml_depth) else { return Vec::new() };
        doc.root
            .children_named("Relationship")
            .map(|r| Relationship {
                id: r.attr("Id").unwrap_or_default().to_string(),
                rel_type: r.attr("Type").unwrap_or_default().to_string(),
                target: r.attr("Target").unwrap_or_default().to_string(),
                target_mode: r.attr("TargetMode").map(str::to_string),
            })
            .collect()
    }

    /// Resolves a relationship target to an absolute part path inside the package.
    pub fn resolve_target(part: &str, target: &str) -> String {
        if let Some(abs) = target.strip_prefix('/') {
            return abs.to_string();
        }
        let mut base: Vec<&str> = match part.rfind('/') {
            Some(i) => part[..i].split('/').collect(),
            None => Vec::new(),
        };
        for seg in target.split('/') {
            match seg {
                ".." => {
                    base.pop();
                }
                "." | "" => {}
                s => base.push(s),
            }
        }
        base.join("/")
    }

    /// (relationship, absolute target path) pairs of the given type, internal targets only.
    pub fn related_parts(&self, part: &str, rel_type: &str) -> Vec<(Relationship, String)> {
        self.relationships(part)
            .into_iter()
            .filter(|r| r.rel_type == rel_type && r.target_mode.as_deref() != Some("External"))
            .map(|r| {
                let p = Package::resolve_target(part, &r.target);
                (r, p)
            })
            .collect()
    }

    pub fn main_document_path(&self) -> Option<String> {
        self.related_parts("", rel::OFFICE_DOCUMENT).into_iter().next().map(|(_, p)| p)
    }

    pub fn add_relationship(&mut self, part: &str, rel_type: &str, target: &str, external: bool) -> String {
        let rels_path = Package::rels_path_for(part);
        let mut xml = self.get_text(&rels_path).unwrap_or_else(|| {
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string()
        });
        let mut used = std::collections::HashSet::new();
        let mut from = 0;
        while let Some(p) = xml[from..].find("Id=\"").map(|p| p + from) {
            from = p + 4;
            let prev = if p == 0 { None } else { xml.as_bytes().get(p - 1).copied() };
            if matches!(prev, Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
                continue;
            }
            if let Some(end) = xml[from..].find('"') {
                if end > 0 {
                    used.insert(xml[from..from + end].to_string());
                }
            }
        }
        let mut n = used.len() + 1;
        while used.contains(&format!("rId{n}")) {
            n += 1;
        }
        let id = format!("rId{n}");
        let mode = if external { " TargetMode=\"External\"" } else { "" };
        let rel = format!("<Relationship Id=\"{id}\" Type=\"{}\" Target=\"{}\"{mode}/>", escape_attr(rel_type), escape_attr(target));
        if let Some(p) = xml.find("</Relationships>") {
            xml.insert_str(p, &rel);
        } else if let Some((s, e)) = {
            let found = find_tags(&xml, "<Relationships").next();
            found
        } {
            if let Some(head) = xml[s..e].strip_suffix("/>") {
                let open = format!("{}>", head.trim_end());
                xml = format!("{}{open}{rel}</Relationships>{}", &xml[..s], &xml[e..]);
            }
        }
        self.set_text(&rels_path, &xml);
        id
    }

    /// Removes relationships matching `pred` (`<Relationship .../>` elements). Returns the removed ones.
    pub fn remove_relationships(&mut self, part: &str, pred: impl Fn(&Relationship) -> bool) -> Vec<Relationship> {
        let rels_path = Package::rels_path_for(part);
        let Some(xml) = self.get_text(&rels_path) else { return Vec::new() };
        let mut removed = Vec::new();
        let mut out = String::with_capacity(xml.len());
        let mut last = 0;
        for (s, e) in find_tags(&xml, "<Relationship").collect::<Vec<_>>() {
            let raw = &xml[s..e];
            if !raw.ends_with("/>") {
                continue;
            }
            let r = Relationship {
                id: attr_in(raw, "Id").unwrap_or_default(),
                rel_type: attr_in(raw, "Type").unwrap_or_default(),
                target: attr_in(raw, "Target").unwrap_or_default(),
                target_mode: None,
            };
            if pred(&r) {
                out.push_str(&xml[last..s]);
                last = e;
                removed.push(r);
            }
        }
        out.push_str(&xml[last..]);
        self.set_text(&rels_path, &out);
        removed
    }

    // ---------- content types ----------

    pub fn ensure_default_content_type(&mut self, extension: &str, content_type: &str) {
        let Some(xml) = self.get_text(CONTENT_TYPES) else { return };
        let ext = extension.to_lowercase();
        let needle = format!("extension=\"{ext}\"");
        if find_tags(&xml, "<Default").any(|(s, e)| xml[s..e].to_lowercase().contains(&needle)) {
            return;
        }
        let add = format!("<Default Extension=\"{}\" ContentType=\"{}\"/></Types>", escape_attr(&ext), escape_attr(content_type));
        self.set_text(CONTENT_TYPES, &xml.replacen("</Types>", &add, 1));
    }

    pub fn ensure_override(&mut self, part: &str, content_type: &str) {
        let Some(xml) = self.get_text(CONTENT_TYPES) else { return };
        if xml.contains(&format!("PartName=\"/{part}\"")) {
            return;
        }
        let add = format!("<Override PartName=\"/{}\" ContentType=\"{}\"/></Types>", escape_attr(part), escape_attr(content_type));
        self.set_text(CONTENT_TYPES, &xml.replacen("</Types>", &add, 1));
    }

    pub fn remove_override(&mut self, part: &str) {
        let Some(xml) = self.get_text(CONTENT_TYPES) else { return };
        let needle = format!("PartName=\"/{part}\"");
        let found = find_tags(&xml, "<Override").find(|&(s, e)| xml[s..e].contains(&needle) && xml[s..e].ends_with("/>"));
        if let Some((s, e)) = found {
            let next = format!("{}{}", &xml[..s], &xml[e..]);
            self.set_text(CONTENT_TYPES, &next);
        }
    }

    /// Stores an image in `<media_dir>/` and links it from `part`. Returns the relationship id.
    ///
    /// Identical images (same bytes and extension) are stored once, and a part that already links
    /// to the stored file reuses its relationship.
    pub fn add_image(&mut self, part: &str, media_dir: &str, data: Rc<[u8]>, extension: &str) -> Result<String> {
        let lower = extension.to_lowercase();
        let ext = if lower == "jpg" { "jpeg".to_string() } else { lower };
        if !crate::image::is_safe_extension(&ext) {
            return Err(Error::new(crate::ErrorCode::InvalidImage, format!("Invalid image extension {ext:?}")));
        }
        // the same value used again (an image in a loop): no hashing, no byte comparison
        let same = self.media_by_ptr.get(&(Rc::as_ptr(&data) as *const u8 as usize, media_dir.to_string(), ext.clone()));
        let existing = same.filter(|m| self.files.get(*m).is_some_and(|b| Rc::ptr_eq(b, &data))).cloned();
        let key = (crc32fast::hash(&data), data.len(), media_dir.to_string(), ext.clone());
        let existing =
            existing.or_else(|| self.media_index.get(&key).filter(|m| self.files.get(*m).is_some_and(|b| b[..] == data[..])).cloned());
        let media = match existing {
            Some(m) => m,
            None => {
                let mut n = 1;
                while self.has(&format!("{media_dir}/image_ct{n}.{ext}")) {
                    n += 1;
                }
                let media = format!("{media_dir}/image_ct{n}.{ext}");
                self.media_by_ptr.insert((Rc::as_ptr(&data) as *const u8 as usize, media_dir.to_string(), ext.clone()), media.clone());
                self.files.insert(media.clone(), data);
                self.media_index.insert(key, media.clone());
                self.ensure_default_content_type(&ext, &image_mime(&ext));
                // a new file cannot be referenced yet
                let target = relative_path(part.rfind('/').map(|i| &part[..i]).unwrap_or(""), &media);
                return Ok(self.add_relationship(part, rel::IMAGE, &target, false));
            }
        };
        let part_dir = part.rfind('/').map(|i| &part[..i]).unwrap_or("");
        let target = relative_path(part_dir, &media);
        if let Some(r) =
            self.relationships(part).into_iter().find(|r| r.rel_type == rel::IMAGE && r.target == target && r.target_mode.is_none())
        {
            return Ok(r.id);
        }
        Ok(self.add_relationship(part, rel::IMAGE, &target, false))
    }
}

fn relative_path(from_dir: &str, to: &str) -> String {
    let from: Vec<&str> = if from_dir.is_empty() { Vec::new() } else { from_dir.split('/').collect() };
    let parts: Vec<&str> = to.split('/').collect();
    let mut i = 0;
    while i < from.len() && i + 1 < parts.len() && from[i] == parts[i] {
        i += 1;
    }
    let mut out: Vec<&str> = vec![".."; from.len() - i];
    out.extend_from_slice(&parts[i..]);
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(Package::rels_path_for("word/document.xml"), "word/_rels/document.xml.rels");
        assert_eq!(Package::resolve_target("word/document.xml", "media/a.png"), "word/media/a.png");
        assert_eq!(Package::resolve_target("ppt/slides/slide1.xml", "../media/a.png"), "ppt/media/a.png");
        assert_eq!(Package::resolve_target("x/y.xml", "/abs/z.xml"), "abs/z.xml");
        assert_eq!(relative_path("ppt/slides", "ppt/media/image_ct1.png"), "../media/image_ct1.png");
        assert_eq!(relative_path("word", "word/media/image_ct1.png"), "media/image_ct1.png");
    }
}
