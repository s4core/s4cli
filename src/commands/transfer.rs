//! `cp` and `mv` between local paths and S3 objects, in any combination.

use std::fs;
use std::path::Path;

use super::Context;
use crate::config::AliasConfig;
use crate::output::JsonObject;
use crate::s3::{Request, S3Client};
use crate::target::S3Target;
use crate::util::{TempPath, ensure_parent_dir};

struct S3Object<'a> {
    alias: &'a AliasConfig,
    bucket: String,
    key: String,
}

enum Location<'a> {
    S3(S3Object<'a>),
    Local(&'a str),
}

/// `alias/bucket/key` with a configured alias is S3; anything else is a local path.
fn classify<'a>(ctx: &'a Context, value: &'a str) -> Location<'a> {
    if let Ok(S3Target {
        alias,
        bucket: Some(bucket),
        key: Some(key),
    }) = S3Target::parse(value)
        && let Some(alias) = ctx.config.aliases.get(&alias)
    {
        return Location::S3(S3Object { alias, bucket, key });
    }
    Location::Local(value)
}

pub fn run(ctx: &Context, args: &[String]) -> Result<(), String> {
    let command = args[0].as_str();
    if args.len() < 3 {
        return Err(format!("usage: s4 {command} <source> <target>"));
    }
    let (source, target) = (args[1].as_str(), args[2].as_str());
    let is_move = command == "mv";

    match (classify(ctx, source), classify(ctx, target)) {
        (Location::Local(src), Location::S3(dst)) => {
            let src = Path::new(src);
            if !src.exists() {
                return Err(format!("source file not found: {}", src.display()));
            }
            ctx.client(dst.alias)
                .upload_file(&dst.bucket, &dst.key, src)?;
            if is_move {
                fs::remove_file(src).map_err(|e| e.to_string())?;
            }
        }
        (Location::S3(src), Location::Local(dst)) => {
            let dst = Path::new(dst);
            ensure_parent_dir(dst)?;
            let client = ctx.client(src.alias);
            client.download(&Request::new("GET", &src.bucket).key(&src.key), dst)?;
            if is_move {
                client.delete_object(&src.bucket, &src.key, None, false)?;
            }
        }
        (Location::S3(src), Location::S3(dst)) => {
            let src_client = ctx.client(src.alias);
            let dst_client = ctx.client(dst.alias);
            copy_between(&src_client, &src, &dst_client, &dst)?;
            if is_move {
                src_client.delete_object(&src.bucket, &src.key, None, false)?;
            }
        }
        (Location::Local(src), Location::Local(dst)) => {
            fs::copy(src, dst).map_err(|e| e.to_string())?;
            if is_move {
                fs::remove_file(src).map_err(|e| e.to_string())?;
            }
        }
    }

    ctx.report(
        JsonObject::new()
            .str("status", "ok")
            .str("command", command)
            .str("source", source)
            .str("target", target),
        format!("{command}: {source} -> {target}"),
    );
    Ok(())
}

/// Server-side copy when both aliases are the same account, otherwise a
/// download to a temp file followed by an upload.
fn copy_between(
    src_client: &S3Client,
    src: &S3Object,
    dst_client: &S3Client,
    dst: &S3Object,
) -> Result<(), String> {
    if src.alias.same_account(dst.alias) {
        return dst_client.copy_object(&src.bucket, &src.key, &dst.bucket, &dst.key);
    }
    let temp = TempPath::new("cp")?;
    src_client.download(&Request::new("GET", &src.bucket).key(&src.key), temp.path())?;
    dst_client.upload_file(&dst.bucket, &dst.key, temp.path())
}
