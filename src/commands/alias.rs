//! `alias set|ls|rm`: manage endpoints in the local config.

use super::Context;
use crate::cli::flag_value;
use crate::config::{AliasConfig, save_config};
use crate::output::JsonObject;

const USAGE: &str = "usage: s4 alias <set|ls|rm> ...";

/// `args` starts at the subcommand (`set`, `ls`, `rm`).
pub fn run(ctx: &mut Context, args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("set") => set(ctx, args),
        Some("ls") => {
            list(ctx);
            Ok(())
        }
        Some("rm") => remove(ctx, args),
        _ => Err(USAGE.to_string()),
    }
}

fn set(ctx: &mut Context, args: &[String]) -> Result<(), String> {
    if args.len() < 5 {
        return Err(
            "usage: s4 alias set <name> <endpoint> <access> <secret> [--region r] [--path-style]"
                .to_string(),
        );
    }
    let mut region = "us-east-1".to_string();
    let mut path_style = false;
    let mut i = 5;
    while i < args.len() {
        match args[i].as_str() {
            "--region" => {
                region = flag_value(args, i, "--region")?.to_string();
                i += 2;
            }
            "--path-style" => {
                path_style = true;
                i += 1;
            }
            other => return Err(format!("unknown alias set flag: {other}")),
        }
    }

    let name = &args[1];
    ctx.config.aliases.insert(
        name.clone(),
        AliasConfig {
            endpoint: args[2].clone(),
            access_key: args[3].clone(),
            secret_key: args[4].clone(),
            region,
            path_style,
        },
    );
    save_config(&ctx.config_path, &ctx.config)?;
    ctx.report(
        JsonObject::new().str("status", "ok").str("alias", name),
        format!("Alias '{name}' saved"),
    );
    Ok(())
}

fn list(ctx: &Context) {
    if ctx.json {
        let items: Vec<String> = ctx
            .config
            .aliases
            .iter()
            .map(|(name, alias)| {
                JsonObject::new()
                    .str("name", name)
                    .str("endpoint", &alias.endpoint)
                    .str("region", &alias.region)
                    .raw("path_style", alias.path_style)
                    .to_string()
            })
            .collect();
        println!("[{}]", items.join(","));
    } else {
        for (name, alias) in &ctx.config.aliases {
            println!(
                "{name}\t{}\t{}\tpath_style={}",
                alias.endpoint, alias.region, alias.path_style
            );
        }
    }
}

fn remove(ctx: &mut Context, args: &[String]) -> Result<(), String> {
    let name = args.get(1).ok_or("usage: s4 alias rm <name>")?;
    let existed = ctx.config.aliases.remove(name).is_some();
    save_config(&ctx.config_path, &ctx.config)?;
    let text = if existed {
        format!("Alias '{name}' removed")
    } else {
        format!("Alias '{name}' not found")
    };
    ctx.report(
        JsonObject::new()
            .str("status", "ok")
            .str("alias", name)
            .raw("removed", existed),
        text,
    );
    Ok(())
}
