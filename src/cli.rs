//! Global flag parsing, help text and shared argument helpers.

use std::path::PathBuf;

use crate::s3::HttpOptions;

#[derive(Debug, Default)]
pub struct GlobalOpts {
    pub config_dir: Option<PathBuf>,
    pub json: bool,
    pub debug: bool,
    pub http: HttpOptions,
}

/// Returns the value that follows the flag at `args[i]`.
pub fn flag_value<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a str, String> {
    args.get(i + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} expects a value"))
}

/// Splits leading global flags from the command and its arguments.
pub fn parse_globals(mut args: Vec<String>) -> Result<(GlobalOpts, Vec<String>), String> {
    let mut opts = GlobalOpts::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-C" | "--config-dir" => {
                let value = flag_value(&args, i, "--config-dir")?;
                opts.config_dir = Some(PathBuf::from(value));
                i += 2;
            }
            "--json" => {
                opts.json = true;
                i += 1;
            }
            "--debug" => {
                opts.debug = true;
                i += 1;
            }
            "--insecure" => {
                opts.http.insecure = true;
                i += 1;
            }
            "--resolve" => {
                let value = flag_value(&args, i, "--resolve")?;
                opts.http.resolve.push(value.to_string());
                i += 2;
            }
            "--limit-upload" => {
                let value = flag_value(&args, i, "--limit-upload")?;
                opts.http.limit_upload = Some(value.to_string());
                i += 2;
            }
            "--limit-download" => {
                let value = flag_value(&args, i, "--limit-download")?;
                opts.http.limit_download = Some(value.to_string());
                i += 2;
            }
            "--custom-header" | "-H" => {
                let value = flag_value(&args, i, "--custom-header")?;
                opts.http.custom_headers.push(value.to_string());
                i += 2;
            }
            "--help" | "-h" | "--version" | "-v" => break,
            x if x.starts_with('-') => return Err(format!("unknown global flag: {x}")),
            _ => break,
        }
    }

    let rest = args.split_off(i);
    Ok((opts, rest))
}

pub fn print_help() {
    println!(
        "s4 - S3 client utility in Rust

USAGE:
  s4 [FLAGS] COMMAND [ARGS]

COMMANDS:
  alias      manage aliases in local config
  ls         list buckets/objects
  mb         make bucket
  rb         remove bucket (--force: with all objects, --bypass: ignore governance lock)
  legalhold  manage legal hold for object(s) (set/clear/info)
  retention  manage retention for object(s) (set/clear/info)
  sql        run SQL queries on objects
  replicate  manage server-side bucket replication [placeholder]
  put        upload object
  get        download object
  rm         remove object (--bypass: ignore governance lock)
  stat       object metadata (raw headers)
  cat        print object content
  cors       manage bucket CORS configuration (set/get/remove)
  encrypt    manage bucket encryption config (set/clear/info)
  event      manage bucket notifications (add/remove/list)
  idp        manage identity providers (openid/ldap) [placeholder]
  ilm        manage lifecycle (rule/tier/restore) [placeholder]
  sync       sync objects from source bucket/prefix to destination
  mirror     alias for sync (mc-compatible naming)
  cp         copy object(s) between local and S3
  mv         move object(s) between local and S3
  find       find objects in bucket/prefix
  tree       show object tree in bucket/prefix
  head       print first N lines from object
  pipe       upload stdin stream to object
  ping       perform liveness check
  ready      check that alias endpoint is ready
  version    print version

FLAGS:
  -C, --config-dir <DIR>
  --json
  --debug
  --insecure
  --resolve <HOST:PORT=IP>
  --limit-upload <RATE>
  --limit-download <RATE>
  -H, --custom-header <KEY:VALUE>
  -h, --help
  -v, --version

NOTE:
  mb supports --with-lock for object-lock buckets (used by legalhold tests)"
    );
}

/// Builds an owned argument vector for parser tests.
#[cfg(test)]
pub fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::{args, parse_globals};

    #[test]
    fn parse_globals_extended_flags() {
        let (opts, rest) = parse_globals(args(&[
            "--insecure",
            "--resolve",
            "minio.local:9000=127.0.0.1",
            "--limit-upload",
            "1M",
            "--limit-download",
            "2M",
            "-H",
            "x-test: one",
            "--custom-header",
            "x-test2: two",
            "ls",
            "a/b",
        ]))
        .expect("parse globals should succeed");
        assert!(opts.http.insecure);
        assert_eq!(
            opts.http.resolve,
            vec!["minio.local:9000=127.0.0.1".to_string()]
        );
        assert_eq!(opts.http.limit_upload.as_deref(), Some("1M"));
        assert_eq!(opts.http.limit_download.as_deref(), Some("2M"));
        assert_eq!(
            opts.http.custom_headers,
            vec!["x-test: one".to_string(), "x-test2: two".to_string()]
        );
        assert_eq!(rest, args(&["ls", "a/b"]));
    }
}
