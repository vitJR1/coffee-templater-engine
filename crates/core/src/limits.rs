//! Safety limits (spec §10). `0` means "no limit".

use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    pub max_input_bytes: u64,
    pub max_entries: u64,
    pub max_uncompressed_bytes: u64,
    pub max_compression_ratio: u64,
    pub max_xml_depth: u64,
    pub max_output_bytes: u64,
    pub max_loop_iterations: u64,
    pub max_host_calls: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_bytes: 100 * MIB,
            max_entries: 10_000,
            max_uncompressed_bytes: 500 * MIB,
            max_compression_ratio: 200,
            max_xml_depth: 256,
            max_output_bytes: 500 * MIB,
            max_loop_iterations: 1_000_000,
            max_host_calls: 1_000_000,
        }
    }
}

/// Entries smaller than this are not checked for compression ratio: tiny, highly repetitive
/// XML parts legitimately compress very well.
pub const RATIO_CHECK_THRESHOLD: u64 = MIB;

/// Hard cap on XML nesting, applied even when `maxXmlDepth` is 0 or larger: the DOM is recursive,
/// and this keeps it well inside the 1 MiB wasm stack.
pub const HARD_MAX_XML_DEPTH: u64 = 1024;

/// Maximum nesting of template sections (`{{#a}}{{#b}}…`): compilation, rendering and inspection
/// recurse once per level.
pub const MAX_SECTION_DEPTH: usize = 128;

/// Effective XML depth limit.
pub fn xml_depth(limit: u64) -> u64 {
    if limit == 0 {
        HARD_MAX_XML_DEPTH
    } else {
        limit.min(HARD_MAX_XML_DEPTH)
    }
}

/// Returns true when `value` exceeds `limit` (`0` = unlimited).
#[inline]
pub fn exceeds(value: u64, limit: u64) -> bool {
    limit != 0 && value > limit
}
