//! `alias[/bucket[/key]]` command-line targets.

#[derive(Debug)]
pub struct S3Target {
    pub alias: String,
    pub bucket: Option<String>,
    pub key: Option<String>,
}

impl S3Target {
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut parts = input.splitn(3, '/');
        let alias = parts.next().unwrap_or_default().to_string();
        if alias.is_empty() {
            return Err("target alias is empty".to_string());
        }
        let bucket = parts.next().map(ToString::to_string);
        let key = parts.next().map(ToString::to_string);
        Ok(Self { alias, bucket, key })
    }

    pub fn require_bucket(&self, cmd: &str) -> Result<&str, String> {
        self.bucket
            .as_deref()
            .ok_or_else(|| format!("{cmd} requires alias/bucket"))
    }

    pub fn require_key(&self, cmd: &str) -> Result<&str, String> {
        self.key
            .as_deref()
            .ok_or_else(|| format!("{cmd} requires alias/bucket/key"))
    }

    /// The key part used as a listing prefix; empty when absent.
    pub fn prefix(&self) -> &str {
        self.key.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::S3Target;

    #[test]
    fn parse_target_with_key() {
        let t = S3Target::parse("local/bucket/folder/file.txt").expect("target should parse");
        assert_eq!(t.alias, "local");
        assert_eq!(t.bucket.as_deref(), Some("bucket"));
        assert_eq!(t.key.as_deref(), Some("folder/file.txt"));
    }
}
