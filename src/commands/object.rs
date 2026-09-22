//! Single-object commands: `put`, `get`, `rm`, `stat`, `cat`, `head`, `pipe`.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

use super::{Context, generic_usage, resolve_target, split_flags, target_arg};
use crate::output::JsonObject;
use crate::s3::Request;
use crate::util::ensure_parent_dir;

pub fn put(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 2, &generic_usage(args))?;
    let source = Path::new(&args[1]);
    if !source.exists() {
        return Err(format!("source file not found: {}", source.display()));
    }
    let bucket = target.require_bucket("put")?;
    let key = target.require_key("put")?;
    client.upload_file(bucket, key, source)?;
    ctx.report(
        JsonObject::new().object(
            "uploaded",
            JsonObject::new().str("bucket", bucket).str("key", key),
        ),
        format!("Uploaded '{}' to '{bucket}/{key}'", source.display()),
    );
    Ok(())
}

pub fn get(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, &generic_usage(args))?;
    if args.len() < 3 {
        return Err("usage: s4 get <alias/bucket/key> <destination_file>".to_string());
    }
    let bucket = target.require_bucket("get")?;
    let key = target.require_key("get")?;
    let destination = Path::new(&args[2]);
    ensure_parent_dir(destination)?;
    client.download(&Request::new("GET", bucket).key(key), destination)?;
    ctx.report(
        JsonObject::new().object(
            "downloaded",
            JsonObject::new()
                .str("bucket", bucket)
                .str("key", key)
                .str("to", &destination.display().to_string()),
        ),
        format!("Downloaded '{bucket}/{key}' to '{}'", destination.display()),
    );
    Ok(())
}

pub fn rm(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (flags, positional) = split_flags(args, &["--bypass"])?;
    let raw = positional
        .first()
        .ok_or("usage: s4 rm [--bypass] <alias/bucket/key>")?;
    let (target, client) = resolve_target(ctx, raw)?;
    let bucket = target.require_bucket("rm")?;
    let key = target.require_key("rm")?;
    client.delete_object(bucket, key, None, flags.contains(&"--bypass"))?;
    ctx.report(
        JsonObject::new().object(
            "deleted",
            JsonObject::new().str("bucket", bucket).str("key", key),
        ),
        format!("Deleted '{bucket}/{key}'"),
    );
    Ok(())
}

pub fn stat(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, &generic_usage(args))?;
    let bucket = target.require_bucket("stat")?;
    let key = target.require_key("stat")?;
    let headers = client.send(&Request::new("HEAD", bucket).key(key))?;
    ctx.report(
        JsonObject::new()
            .str("bucket", bucket)
            .str("key", key)
            .str("headers", &headers),
        &headers,
    );
    Ok(())
}

pub fn cat(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, &generic_usage(args))?;
    let bucket = target.require_bucket("cat")?;
    let key = target.require_key("cat")?;
    let mut body = client.open(&Request::new("GET", bucket).key(key))?;
    let mut stdout = io::stdout().lock();
    io::copy(&mut body, &mut stdout).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

pub fn head(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, "usage: s4 head <alias/bucket/key> [lines]")?;
    let bucket = target.require_bucket("head")?;
    let key = target.require_key("head")?;
    let lines = match args.get(2) {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| "head lines must be integer".to_string())?,
        None => 10,
    };
    // Stream only as much of the object as the requested lines need.
    let mut body = BufReader::new(client.open(&Request::new("GET", bucket).key(key))?);
    let mut stdout = io::stdout().lock();
    let mut line = Vec::new();
    for _ in 0..lines {
        line.clear();
        if body
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        stdout.write_all(&line).map_err(|e| e.to_string())?;
    }
    stdout.flush().map_err(|e| e.to_string())
}

pub fn pipe(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (target, client) = target_arg(ctx, args, 1, "usage: s4 pipe <alias/bucket/key>")?;
    let bucket = target.require_bucket("pipe")?;
    let key = target.require_key("pipe")?;
    client.upload_reader(bucket, key, io::stdin().lock())?;
    ctx.report(
        JsonObject::new().object(
            "uploaded",
            JsonObject::new()
                .str("bucket", bucket)
                .str("key", key)
                .str("source", "stdin"),
        ),
        format!("Uploaded STDIN to '{bucket}/{key}'"),
    );
    Ok(())
}
