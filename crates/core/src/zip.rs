//! Minimal ZIP reader/writer for OPC packages.
//!
//! Reading enforces the safety limits of spec §10 (entry count, total size, compression ratio,
//! path sanity, CRC). Writing is deterministic (spec §12): entries in the given order, fixed
//! timestamp 1980-01-01 00:00:00, deflate level 6 by the bundled `miniz_oxide`.

use crate::error::{Error, Result};
use crate::limits::{exceeds, Limits, RATIO_CHECK_THRESHOLD};

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
/// MS-DOS date for 1980-01-01 (time 00:00:00 is 0).
const DOS_DATE: u16 = (1 << 5) | 1;
const DEFLATE_LEVEL: u8 = 6;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid_document(msg)
}

fn not_zip() -> Error {
    bad("Input is not a valid OOXML (zip) file")
}

/// Validates an entry name: no absolute paths, no `..`, no backslashes, no empty names.
fn check_name(name: &str) -> Result<()> {
    let bad_name = name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.split('/').any(|s| s == "..")
        || name.as_bytes().get(1) == Some(&b':');
    if bad_name {
        return Err(bad(format!("Unsafe path in zip: \"{name}\"")));
    }
    Ok(())
}

pub fn read(data: &[u8], limits: &Limits) -> Result<Vec<(String, Vec<u8>)>> {
    if data.len() < 22 {
        return Err(not_zip());
    }
    // End of central directory: the last 22 bytes plus up to 64 KiB of comment.
    let min = data.len().saturating_sub(22 + 0xFFFF);
    let eocd = (min..=data.len() - 22).rev().find(|&i| u32_at(data, i) == Some(SIG_EOCD)).ok_or_else(not_zip)?;
    let count = u16_at(data, eocd + 10).ok_or_else(not_zip)? as usize;
    let cd_size = u32_at(data, eocd + 12).ok_or_else(not_zip)? as usize;
    let cd_offset = u32_at(data, eocd + 16).ok_or_else(not_zip)? as usize;
    if count == 0xFFFF || cd_offset == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
        return Err(bad("ZIP64 archives are not supported"));
    }
    if exceeds(count as u64, limits.max_entries) {
        return Err(Error::limit("maxEntries", limits.max_entries));
    }
    if cd_offset.checked_add(cd_size).is_none_or(|e| e > data.len()) {
        return Err(not_zip());
    }

    let mut out: Vec<(String, Vec<u8>)> = Vec::with_capacity(count);
    let mut seen = std::collections::HashSet::new();
    let mut total: u64 = 0;
    let mut p = cd_offset;
    for _ in 0..count {
        if u32_at(data, p) != Some(SIG_CENTRAL) {
            return Err(bad("Corrupt zip central directory"));
        }
        let field = |o: usize| u16_at(data, p + o).ok_or_else(not_zip);
        let flags = field(8)?;
        let method = field(10)?;
        let crc = u32_at(data, p + 16).ok_or_else(not_zip)?;
        let csize = u32_at(data, p + 20).ok_or_else(not_zip)? as u64;
        let usize_ = u32_at(data, p + 24).ok_or_else(not_zip)? as u64;
        let nlen = field(28)? as usize;
        let elen = field(30)? as usize;
        let clen = field(32)? as usize;
        let local = u32_at(data, p + 42).ok_or_else(not_zip)? as usize;
        let name_bytes = data.get(p + 46..p + 46 + nlen).ok_or_else(not_zip)?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        p += 46 + nlen + elen + clen;

        if flags & 1 != 0 {
            return Err(bad(format!("Encrypted zip entry \"{name}\" is not supported")));
        }
        if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
            return Err(bad("ZIP64 archives are not supported"));
        }
        check_name(&name)?;
        if !seen.insert(name.clone()) {
            return Err(bad(format!("Duplicate zip entry \"{name}\"")));
        }
        total += usize_;
        if exceeds(total, limits.max_uncompressed_bytes) {
            return Err(Error::limit("maxUncompressedBytes", limits.max_uncompressed_bytes));
        }
        if limits.max_compression_ratio != 0 && usize_ > RATIO_CHECK_THRESHOLD && usize_ / csize.max(1) > limits.max_compression_ratio {
            return Err(Error::limit("maxCompressionRatio", limits.max_compression_ratio).with_detail("entry", name.as_str()));
        }

        if u32_at(data, local) != Some(SIG_LOCAL) {
            return Err(bad(format!("Corrupt zip entry \"{name}\"")));
        }
        let lnlen = u16_at(data, local + 26).ok_or_else(not_zip)? as usize;
        let lelen = u16_at(data, local + 28).ok_or_else(not_zip)? as usize;
        let start = local + 30 + lnlen + lelen;
        let raw = data.get(start..start + csize as usize).ok_or_else(|| bad(format!("Truncated zip entry \"{name}\"")))?;
        let bytes = match method {
            0 => raw.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, usize_ as usize)
                .map_err(|_| bad(format!("Cannot inflate zip entry \"{name}\"")))?,
            m => return Err(bad(format!("Unsupported zip compression method {m} for \"{name}\""))),
        };
        if bytes.len() as u64 != usize_ || crc32fast::hash(&bytes) != crc {
            return Err(bad(format!("Corrupt zip entry \"{name}\" (size or CRC mismatch)")));
        }
        out.push((name, bytes));
    }
    Ok(out)
}

pub fn write<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut count: usize = 0;
    for (name, data) in entries {
        check_name(name)?;
        let crc = crc32fast::hash(data);
        let deflated = miniz_oxide::deflate::compress_to_vec(data, DEFLATE_LEVEL);
        let (method, body): (u16, &[u8]) = if deflated.len() < data.len() { (8, &deflated) } else { (0, data) };
        let offset = out.len();
        if offset > u32::MAX as usize || data.len() > u32::MAX as usize {
            return Err(Error::invalid_document("Output document is too large for a zip file"));
        }
        let flags: u16 = if name.is_ascii() { 0 } else { 0x0800 };
        let n = name.as_bytes();
        let header = |sig: u32, buf: &mut Vec<u8>, central: bool| {
            buf.extend_from_slice(&sig.to_le_bytes());
            if central {
                buf.extend_from_slice(&20u16.to_le_bytes()); // version made by
            }
            buf.extend_from_slice(&20u16.to_le_bytes()); // version needed
            buf.extend_from_slice(&flags.to_le_bytes());
            buf.extend_from_slice(&method.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // time
            buf.extend_from_slice(&DOS_DATE.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(n.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra
            if central {
                buf.extend_from_slice(&0u16.to_le_bytes()); // comment
                buf.extend_from_slice(&0u16.to_le_bytes()); // disk
                buf.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
                buf.extend_from_slice(&0u32.to_le_bytes()); // external attrs
                buf.extend_from_slice(&(offset as u32).to_le_bytes());
            }
            buf.extend_from_slice(n);
        };
        header(SIG_LOCAL, &mut out, false);
        out.extend_from_slice(body);
        header(SIG_CENTRAL, &mut central, true);
        count += 1;
    }
    if count > 0xFFFE {
        return Err(Error::invalid_document("Too many entries for a zip file"));
    }
    let cd_offset = out.len();
    out.extend_from_slice(&central);
    out.extend_from_slice(&SIG_EOCD.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]); // disk numbers
    out.extend_from_slice(&(count as u16).to_le_bytes());
    out.extend_from_slice(&(count as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&(cd_offset as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_determinism() {
        let big = "hello ".repeat(1000);
        let entries = vec![("[Content_Types].xml", b"<Types/>".as_slice()), ("word/document.xml", big.as_bytes()), ("ü.txt", b"x")];
        let a = write(entries.clone()).unwrap();
        let b = write(entries).unwrap();
        assert_eq!(a, b);
        let back = read(&a, &Limits::default()).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].1, big.as_bytes());
        assert_eq!(back[2].0, "ü.txt");
    }

    #[test]
    fn rejects_unsafe_and_limits() {
        assert!(write(vec![("../evil", b"x".as_slice())]).is_err());
        assert!(write(vec![("word/media/x.png/../../evil", b"x".as_slice())]).is_err());
        // a crafted archive: same name length, patched to "../evil" in both headers
        let mut z = write(vec![("aa/evil", b"x".as_slice())]).unwrap();
        for i in 0..z.len() - 3 {
            if &z[i..i + 3] == b"aa/" {
                z[i..i + 3].copy_from_slice(b"../");
            }
        }
        assert!(read(&z, &Limits::default()).is_err());
        let z = write(vec![("a", b"x".as_slice()), ("b", b"y".as_slice())]).unwrap();
        let limits = Limits { max_entries: 1, ..Limits::default() };
        assert_eq!(read(&z, &limits).unwrap_err().code, crate::ErrorCode::LimitExceeded);
        let bomb = vec![0u8; 4 * 1024 * 1024];
        let z = write(vec![("bomb", bomb.as_slice())]).unwrap();
        assert_eq!(read(&z, &Limits::default()).unwrap_err().code, crate::ErrorCode::LimitExceeded);
        let limits = Limits { max_uncompressed_bytes: 1000, ..Limits::default() };
        assert_eq!(read(&z, &limits).unwrap_err().code, crate::ErrorCode::LimitExceeded);
        assert!(read(b"not a zip at all, definitely not", &Limits::default()).is_err());
    }
}
