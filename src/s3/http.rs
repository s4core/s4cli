//! Connection-level options shared by every request (set from global flags).

use std::process::Command;

#[derive(Debug, Default, Clone)]
pub struct HttpOptions {
    pub insecure: bool,
    pub resolve: Vec<String>,
    pub limit_upload: Option<String>,
    pub limit_download: Option<String>,
    /// Raw `Name: value` lines added to every request.
    pub custom_headers: Vec<String>,
}

impl HttpOptions {
    /// Adds TLS, DNS override and rate-limit flags; headers are sent separately.
    pub fn apply_to_curl(&self, cmd: &mut Command, is_upload: bool) {
        if self.insecure {
            cmd.arg("-k");
        }
        for resolve in &self.resolve {
            cmd.arg("--resolve").arg(normalize_resolve_entry(resolve));
        }
        let rate_limit = if is_upload {
            &self.limit_upload
        } else {
            &self.limit_download
        };
        if let Some(limit) = rate_limit {
            cmd.arg("--limit-rate").arg(limit);
        }
    }
}

/// Accepts both `host:port=ip` and curl's native `host:port:ip`.
fn normalize_resolve_entry(entry: &str) -> String {
    if entry.contains('=') {
        entry.replacen('=', ":", 1)
    } else {
        entry.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_resolve_entry;

    #[test]
    fn normalize_resolve_entry_supports_equals_and_colon_formats() {
        assert_eq!(
            normalize_resolve_entry("minio.local:9000=127.0.0.1"),
            "minio.local:9000:127.0.0.1"
        );
        assert_eq!(
            normalize_resolve_entry("minio.local:9000:127.0.0.1"),
            "minio.local:9000:127.0.0.1"
        );
    }
}
