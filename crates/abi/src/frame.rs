//! Message frames (spec §4): a JSON header plus binary blobs.
//!
//! ```text
//! offset  size   field
//! 0       4      magic = "CTF1"
//! 4       4      json_len (u32 LE)
//! 8       N      JSON (UTF-8, no BOM)
//! 8+N     4      blob_count (u32 LE)
//! ...            blob_count × { u32 blob_len, bytes }
//! ```

pub const MAGIC: &[u8; 4] = b"CTF1";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    pub json: String,
    pub blobs: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError(pub String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Frame {
    pub fn new(json: impl Into<String>, blobs: Vec<Vec<u8>>) -> Self {
        Frame { json: json.into(), blobs }
    }

    pub fn encode(&self) -> Vec<u8> {
        let size = 12 + self.json.len() + self.blobs.iter().map(|b| 4 + b.len()).sum::<usize>();
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.json.len() as u32).to_le_bytes());
        out.extend_from_slice(self.json.as_bytes());
        out.extend_from_slice(&(self.blobs.len() as u32).to_le_bytes());
        for b in &self.blobs {
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        out
    }

    pub fn decode(data: &[u8]) -> Result<Frame, FrameError> {
        let err = |m: &str| FrameError(format!("Invalid frame: {m}"));
        if data.len() < 12 || &data[..4] != MAGIC {
            return Err(err("bad magic or too short"));
        }
        let mut p = 4;
        let read_u32 = |p: &mut usize| -> Result<usize, FrameError> {
            let b = data.get(*p..*p + 4).ok_or_else(|| err("truncated"))?;
            *p += 4;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        let json_len = read_u32(&mut p)?;
        let json = data.get(p..p.checked_add(json_len).ok_or_else(|| err("bad json length"))?).ok_or_else(|| err("truncated JSON"))?;
        if json.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return Err(err("JSON must not start with a BOM"));
        }
        let json = std::str::from_utf8(json).map_err(|_| err("JSON is not UTF-8"))?.to_string();
        p += json_len;
        let count = read_u32(&mut p)?;
        if count > data.len() / 4 {
            return Err(err("bad blob count"));
        }
        let mut blobs = Vec::with_capacity(count);
        for _ in 0..count {
            let len = read_u32(&mut p)?;
            let b = data.get(p..p.checked_add(len).ok_or_else(|| err("bad blob length"))?).ok_or_else(|| err("truncated blob"))?;
            blobs.push(b.to_vec());
            p += len;
        }
        if p != data.len() {
            return Err(err("trailing bytes"));
        }
        Ok(Frame { json, blobs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let f = Frame::new(r#"{"op":"info"}"#, vec![vec![1, 2, 3], vec![]]);
        let bytes = f.encode();
        assert_eq!(&bytes[..4], b"CTF1");
        assert_eq!(Frame::decode(&bytes).unwrap(), f);
        assert!(Frame::decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(Frame::decode(b"XXXX\0\0\0\0\0\0\0\0").is_err());
    }
}
