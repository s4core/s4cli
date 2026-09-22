//! Alias configuration stored in `~/.s4/config.toml` (tab-separated lines).

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AliasConfig {
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
    pub path_style: bool,
}

impl AliasConfig {
    /// Whether both aliases reach the same server with the same credentials, so
    /// a server-side copy between them works.
    pub fn same_account(&self, other: &AliasConfig) -> bool {
        self.endpoint == other.endpoint && self.access_key == other.access_key
    }
}

#[derive(Debug, Default)]
pub struct AppConfig {
    pub aliases: BTreeMap<String, AliasConfig>,
}

impl AppConfig {
    pub fn alias(&self, name: &str) -> Result<&AliasConfig, String> {
        self.aliases
            .get(name)
            .ok_or_else(|| format!("unknown alias: {name}"))
    }
}

pub fn resolve_config_path(custom_dir: Option<&Path>) -> Result<PathBuf, String> {
    match custom_dir {
        Some(p) => Ok(p.join("config.toml")),
        None => {
            let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
            Ok(PathBuf::from(home).join(".s4").join("config.toml"))
        }
    }
}

pub fn load_config(path: &Path) -> Result<AppConfig, String> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    restrict_permissions(path);
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_config(&text)
}

/// Writes the config atomically; the file holds secret keys, so it is created
/// readable by the owner only (0600) inside a 0700 directory.
pub fn save_config(path: &Path, cfg: &AppConfig) -> Result<(), String> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !dir.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).map_err(|e| e.to_string())?;
    }

    let tmp = dir.join(format!(".config.toml.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&tmp)
        .and_then(|mut file| file.write_all(serialize_config(cfg).as_bytes()))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

/// Tightens a config left group/world-readable by older versions (best effort).
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

fn parse_config(text: &str) -> Result<AppConfig, String> {
    let mut cfg = AppConfig::default();
    for (ln, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 6 {
            return Err(format!("invalid config at line {}", ln + 1));
        }
        cfg.aliases.insert(
            parts[0].to_string(),
            AliasConfig {
                endpoint: parts[1].to_string(),
                access_key: parts[2].to_string(),
                secret_key: parts[3].to_string(),
                region: parts[4].to_string(),
                path_style: parts[5] == "1",
            },
        );
    }
    Ok(cfg)
}

fn serialize_config(cfg: &AppConfig) -> String {
    let mut out = String::new();
    for (name, a) in &cfg.aliases {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            name,
            a.endpoint,
            a.access_key,
            a.secret_key,
            a.region,
            if a.path_style { "1" } else { "0" }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{AliasConfig, AppConfig, parse_config, serialize_config};
    use std::collections::BTreeMap;

    #[test]
    fn roundtrip_config() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "local".to_string(),
            AliasConfig {
                endpoint: "http://127.0.0.1:9000".to_string(),
                access_key: "minio".to_string(),
                secret_key: "minio123".to_string(),
                region: "us-east-1".to_string(),
                path_style: true,
            },
        );
        let cfg = AppConfig { aliases };

        let text = serialize_config(&cfg);
        let parsed = parse_config(&text).expect("config should parse");
        assert_eq!(parsed.aliases.len(), 1);
        let alias = parsed.aliases.get("local").expect("alias exists");
        assert!(alias.path_style);
        assert_eq!(alias.region, "us-east-1");
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_private() {
        use super::save_config;
        use crate::util::TestDir;
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let path = dir.path().join("nested").join("config.toml");
        save_config(&path, &AppConfig::default()).expect("save");
        save_config(&path, &AppConfig::default()).expect("overwrite");
        let mode = |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode();
        assert_eq!(mode(&path) & 0o777, 0o600);
        assert_eq!(mode(path.parent().expect("dir")) & 0o777, 0o700);
    }
}
