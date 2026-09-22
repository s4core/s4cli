//! Endpoint parsing and URI encoding.

#[derive(Debug)]
pub struct Endpoint {
    pub scheme: String,
    pub host: String,
    pub base_path: String,
}

impl Endpoint {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (scheme, rest) = if let Some(v) = raw.strip_prefix("http://") {
            ("http", v)
        } else if let Some(v) = raw.strip_prefix("https://") {
            ("https", v)
        } else {
            return Err("endpoint must start with http:// or https://".to_string());
        };

        let mut parts = rest.splitn(2, '/');
        let host = parts.next().unwrap_or("").to_string();
        if host.is_empty() {
            return Err("endpoint host is empty".to_string());
        }
        let base_path = match parts.next() {
            Some(v) if !v.is_empty() => format!("/{}", v.trim_end_matches('/')),
            _ => String::new(),
        };

        Ok(Self {
            scheme: scheme.to_string(),
            host,
            base_path,
        })
    }

    /// Path-style request path: `<base>/<bucket>/<key>`, or `/` for the service root.
    pub fn object_path(&self, bucket: &str, key: Option<&str>) -> String {
        let mut path = self.base_path.clone();
        if !bucket.is_empty() {
            path.push('/');
            path.push_str(&uri_encode_path(bucket));
        }
        if let Some(k) = key {
            path.push('/');
            path.push_str(&uri_encode_path(k));
        }
        if path.is_empty() {
            path.push('/');
        }
        path
    }
}

fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.' | '~')
            || (keep_slash && c == '/')
        {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encodes everything except unreserved characters and `/`.
pub fn uri_encode_path(s: &str) -> String {
    uri_encode(s, true)
}

/// Percent-encodes everything except unreserved characters.
pub fn uri_encode_query_component(s: &str) -> String {
    uri_encode(s, false)
}

#[cfg(test)]
mod tests {
    use super::{uri_encode_path, uri_encode_query_component};

    #[test]
    fn uri_encode_works() {
        assert_eq!(uri_encode_path("a b/c"), "a%20b/c");
    }

    #[test]
    fn uri_encode_query_component_works() {
        assert_eq!(uri_encode_query_component("a b/+"), "a%20b%2F%2B");
    }
}
