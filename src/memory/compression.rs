// LZ4 compression for large memory entries.
//
// Content >1KB is compressed and stored with "lz4:" prefix.
// FTS5 still indexes the uncompressed text (stored in a separate column).

const COMPRESSION_THRESHOLD: usize = 1024; // 1KB
const LZ4_PREFIX: &str = "lz4:";

/// Compress content if it exceeds the threshold.
/// Returns (`stored_content`, `is_compressed`).
pub fn maybe_compress(content: &str) -> (String, bool) {
    if content.len() <= COMPRESSION_THRESHOLD {
        return (content.to_string(), false);
    }

    let compressed = lz4_flex::compress_prepend_size(content.as_bytes());
    let hex = hex_encode(&compressed);
    (format!("{LZ4_PREFIX}{hex}"), true)
}

/// Decompress content if it has the LZ4 prefix, otherwise return as-is.
pub fn maybe_decompress(stored: &str) -> anyhow::Result<String> {
    if let Some(hex) = stored.strip_prefix(LZ4_PREFIX) {
        let compressed = hex_decode(hex)?;
        let decompressed = lz4_flex::decompress_size_prepended(&compressed)
            .map_err(|e| anyhow::anyhow!("LZ4 decompression failed: {e}"))?;
        String::from_utf8(decompressed)
            .map_err(|e| anyhow::anyhow!("Decompressed data is not valid UTF-8: {e}"))
    } else {
        Ok(stored.to_string())
    }
}

/// Returns true if content is LZ4-compressed.
pub fn is_compressed(stored: &str) -> bool {
    stored.starts_with(LZ4_PREFIX)
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(hex: &str) -> anyhow::Result<Vec<u8>> {
    #[allow(clippy::manual_is_multiple_of)]
    if hex.len() % 2 != 0 {
        anyhow::bail!("Hex string has odd length");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| anyhow::anyhow!("Invalid hex at position {i}: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_content_not_compressed() {
        let (stored, compressed) = maybe_compress("short");
        assert!(!compressed);
        assert_eq!(stored, "short");
    }

    #[test]
    fn large_content_compressed() {
        let content = "a".repeat(2000);
        let (stored, compressed) = maybe_compress(&content);
        assert!(compressed);
        assert!(stored.starts_with(LZ4_PREFIX));
        assert!(stored.len() < content.len(), "Compressed should be smaller");
    }

    #[test]
    fn roundtrip() {
        let content = "hello world! ".repeat(200);
        let (stored, compressed) = maybe_compress(&content);
        assert!(compressed);

        let decompressed = maybe_decompress(&stored).unwrap();
        assert_eq!(decompressed, content);
    }

    #[test]
    fn uncompressed_passthrough() {
        let result = maybe_decompress("plain text").unwrap();
        assert_eq!(result, "plain text");
    }

    #[test]
    fn is_compressed_detects_prefix() {
        assert!(is_compressed("lz4:aabbcc"));
        assert!(!is_compressed("plain text"));
        assert!(!is_compressed(""));
    }

    #[test]
    fn exact_threshold_not_compressed() {
        let content = "a".repeat(COMPRESSION_THRESHOLD);
        let (_, compressed) = maybe_compress(&content);
        assert!(!compressed);
    }

    #[test]
    fn just_over_threshold_compressed() {
        let content = "a".repeat(COMPRESSION_THRESHOLD + 1);
        let (_, compressed) = maybe_compress(&content);
        assert!(compressed);
    }
}
