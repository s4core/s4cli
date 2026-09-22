//! Bucket-level commands: `ls`, `mb`, `rb`, `find`, `tree`.

use super::{Context, generic_usage, resolve_target, split_flags, target_arg};
use crate::output::{JsonObject, print_status};
use crate::s3::Request;

pub fn ls(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, &generic_usage(args))?;
    let body = match &target.bucket {
        None => client.send(&Request::new("GET", ""))?,
        Some(bucket) => client.send(&Request::new("GET", bucket).param("list-type", "2"))?,
    };
    if ctx.json {
        println!("{}", JsonObject::new().str("xml", &body));
    } else {
        println!("{body}");
    }
    Ok(())
}

pub fn mb(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (flags, positional) = split_flags(args, &["--with-lock"])?;
    let raw = positional
        .last()
        .ok_or("usage: s4 mb [--with-lock] <alias/bucket>")?;
    let (target, client) = resolve_target(ctx, raw)?;
    let bucket = target.require_bucket("mb")?;
    let mut req = Request::new("PUT", bucket);
    if flags.contains(&"--with-lock") {
        req = req.header("x-amz-bucket-object-lock-enabled", "true");
    }
    client.send(&req)?;
    print_status(ctx.json, "created", bucket);
    Ok(())
}

pub fn rb(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (flags, positional) = split_flags(args, &["--force", "--bypass"])?;
    let raw = positional
        .first()
        .ok_or("usage: s4 rb [--force [--bypass]] <alias/bucket>")?;
    let (target, client) = resolve_target(ctx, raw)?;
    let bucket = target.require_bucket("rb")?;
    let delete = Request::new("DELETE", bucket);
    if let Err(err) = client.send(&delete) {
        if !err.contains("BucketNotEmpty") {
            return Err(err);
        }
        if !flags.contains(&"--force") {
            return Err(format!(
                "bucket '{bucket}' is not empty; use --force to delete it with all objects and versions"
            ));
        }
        client.purge_bucket_versions(bucket, flags.contains(&"--bypass"))?;
        client.send(&delete)?;
    }
    print_status(ctx.json, "deleted", bucket);
    Ok(())
}

pub fn find(ctx: &Context, args: &[String]) -> Result<(), String> {
    let usage = "usage: s4 find <alias/bucket[/prefix]> [needle]";
    let (target, client) = target_arg(ctx, args, 1, usage)?;
    let bucket = target.require_bucket("find")?;
    let needle = args.get(2);
    for key in client.list_object_keys(bucket, target.prefix())? {
        if needle.is_some_and(|n| !key.contains(n.as_str())) {
            continue;
        }
        if ctx.json {
            println!(
                "{}",
                JsonObject::new().str("bucket", bucket).str("key", &key)
            );
        } else {
            println!("{key}");
        }
    }
    Ok(())
}

pub fn tree(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, "usage: s4 tree <alias/bucket[/prefix]>")?;
    let bucket = target.require_bucket("tree")?;
    let mut keys = client.list_object_keys(bucket, target.prefix())?;
    keys.sort();
    println!("{bucket}/");
    for key in keys {
        let depth = key.matches('/').count();
        let indent = "  ".repeat(depth + 1);
        let name = key.rsplit('/').next().unwrap_or(&key);
        println!("{indent}{name}");
    }
    Ok(())
}
