//! Minimal string-based XML helpers for S3 request and response bodies.

/// Returns the raw inner text of every `<tag>...</tag>` element (not nesting-aware).
pub fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");

    let mut out = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find(&open) {
        let after_open = &remaining[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        out.push(after_open[..end].to_string());
        remaining = &after_open[end + close.len()..];
    }
    out
}

/// The first `<tag>` value, unescaped.
pub fn first_tag_value(xml: &str, tag: &str) -> Option<String> {
    tag_values(xml, tag).first().map(|v| unescape(v))
}

/// Whether a paginated listing response has more pages.
pub fn is_truncated(xml: &str) -> bool {
    tag_values(xml, "IsTruncated")
        .first()
        .is_some_and(|v| v.trim() == "true")
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Decodes the predefined entities and numeric character references in one pass.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest
            .find(';')
            .and_then(|end| Some((decode_entity(&rest[1..end])?, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{escape, tag_values, unescape};

    #[test]
    fn tag_values_returns_blocks() {
        let xml =
            "<Root><Version><Key>a.txt</Key></Version><Version><Key>b.txt</Key></Version></Root>";
        let blocks = tag_values(xml, "Version");
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("<Key>a.txt</Key>"));
        assert!(blocks[1].contains("<Key>b.txt</Key>"));
    }

    #[test]
    fn tag_values_returns_keys() {
        let xml = "<ListBucketResult><Contents><Key>a.txt</Key></Contents><Contents><Key>dir/b.txt</Key></Contents></ListBucketResult>";
        let keys = tag_values(xml, "Key");
        assert_eq!(keys, vec!["a.txt".to_string(), "dir/b.txt".to_string()]);
    }

    #[test]
    fn unescape_works() {
        assert_eq!(unescape("a&amp;b&quot;c"), "a&b\"c");
    }

    #[test]
    fn unescape_decodes_each_entity_once() {
        assert_eq!(unescape("a&amp;lt;b"), "a&lt;b");
        assert_eq!(unescape(&escape("<&'\">")), "<&'\">");
    }

    #[test]
    fn unescape_handles_numeric_references_and_stray_ampersands() {
        assert_eq!(unescape("&#x41;&#66;&#xd;"), "AB\r");
        assert_eq!(unescape("a & b &unknown; c"), "a & b &unknown; c");
    }
}
