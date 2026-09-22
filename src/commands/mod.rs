//! Command dispatch and the execution context shared by all commands.
//!
//! Every command handler receives the full argument list with the command
//! name at `args[0]` (except `alias`, which gets its subcommand first).

mod alias;
mod bucket;
mod bucket_config;
mod health;
mod object;
mod object_lock;
mod sql;
mod stubs;
mod sync;
mod transfer;

use std::fmt::Display;
use std::path::PathBuf;

use crate::config::{AliasConfig, AppConfig};
use crate::output::JsonObject;
use crate::s3::{HttpOptions, S3Client};
use crate::target::S3Target;

pub struct Context {
    pub config: AppConfig,
    pub config_path: PathBuf,
    pub json: bool,
    pub debug: bool,
    pub http: HttpOptions,
}

impl Context {
    pub fn client<'a>(&'a self, alias: &'a AliasConfig) -> S3Client<'a> {
        S3Client::new(alias, &self.http, self.debug)
    }

    /// Resolves the target's alias and returns a client for it.
    pub fn client_for(&self, target: &S3Target) -> Result<S3Client<'_>, String> {
        Ok(self.client(self.config.alias(&target.alias)?))
    }

    /// Prints `json` in `--json` mode and `text` otherwise.
    pub fn report(&self, json: JsonObject, text: impl Display) {
        if self.json {
            println!("{json}");
        } else {
            println!("{text}");
        }
    }
}

pub fn dispatch(ctx: &mut Context, args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        "alias" => alias::run(ctx, &args[1..]),
        "ls" => bucket::ls(ctx, args),
        "mb" => bucket::mb(ctx, args),
        "rb" => bucket::rb(ctx, args),
        "find" => bucket::find(ctx, args),
        "tree" => bucket::tree(ctx, args),
        "put" => object::put(ctx, args),
        "get" => object::get(ctx, args),
        "rm" => object::rm(ctx, args),
        "stat" => object::stat(ctx, args),
        "cat" => object::cat(ctx, args),
        "head" => object::head(ctx, args),
        "pipe" => object::pipe(ctx, args),
        "cp" | "mv" => transfer::run(ctx, args),
        "sync" | "mirror" => sync::run(ctx, args),
        "ping" => health::ping(ctx, args),
        "ready" => health::ready(ctx, args),
        "cors" => bucket_config::cors(ctx, args),
        "encrypt" => bucket_config::encrypt(ctx, args),
        "event" => bucket_config::event(ctx, args),
        "legalhold" => object_lock::legalhold(ctx, args),
        "retention" => object_lock::retention(ctx, args),
        "sql" => sql::run(ctx, args),
        "idp" => stubs::idp(ctx, args),
        "ilm" => stubs::ilm(ctx, args),
        "replicate" => stubs::replicate(ctx, args),
        other => Err(format!("unknown command: {other}")),
    }
}

/// Parses `args[idx]` as a target and resolves its alias; `usage` if it is missing.
fn target_arg<'a>(
    ctx: &'a Context,
    args: &[String],
    idx: usize,
    usage: &str,
) -> Result<(S3Target, S3Client<'a>), String> {
    let raw = args.get(idx).ok_or_else(|| usage.to_string())?;
    resolve_target(ctx, raw)
}

fn resolve_target<'a>(ctx: &'a Context, raw: &str) -> Result<(S3Target, S3Client<'a>), String> {
    let target = S3Target::parse(raw)?;
    let client = ctx.client_for(&target)?;
    Ok((target, client))
}

/// Splits `args[1..]` into boolean flags (from `known`) and positional arguments.
fn split_flags<'a>(
    args: &'a [String],
    known: &[&str],
) -> Result<(Vec<&'a str>, Vec<&'a str>), String> {
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    for arg in &args[1..] {
        let arg = arg.as_str();
        if known.contains(&arg) {
            flags.push(arg);
        } else if arg.starts_with('-') {
            return Err(format!("unknown {} flag: {arg}", args[0]));
        } else {
            positional.push(arg);
        }
    }
    Ok((flags, positional))
}

fn generic_usage(args: &[String]) -> String {
    format!("usage: s4 {} ...", args[0])
}
