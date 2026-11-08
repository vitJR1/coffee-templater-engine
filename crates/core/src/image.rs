//! Image values for image tags (`{{%logo}}`).
//!
//! An image is an object with `data` (`$blob`/`$base64`, or a base64 / data-URL string) and
//! optional `width`, `height` (pixels at 96 dpi), `extension` and `alt`.

use crate::value::{decode_base64, Value};
use std::rc::Rc;

pub const EMU_PER_PX: f64 = 9525.0;

#[derive(Debug, Clone)]
pub struct ResolvedImage {
    pub data: Rc<[u8]>,
    pub extension: String,
    pub width: f64,
    pub height: f64,
    pub alt: String,
}

/// Image formats accepted in `extension` (lower case) and their content types. Anything else is
/// rejected: the value ends up in a package path and a content type, and names such as `rels` or
/// `xml` would make the picture take over the content type of package parts.
pub const IMAGE_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("svg", "image/svg+xml"),
    ("webp", "image/webp"),
    ("emf", "image/x-emf"),
    ("wmf", "image/x-wmf"),
    ("ico", "image/x-icon"),
];

pub fn image_mime(ext: &str) -> Option<&'static str> {
    IMAGE_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

pub fn is_safe_extension(ext: &str) -> bool {
    image_mime(ext).is_some()
}

pub fn is_image_value(v: &Value) -> bool {
    matches!(v, Value::Object(o) if o.contains_key("data"))
}

fn opt_num(v: Option<&Value>) -> Option<f64> {
    match v {
        None | Some(Value::Undefined) | Some(Value::Null) => None,
        Some(x) => Some(x.js_number()).filter(|n| n.is_finite()),
    }
}

pub fn resolve_image(value: &Value) -> Result<ResolvedImage, String> {
    let Value::Object(o) = value else { return Err("Image value must be an object".into()) };
    let data: Rc<[u8]> = match o.get("data") {
        Some(Value::Bytes(b)) => b.clone(),
        Some(Value::String(s)) => Rc::from(decode_base64(s).ok_or("Image data is not valid base64")?),
        Some(Value::Array(a)) => {
            // plain byte arrays (`[137, 80, 78, ...]`)
            let bytes: Option<Vec<u8>> = a
                .iter()
                .map(|x| match x {
                    Value::Number(n) if (0.0..=255.0).contains(&n.value) && n.value.fract() == 0.0 => Some(n.value as u8),
                    _ => None,
                })
                .collect();
            Rc::from(bytes.ok_or("Image data array must contain bytes")?)
        }
        _ => return Err("Image data must be bytes ($blob), base64 or a data URL".into()),
    };
    let info = sniff_image(&data);
    let ext_given = match o.get("extension") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    let extension = ext_given.or_else(|| info.as_ref().map(|i| i.0.to_string())).unwrap_or_default().trim_start_matches('.').to_lowercase();
    if extension.is_empty() {
        return Err("Cannot detect image format; pass `extension`".into());
    }
    if !is_safe_extension(&extension) {
        return Err(format!(
            "Invalid image extension {extension:?}: expected one of {}",
            IMAGE_TYPES.iter().map(|(e, _)| *e).collect::<Vec<_>>().join(", ")
        ));
    }
    let natural = info.filter(|i| i.1 > 0.0 && i.2 > 0.0);
    let (w, h) = (opt_num(o.get("width")), opt_num(o.get("height")));
    let (width, height) = match (w, h) {
        (None, None) => natural.map(|n| (n.1, n.2)).unwrap_or((100.0, 100.0)),
        (None, Some(h)) => (natural.map(|n| h * n.1 / n.2).unwrap_or(h), h),
        (Some(w), None) => (w, natural.map(|n| w * n.2 / n.1).unwrap_or(w)),
        (Some(w), Some(h)) => (w, h),
    };
    let alt = match o.get("alt") {
        Some(v) if !v.is_nullish() => v.js_string(),
        _ => String::new(),
    };
    Ok(ResolvedImage { data, extension, width, height, alt })
}

/// Detects format and pixel size of PNG, JPEG, GIF, BMP and SVG images.
pub fn sniff_image(d: &[u8]) -> Option<(&'static str, f64, f64)> {
    let u32be = |o: usize| d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64).unwrap_or(0.0);
    let u16be = |o: usize| d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as f64).unwrap_or(0.0);
    let u16le = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as f64).unwrap_or(0.0);
    let i32le = |o: usize| d.get(o..o + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64).unwrap_or(0.0);
    if d.len() > 24 && d.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        return Some(("png", u32be(16), u32be(20)));
    }
    if d.len() > 10 && d.starts_with(b"GIF") {
        return Some(("gif", u16le(6), u16le(8)));
    }
    if d.len() > 26 && d.starts_with(b"BM") {
        return Some(("bmp", i32le(18).abs(), i32le(22).abs()));
    }
    if d.len() > 4 && d[0] == 0xff && d[1] == 0xd8 {
        let mut o = 2;
        while o + 9 < d.len() {
            if d[o] != 0xff {
                o += 1;
                continue;
            }
            let marker = d[o + 1];
            if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc {
                return Some(("jpeg", u16be(o + 7), u16be(o + 5)));
            }
            o += 2 + u16be(o + 2) as usize;
        }
        return Some(("jpeg", 100.0, 100.0));
    }
    let head = String::from_utf8_lossy(&d[..d.len().min(512)]).to_lowercase();
    if let Some(p) = head.find("<svg") {
        if matches!(head.as_bytes().get(p + 4), Some(c) if c.is_ascii_whitespace() || *c == b'>') {
            let dim = |name: &str| -> f64 {
                let pat = format!(" {name}=\"");
                head.find(&pat)
                    .map(|i| {
                        let v: String = head[i + pat.len()..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                        v.parse().unwrap_or(100.0)
                    })
                    .unwrap_or(100.0)
            };
            return Some(("svg", dim("width"), dim("height")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::parse_data;

    const PNG_2X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGP4z8DwnwEIGBgAHvAD/c5k+8YAAAAASUVORK5CYII=";

    #[test]
    fn resolves_size_from_header() {
        let v = parse_data(&format!(r#"{{"data":{{"$base64":"{PNG_2X1}"}},"width":120}}"#), &[]).unwrap();
        let img = resolve_image(&v).unwrap();
        assert_eq!((img.extension.as_str(), img.width, img.height), ("png", 120.0, 60.0));
        let v = parse_data(&format!(r#"{{"data":"data:image/png;base64,{PNG_2X1}"}}"#), &[]).unwrap();
        assert_eq!(resolve_image(&v).unwrap().width, 2.0);
        assert!(resolve_image(&parse_data(r#"{"data":{"$base64":"AAAA"}}"#, &[]).unwrap()).is_err());
    }
}
