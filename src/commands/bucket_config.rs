//! Bucket configuration subresources: `cors`, `encrypt`, `event`.

use std::path::{Path, PathBuf};

use super::Context;
use crate::output::JsonObject;
use crate::s3::Request;
use crate::target::S3Target;

/// One bucket configuration subresource and the wording used in its output.
struct BucketConfig {
    command: &'static str,
    subresource: &'static str,
    label: &'static str,
}

const CORS: BucketConfig = BucketConfig {
    command: "cors",
    subresource: "cors",
    label: "CORS",
};

const ENCRYPTION: BucketConfig = BucketConfig {
    command: "encrypt",
    subresource: "encryption",
    label: "Encryption",
};

const NOTIFICATION: BucketConfig = BucketConfig {
    command: "event",
    subresource: "notification",
    label: "Notification config",
};

#[derive(Debug)]
pub enum CorsCommand {
    Set { target: S3Target, file: PathBuf },
    Get { target: S3Target },
    Remove { target: S3Target },
}

#[derive(Debug)]
pub enum EncryptCommand {
    Set { target: S3Target, file: PathBuf },
    Clear { target: S3Target },
    Info { target: S3Target },
}

#[derive(Debug)]
pub enum EventCommand {
    Add { target: S3Target, file: PathBuf },
    Remove { target: S3Target, force: bool },
    List { target: S3Target },
}

pub fn cors(ctx: &Context, args: &[String]) -> Result<(), String> {
    match parse_cors_args(args)? {
        CorsCommand::Set { target, file } => put_config(ctx, &CORS, "set", &target, &file),
        CorsCommand::Get { target } => show_config(ctx, &CORS, "get", &target),
        CorsCommand::Remove { target } => delete_config(ctx, &CORS, "remove", "removed", &target),
    }
}

pub fn encrypt(ctx: &Context, args: &[String]) -> Result<(), String> {
    match parse_encrypt_args(args)? {
        EncryptCommand::Set { target, file } => put_config(ctx, &ENCRYPTION, "set", &target, &file),
        EncryptCommand::Clear { target } => {
            delete_config(ctx, &ENCRYPTION, "clear", "cleared", &target)
        }
        EncryptCommand::Info { target } => show_config(ctx, &ENCRYPTION, "info", &target),
    }
}

pub fn event(ctx: &Context, args: &[String]) -> Result<(), String> {
    match parse_event_args(args)? {
        EventCommand::Add { target, file } => put_config(ctx, &NOTIFICATION, "add", &target, &file),
        EventCommand::Remove { target, force } => {
            let client = ctx.client_for(&target)?;
            let bucket = target.require_bucket("event remove")?;
            client.put_subresource_xml(
                bucket,
                None,
                NOTIFICATION.subresource,
                "<NotificationConfiguration></NotificationConfiguration>",
                false,
            )?;
            ctx.report(
                JsonObject::new()
                    .str("status", "ok")
                    .str("command", "event remove")
                    .str("bucket", bucket)
                    .raw("force", force),
                format!("Notification config removed for bucket '{bucket}' (force: {force})"),
            );
            Ok(())
        }
        EventCommand::List { target } => show_config(ctx, &NOTIFICATION, "list", &target),
    }
}

fn put_config(
    ctx: &Context,
    cfg: &BucketConfig,
    verb: &str,
    target: &S3Target,
    file: &Path,
) -> Result<(), String> {
    if !file.exists() {
        return Err(format!(
            "{} file not found: {}",
            cfg.subresource,
            file.display()
        ));
    }
    let client = ctx.client_for(target)?;
    let command = format!("{} {verb}", cfg.command);
    let bucket = target.require_bucket(&command)?;
    client.put_subresource(bucket, None, cfg.subresource, file, false)?;
    ctx.report(
        JsonObject::new()
            .str("status", "ok")
            .str("command", &command)
            .str("bucket", bucket),
        format!("{} set for bucket '{bucket}'", cfg.label),
    );
    Ok(())
}

fn show_config(
    ctx: &Context,
    cfg: &BucketConfig,
    verb: &str,
    target: &S3Target,
) -> Result<(), String> {
    let client = ctx.client_for(target)?;
    let bucket = target.require_bucket(&format!("{} {verb}", cfg.command))?;
    let body = client.send(&Request::new("GET", bucket).subresource(cfg.subresource))?;
    if ctx.json {
        println!(
            "{}",
            JsonObject::new()
                .str("bucket", bucket)
                .str(cfg.subresource, &body)
        );
    } else {
        print!("{body}");
    }
    Ok(())
}

fn delete_config(
    ctx: &Context,
    cfg: &BucketConfig,
    verb: &str,
    past: &str,
    target: &S3Target,
) -> Result<(), String> {
    let client = ctx.client_for(target)?;
    let command = format!("{} {verb}", cfg.command);
    let bucket = target.require_bucket(&command)?;
    client.send(&Request::new("DELETE", bucket).subresource(cfg.subresource))?;
    ctx.report(
        JsonObject::new()
            .str("status", "ok")
            .str("command", &command)
            .str("bucket", bucket),
        format!("{} {past} for bucket '{bucket}'", cfg.label),
    );
    Ok(())
}

pub fn parse_cors_args(args: &[String]) -> Result<CorsCommand, String> {
    const USAGE: &str = "usage: s4 cors <set|get|remove> ...";
    if args.len() < 3 {
        return Err(USAGE.to_string());
    }
    match args[1].as_str() {
        "set" => {
            let file = args
                .get(3)
                .ok_or("usage: s4 cors set <alias/bucket> <cors_xml_file>")?;
            Ok(CorsCommand::Set {
                target: S3Target::parse(&args[2])?,
                file: PathBuf::from(file),
            })
        }
        "get" => Ok(CorsCommand::Get {
            target: S3Target::parse(&args[2])?,
        }),
        "remove" => Ok(CorsCommand::Remove {
            target: S3Target::parse(&args[2])?,
        }),
        "help" | "h" => Err(USAGE.to_string()),
        other => Err(format!("unknown cors subcommand: {other}")),
    }
}

pub fn parse_encrypt_args(args: &[String]) -> Result<EncryptCommand, String> {
    const USAGE: &str = "usage: s4 encrypt <set|clear|info> ...";
    if args.len() < 3 {
        return Err(USAGE.to_string());
    }
    match args[1].as_str() {
        "set" => {
            let file = args
                .get(3)
                .ok_or("usage: s4 encrypt set <alias/bucket> <encryption_xml_file>")?;
            Ok(EncryptCommand::Set {
                target: S3Target::parse(&args[2])?,
                file: PathBuf::from(file),
            })
        }
        "clear" => Ok(EncryptCommand::Clear {
            target: S3Target::parse(&args[2])?,
        }),
        "info" => Ok(EncryptCommand::Info {
            target: S3Target::parse(&args[2])?,
        }),
        "help" | "h" => Err(USAGE.to_string()),
        other => Err(format!("unknown encrypt subcommand: {other}")),
    }
}

pub fn parse_event_args(args: &[String]) -> Result<EventCommand, String> {
    const USAGE: &str = "usage: s4 event <add|remove|rm|list|ls> ...";
    if args.len() < 3 {
        return Err(USAGE.to_string());
    }
    match args[1].as_str() {
        "add" => {
            let file = args
                .get(3)
                .ok_or("usage: s4 event add <alias/bucket> <notification_xml_file>")?;
            Ok(EventCommand::Add {
                target: S3Target::parse(&args[2])?,
                file: PathBuf::from(file),
            })
        }
        "remove" | "rm" => Ok(EventCommand::Remove {
            target: S3Target::parse(&args[2])?,
            force: args.iter().any(|a| a == "--force"),
        }),
        "list" | "ls" => Ok(EventCommand::List {
            target: S3Target::parse(&args[2])?,
        }),
        "help" | "h" => Err(USAGE.to_string()),
        other => Err(format!("unknown event subcommand: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CorsCommand, EncryptCommand, EventCommand, parse_cors_args, parse_encrypt_args,
        parse_event_args,
    };
    use crate::cli::args;

    #[test]
    fn parse_cors_args_set_works() {
        let parsed = parse_cors_args(&args(&["cors", "set", "a/bucket", "cors.xml"]))
            .expect("cors args should parse");
        match parsed {
            CorsCommand::Set { target, file } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
                assert_eq!(file.to_string_lossy(), "cors.xml");
            }
            _ => panic!("expected cors set"),
        }
    }

    #[test]
    fn parse_cors_args_get_works() {
        let parsed =
            parse_cors_args(&args(&["cors", "get", "a/bucket"])).expect("cors args should parse");
        match parsed {
            CorsCommand::Get { target } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
            }
            _ => panic!("expected cors get"),
        }
    }

    #[test]
    fn parse_encrypt_args_set_works() {
        let parsed = parse_encrypt_args(&args(&["encrypt", "set", "a/bucket", "enc.xml"]))
            .expect("encrypt args should parse");
        match parsed {
            EncryptCommand::Set { target, file } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
                assert_eq!(file.to_string_lossy(), "enc.xml");
            }
            _ => panic!("expected encrypt set"),
        }
    }

    #[test]
    fn parse_encrypt_args_info_works() {
        let parsed = parse_encrypt_args(&args(&["encrypt", "info", "a/bucket"]))
            .expect("encrypt args should parse");
        match parsed {
            EncryptCommand::Info { target } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
            }
            _ => panic!("expected encrypt info"),
        }
    }

    #[test]
    fn parse_event_args_add_works() {
        let parsed = parse_event_args(&args(&["event", "add", "a/bucket", "event.xml"]))
            .expect("event args should parse");
        match parsed {
            EventCommand::Add { target, file } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
                assert_eq!(file.to_string_lossy(), "event.xml");
            }
            _ => panic!("expected event add"),
        }
    }

    #[test]
    fn parse_event_args_remove_force_works() {
        let parsed = parse_event_args(&args(&["event", "rm", "a/bucket", "--force"]))
            .expect("event args should parse");
        match parsed {
            EventCommand::Remove { target, force } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
                assert!(force);
            }
            _ => panic!("expected event remove"),
        }
    }
}
