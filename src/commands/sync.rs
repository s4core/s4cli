//! `sync` / `mirror`: copy objects from one bucket/prefix to another.
//!
//! A prefix is treated as a directory: `alias/bucket/photos` covers `photos`
//! and `photos/...`, but not `photos2/...`.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::thread::sleep;
use std::time::Duration;

use super::Context;
use crate::cli::flag_value;
use crate::output::JsonObject;
use crate::s3::{MAX_COPY_OBJECT_SIZE, ObjectInfo, Request, S3Client};
use crate::target::S3Target;
use crate::time;
use crate::util::TempPath;

const USAGE: &str =
    "usage: s4 sync|mirror [FLAGS] <src_alias/bucket[/prefix]> <dst_alias/bucket[/prefix]>";

#[derive(Debug, Default)]
pub struct SyncOptions {
    pub overwrite: bool,
    pub dry_run: bool,
    pub remove: bool,
    pub watch: bool,
    pub excludes: Vec<String>,
    pub newer_than: Option<u64>,
    pub older_than: Option<u64>,
}

pub fn run(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (options, source, destination) = parse_sync_args(args)?;
    let src = ctx.client_for(&source)?;
    let dst = ctx.client_for(&destination)?;
    let src_bucket = source.require_bucket("sync")?;
    let dst_bucket = destination.require_bucket("sync")?;

    for pass in 0.. {
        let (copied, removed) = sync_once(&src, &dst, &source, &destination, &options, ctx.json)?;

        // In watch mode only passes that changed something are reported.
        if pass == 0 || copied + removed > 0 {
            ctx.report(
                JsonObject::new()
                    .str("status", "ok")
                    .raw("copied", copied)
                    .raw("removed", removed)
                    .raw("dry_run", options.dry_run)
                    .raw("watch", options.watch)
                    .str("src", &format!("{}/{src_bucket}", source.alias))
                    .str("dst", &format!("{}/{dst_bucket}", destination.alias)),
                format!(
                    "Synced {copied} object(s) from {}/{src_bucket} to {}/{dst_bucket} (removed: {removed}, dry-run: {}, watch: {})",
                    source.alias, destination.alias, options.dry_run, options.watch
                ),
            );
        }

        if !options.watch {
            break;
        }
        sleep(watch_interval());
    }
    Ok(())
}

/// One sync pass; returns `(copied, removed)` counts.
fn sync_once(
    src: &S3Client,
    dst: &S3Client,
    source: &S3Target,
    destination: &S3Target,
    options: &SyncOptions,
    json: bool,
) -> Result<(usize, usize), String> {
    let src_bucket = source.require_bucket("sync")?;
    let dst_bucket = destination.require_bucket("sync")?;
    let src_objects = src.list_objects(src_bucket, source.prefix())?;
    let dst_objects = dst.list_objects(dst_bucket, destination.prefix())?;
    let plan = plan(
        &src_objects,
        &dst_objects,
        source.prefix(),
        destination.prefix(),
        options,
        time::now_secs(),
    );

    for (object, dest_key) in &plan.copy {
        if options.dry_run {
            if !json {
                println!(
                    "[dry-run] copy {src_bucket}/{} -> {dst_bucket}/{dest_key}",
                    object.key
                );
            }
            continue;
        }
        copy_object(src, dst, src_bucket, dst_bucket, object, dest_key)?;
    }

    for key in &plan.remove {
        if options.dry_run {
            if !json {
                println!("[dry-run] remove {dst_bucket}/{key}");
            }
            continue;
        }
        dst.delete_object(dst_bucket, key, None, false)?;
    }

    Ok((plan.copy.len(), plan.remove.len()))
}

/// Server-side copy within one account, otherwise download + upload.
fn copy_object(
    src: &S3Client,
    dst: &S3Client,
    src_bucket: &str,
    dst_bucket: &str,
    object: &ObjectInfo,
    dest_key: &str,
) -> Result<(), String> {
    if src.alias().same_account(dst.alias()) && object.size <= MAX_COPY_OBJECT_SIZE {
        return dst.copy_object(src_bucket, &object.key, dst_bucket, dest_key);
    }
    let temp = TempPath::new("sync")?;
    src.download(
        &Request::new("GET", src_bucket).key(&object.key),
        temp.path(),
    )?;
    dst.upload_file(dst_bucket, dest_key, temp.path())
}

/// What one pass copies (source object → destination key) and removes.
#[derive(Debug, PartialEq, Eq)]
struct Plan<'a> {
    copy: Vec<(&'a ObjectInfo, String)>,
    remove: Vec<String>,
}

fn plan<'a>(
    src_objects: &'a [ObjectInfo],
    dst_objects: &[ObjectInfo],
    src_prefix: &str,
    dst_prefix: &str,
    options: &SyncOptions,
    now: u64,
) -> Plan<'a> {
    let dst_by_key: BTreeMap<&str, &ObjectInfo> = dst_objects
        .iter()
        .filter(|o| within_prefix(&o.key, dst_prefix))
        .map(|o| (o.key.as_str(), o))
        .collect();

    // Every source object keeps its destination counterpart alive, including
    // ones skipped by --exclude or the age filters.
    let mut expected = HashSet::new();
    let mut copy = Vec::new();
    for object in src_objects
        .iter()
        .filter(|o| within_prefix(&o.key, src_prefix))
    {
        let dest_key = sync_destination_key(&object.key, src_prefix, dst_prefix);
        expected.insert(dest_key.clone());
        if is_excluded(&object.key, &options.excludes) || !passes_age_filter(object, options, now) {
            continue;
        }
        if dst_by_key
            .get(dest_key.as_str())
            .is_some_and(|existing| is_up_to_date(object, existing))
        {
            continue;
        }
        copy.push((object, dest_key));
    }

    let remove = if options.remove {
        dst_by_key
            .keys()
            .filter(|key| !expected.contains(**key))
            .filter(|key| {
                let source_key = join_prefix(src_prefix, strip_dir_prefix(key, dst_prefix));
                !is_excluded(&source_key, &options.excludes)
            })
            .map(|key| key.to_string())
            .collect()
    } else {
        Vec::new()
    };

    Plan { copy, remove }
}

/// Same size and either the same ETag or a destination at least as new as the source.
fn is_up_to_date(source: &ObjectInfo, destination: &ObjectInfo) -> bool {
    source.size == destination.size
        && ((!source.etag.is_empty() && source.etag == destination.etag)
            || matches!(
                (source.last_modified, destination.last_modified),
                (Some(src), Some(dst)) if dst >= src
            ))
}

fn passes_age_filter(object: &ObjectInfo, options: &SyncOptions, now: u64) -> bool {
    if options.newer_than.is_none() && options.older_than.is_none() {
        return true;
    }
    let Some(modified) = object.last_modified else {
        return false;
    };
    let age = now.saturating_sub(modified);
    options.newer_than.is_none_or(|max| age <= max)
        && options.older_than.is_none_or(|min| age >= min)
}

fn watch_interval() -> Duration {
    let seconds = env::var("S4_SYNC_WATCH_INTERVAL_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2);
    Duration::from_secs(seconds.max(1))
}

/// Whether `key` is `prefix` itself or lies under the `prefix/` "directory".
fn within_prefix(key: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_matches('/');
    prefix.is_empty()
        || key == prefix
        || key
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `key` relative to the `prefix` directory.
fn strip_dir_prefix<'k>(key: &'k str, prefix: &str) -> &'k str {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return key;
    }
    if key == prefix {
        return "";
    }
    key.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(key)
}

fn join_prefix(prefix: &str, relative: &str) -> String {
    let prefix = prefix.trim_matches('/');
    match (prefix.is_empty(), relative.is_empty()) {
        (true, _) => relative.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{relative}"),
    }
}

/// Maps a source key to the destination by swapping `src_prefix` for `dst_prefix`.
fn sync_destination_key(source_key: &str, src_prefix: &str, dst_prefix: &str) -> String {
    join_prefix(dst_prefix, strip_dir_prefix(source_key, src_prefix))
}

/// Glob match supporting `*` and `?`.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star: Option<usize> = None;
    let mut match_ti = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            match_ti = ti;
        } else if let Some(star_idx) = star {
            pi = star_idx + 1;
            match_ti += 1;
            ti = match_ti;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn is_excluded(key: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| wildcard_match(p, key))
}

/// Parses durations like `7d10h30m5s` into seconds.
fn parse_human_duration(input: &str) -> Result<u64, String> {
    if input.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let mut total = 0u64;
    let mut value = 0u64;
    let mut has_unit = false;
    for c in input.chars() {
        if let Some(digit) = c.to_digit(10) {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(digit)))
                .ok_or_else(|| "duration value overflow".to_string())?;
            has_unit = false;
            continue;
        }
        let unit = match c {
            'd' => 86_400u64,
            'h' => 3_600u64,
            'm' => 60u64,
            's' => 1u64,
            _ => return Err(format!("unsupported duration unit: {c}")),
        };
        total = value
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(|| "duration overflow".to_string())?;
        value = 0;
        has_unit = true;
    }
    if !has_unit || value != 0 {
        return Err("duration must end with unit (d/h/m/s)".to_string());
    }
    Ok(total)
}

pub fn parse_sync_args(args: &[String]) -> Result<(SyncOptions, S3Target, S3Target), String> {
    if args.len() < 3 {
        return Err(USAGE.to_string());
    }

    let mut opts = SyncOptions::default();
    let mut positional: Vec<&String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--overwrite" => opts.overwrite = true,
            "--dry-run" => opts.dry_run = true,
            "--remove" => opts.remove = true,
            "--watch" | "-w" => opts.watch = true,
            "--exclude" => {
                opts.excludes
                    .push(flag_value(args, i, "--exclude")?.to_string());
                i += 1;
            }
            "--newer-than" => {
                opts.newer_than = Some(parse_human_duration(flag_value(args, i, "--newer-than")?)?);
                i += 1;
            }
            "--older-than" => {
                opts.older_than = Some(parse_human_duration(flag_value(args, i, "--older-than")?)?);
                i += 1;
            }
            f if f.starts_with('-') => {
                return Err(format!("sync/mirror flag not implemented yet: {f}"));
            }
            _ => positional.push(&args[i]),
        }
        i += 1;
    }

    let [src, dst] = positional.as_slice() else {
        return Err(USAGE.to_string());
    };
    Ok((opts, S3Target::parse(src)?, S3Target::parse(dst)?))
}

#[cfg(test)]
mod tests {
    use super::{
        SyncOptions, is_excluded, parse_human_duration, parse_sync_args, plan,
        sync_destination_key, wildcard_match,
    };
    use crate::cli::args;
    use crate::s3::ObjectInfo;

    const NOW: u64 = 1_000_000;

    fn object(key: &str, size: u64, etag: &str, last_modified: u64) -> ObjectInfo {
        ObjectInfo {
            key: key.to_string(),
            size,
            etag: etag.to_string(),
            last_modified: Some(last_modified),
        }
    }

    fn copied(plan: &super::Plan) -> Vec<(String, String)> {
        plan.copy
            .iter()
            .map(|(o, dest)| (o.key.clone(), dest.clone()))
            .collect()
    }

    #[test]
    fn sync_destination_key_respects_prefixes() {
        assert_eq!(
            sync_destination_key("images/cat.jpg", "images", "backup"),
            "backup/cat.jpg"
        );
        assert_eq!(
            sync_destination_key("images/nested/cat.jpg", "", "archive"),
            "archive/images/nested/cat.jpg"
        );
        assert_eq!(sync_destination_key("a.txt", "", ""), "a.txt");
    }

    #[test]
    fn remove_keeps_objects_skipped_by_filters() {
        let src = vec![
            object("keep.txt", 1, "a", NOW),
            object("app.log", 1, "b", NOW),
            object("old.txt", 1, "c", NOW - 10 * 86_400),
        ];
        let dst = vec![
            object("app.log", 5, "x", NOW),
            object("old.txt", 5, "y", NOW),
            object("local.log", 1, "z", NOW),
            object("stale.txt", 1, "w", NOW),
        ];
        let options = SyncOptions {
            remove: true,
            excludes: vec!["*.log".to_string()],
            newer_than: Some(86_400),
            ..SyncOptions::default()
        };
        let plan = plan(&src, &dst, "", "", &options, NOW);
        assert_eq!(copied(&plan), vec![("keep.txt".into(), "keep.txt".into())]);
        assert_eq!(plan.remove, vec!["stale.txt".to_string()]);
    }

    #[test]
    fn up_to_date_objects_are_not_copied_again() {
        let src = vec![
            object("same-etag", 3, "e1", NOW),
            object("dst-newer", 3, "e2", NOW - 60),
            object("changed", 3, "e3", NOW),
            object("resized", 3, "e4", NOW - 60),
            object("missing", 3, "e5", NOW),
        ];
        let dst = vec![
            object("same-etag", 3, "e1", NOW - 60),
            object("dst-newer", 3, "other", NOW),
            object("changed", 3, "other", NOW - 60),
            object("resized", 4, "other", NOW),
        ];
        let plan = plan(&src, &dst, "", "", &SyncOptions::default(), NOW);
        let keys: Vec<String> = copied(&plan).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["changed", "resized", "missing"]);
    }

    #[test]
    fn prefixes_are_directory_boundaries() {
        let src = vec![
            object("images/a", 1, "a", NOW),
            object("images2/b", 1, "b", NOW),
        ];
        let dst = vec![
            object("backup/old", 1, "o", NOW),
            object("backup2/other", 1, "p", NOW),
        ];
        let options = SyncOptions {
            remove: true,
            ..SyncOptions::default()
        };
        let plan = plan(&src, &dst, "images", "backup", &options, NOW);
        assert_eq!(copied(&plan), vec![("images/a".into(), "backup/a".into())]);
        assert_eq!(plan.remove, vec!["backup/old".to_string()]);
    }

    #[test]
    fn excludes_apply_to_destination_keys_mapped_back_to_source() {
        let dst = vec![object("backup/tmp/x.tmp", 1, "t", NOW)];
        let options = SyncOptions {
            remove: true,
            excludes: vec!["images/tmp/*".to_string()],
            ..SyncOptions::default()
        };
        let plan = plan(&[], &dst, "images", "backup", &options, NOW);
        assert!(plan.remove.is_empty());
    }

    #[test]
    fn wildcard_match_works() {
        assert!(wildcard_match("*.tmp", "a.tmp"));
        assert!(wildcard_match("foo/*/bar", "foo/x/bar"));
        assert!(!wildcard_match("*.tmp", "a.txt"));
    }

    #[test]
    fn parse_sync_args_with_flags() {
        let (opts, src, dst) = parse_sync_args(&args(&[
            "mirror",
            "--dry-run",
            "--remove",
            "-w",
            "--exclude",
            "*.tmp",
            "a/src/prefix",
            "b/dst/prefix",
        ]))
        .expect("sync args should parse");
        assert!(opts.dry_run);
        assert!(opts.remove);
        assert!(opts.watch);
        assert_eq!(opts.excludes, vec!["*.tmp".to_string()]);
        assert_eq!(opts.newer_than, None);
        assert_eq!(opts.older_than, None);
        assert_eq!(src.alias, "a");
        assert_eq!(dst.alias, "b");
        assert!(is_excluded("x.tmp", &opts.excludes));
    }

    #[test]
    fn parse_human_duration_works() {
        assert_eq!(parse_human_duration("10d").expect("duration"), 864000);
        assert_eq!(
            parse_human_duration("7d10h30m5s").expect("duration"),
            642605
        );
        assert!(parse_human_duration("10").is_err());
    }

    #[test]
    fn parse_sync_args_with_time_filters() {
        let (opts, _, _) = parse_sync_args(&args(&[
            "sync",
            "--newer-than",
            "10d",
            "--older-than",
            "1h",
            "a/src",
            "b/dst",
        ]))
        .expect("sync args should parse");
        assert!(!opts.watch);
        assert_eq!(opts.newer_than, Some(864000));
        assert_eq!(opts.older_than, Some(3600));
    }
}
