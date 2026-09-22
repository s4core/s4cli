//! Object lock commands: `legalhold`, `retention`.

use super::Context;
use crate::cli::flag_value;
use crate::output::JsonObject;
use crate::s3::{Request, xml};
use crate::target::S3Target;
use crate::time;

#[derive(Debug)]
pub enum LegalHoldCommand {
    Set { target: S3Target },
    Clear { target: S3Target },
    Info { target: S3Target },
}

#[derive(Debug)]
pub enum RetentionCommand {
    Set {
        target: S3Target,
        mode: String,
        retain_until: String,
        bypass: bool,
    },
    Clear {
        target: S3Target,
    },
    Info {
        target: S3Target,
    },
}

pub fn legalhold(ctx: &Context, args: &[String]) -> Result<(), String> {
    match parse_legalhold_args(args)? {
        LegalHoldCommand::Set { target } => set_legal_hold(ctx, &target, "set", "ON", "set"),
        LegalHoldCommand::Clear { target } => {
            set_legal_hold(ctx, &target, "clear", "OFF", "cleared")
        }
        LegalHoldCommand::Info { target } => {
            show_object_config(ctx, &target, "legalhold info", "legal-hold", "legalhold")
        }
    }
}

pub fn retention(ctx: &Context, args: &[String]) -> Result<(), String> {
    match parse_retention_args(args)? {
        RetentionCommand::Set {
            target,
            mode,
            retain_until,
            bypass,
        } => {
            let client = ctx.client_for(&target)?;
            let bucket = target.require_bucket("retention set")?;
            let key = target.require_key("retention set")?;
            let body = format!(
                "<Retention><Mode>{}</Mode><RetainUntilDate>{}</RetainUntilDate></Retention>",
                xml::escape(&mode),
                xml::escape(&retain_until)
            );
            client.put_subresource_xml(bucket, Some(key), "retention", &body, bypass)?;
            ctx.report(
                JsonObject::new()
                    .str("status", "ok")
                    .str("command", "retention set")
                    .str("bucket", bucket)
                    .str("key", key)
                    .str("mode", &mode)
                    .str("retain_until", &retain_until),
                format!(
                    "Retention set for '{bucket}/{key}' mode={mode} retain-until={retain_until}"
                ),
            );
            Ok(())
        }
        RetentionCommand::Clear { target } => {
            let client = ctx.client_for(&target)?;
            let bucket = target.require_bucket("retention clear")?;
            let key = target.require_key("retention clear")?;

            // An empty retention document with governance bypass removes the
            // retention (as `mc retention clear` does); COMPLIANCE cannot be cleared.
            client.put_subresource_xml(
                bucket,
                Some(key),
                "retention",
                "<Retention></Retention>",
                true,
            )?;
            ctx.report(
                JsonObject::new()
                    .str("status", "ok")
                    .str("command", "retention clear")
                    .str("bucket", bucket)
                    .str("key", key),
                format!("Retention cleared for '{bucket}/{key}'"),
            );
            Ok(())
        }
        RetentionCommand::Info { target } => {
            show_object_config(ctx, &target, "retention info", "retention", "retention")
        }
    }
}

fn set_legal_hold(
    ctx: &Context,
    target: &S3Target,
    verb: &str,
    status: &str,
    past: &str,
) -> Result<(), String> {
    let client = ctx.client_for(target)?;
    let command = format!("legalhold {verb}");
    let bucket = target.require_bucket(&command)?;
    let key = target.require_key(&command)?;
    let body = format!("<LegalHold><Status>{status}</Status></LegalHold>");
    client.put_subresource_xml(bucket, Some(key), "legal-hold", &body, false)?;
    ctx.report(
        JsonObject::new()
            .str("status", "ok")
            .str("command", &command)
            .str("bucket", bucket)
            .str("key", key),
        format!("Legal hold {past} for '{bucket}/{key}'"),
    );
    Ok(())
}

/// Prints an object subresource document (`?legal-hold`, `?retention`).
fn show_object_config(
    ctx: &Context,
    target: &S3Target,
    command: &str,
    subresource: &str,
    json_field: &str,
) -> Result<(), String> {
    let client = ctx.client_for(target)?;
    let bucket = target.require_bucket(command)?;
    let key = target.require_key(command)?;
    let body = client.send(
        &Request::new("GET", bucket)
            .key(key)
            .subresource(subresource),
    )?;
    if ctx.json {
        println!(
            "{}",
            JsonObject::new()
                .str("bucket", bucket)
                .str("key", key)
                .str(json_field, &body)
        );
    } else {
        print!("{body}");
    }
    Ok(())
}

pub fn parse_legalhold_args(args: &[String]) -> Result<LegalHoldCommand, String> {
    const USAGE: &str = "usage: s4 legalhold <set|clear|info> <alias/bucket/key>";
    if args.len() < 3 {
        return Err(USAGE.to_string());
    }
    match args[1].as_str() {
        "set" => Ok(LegalHoldCommand::Set {
            target: S3Target::parse(&args[2])?,
        }),
        "clear" => Ok(LegalHoldCommand::Clear {
            target: S3Target::parse(&args[2])?,
        }),
        "info" => Ok(LegalHoldCommand::Info {
            target: S3Target::parse(&args[2])?,
        }),
        "help" | "h" => Err(USAGE.to_string()),
        other => Err(format!("unknown legalhold subcommand: {other}")),
    }
}

pub fn parse_retention_args(args: &[String]) -> Result<RetentionCommand, String> {
    const USAGE: &str = "usage: s4 retention <set|clear|info> ...";
    if args.len() < 3 {
        return Err(USAGE.to_string());
    }
    match args[1].as_str() {
        "set" => {
            if args.len() < 4 {
                return Err("usage: s4 retention set <alias/bucket/key> --mode <GOVERNANCE|COMPLIANCE> --retain-until <RFC3339> [--bypass]".to_string());
            }
            let target = S3Target::parse(&args[2])?;
            let mut mode = None;
            let mut retain_until = None;
            let mut bypass = false;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--bypass" => {
                        bypass = true;
                        i += 1;
                    }
                    "--mode" => {
                        mode = Some(flag_value(args, i, "--mode")?.to_string());
                        i += 2;
                    }
                    "--retain-until" => {
                        retain_until = Some(flag_value(args, i, "--retain-until")?.to_string());
                        i += 2;
                    }
                    f if f.starts_with('-') => {
                        return Err(format!("unknown retention set flag: {f}"));
                    }
                    other => return Err(format!("unexpected retention set argument: {other}")),
                }
            }
            let mode = mode
                .ok_or("retention set requires --mode")?
                .to_ascii_uppercase();
            if mode != "GOVERNANCE" && mode != "COMPLIANCE" {
                return Err(format!(
                    "retention mode must be GOVERNANCE or COMPLIANCE, got {mode}"
                ));
            }
            let retain_until = retain_until.ok_or("retention set requires --retain-until")?;
            if time::parse_iso8601(&retain_until).is_none() {
                return Err(format!(
                    "--retain-until must be a UTC timestamp like 2030-01-01T00:00:00Z, got {retain_until}"
                ));
            }
            Ok(RetentionCommand::Set {
                target,
                mode,
                retain_until,
                bypass,
            })
        }
        "clear" => Ok(RetentionCommand::Clear {
            target: S3Target::parse(&args[2])?,
        }),
        "info" => Ok(RetentionCommand::Info {
            target: S3Target::parse(&args[2])?,
        }),
        "help" | "h" => Err(USAGE.to_string()),
        other => Err(format!("unknown retention subcommand: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{LegalHoldCommand, RetentionCommand, parse_legalhold_args, parse_retention_args};
    use crate::cli::args;

    #[test]
    fn parse_legalhold_args_set_works() {
        let parsed = parse_legalhold_args(&args(&["legalhold", "set", "a/b/k"]))
            .expect("legalhold args should parse");
        match parsed {
            LegalHoldCommand::Set { target } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("b"));
                assert_eq!(target.key.as_deref(), Some("k"));
            }
            _ => panic!("expected legalhold set"),
        }
    }

    #[test]
    fn parse_legalhold_args_info_works() {
        let parsed = parse_legalhold_args(&args(&["legalhold", "info", "a/b/k"]))
            .expect("legalhold args should parse");
        match parsed {
            LegalHoldCommand::Info { target } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("b"));
                assert_eq!(target.key.as_deref(), Some("k"));
            }
            _ => panic!("expected legalhold info"),
        }
    }

    #[test]
    fn parse_retention_args_set_works() {
        let parsed = parse_retention_args(&args(&[
            "retention",
            "set",
            "a/b/k",
            "--mode",
            "GOVERNANCE",
            "--retain-until",
            "2030-01-01T00:00:00Z",
        ]))
        .expect("retention args should parse");
        match parsed {
            RetentionCommand::Set {
                target,
                mode,
                retain_until,
                bypass,
            } => {
                assert!(!bypass);
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("b"));
                assert_eq!(target.key.as_deref(), Some("k"));
                assert_eq!(mode, "GOVERNANCE");
                assert_eq!(retain_until, "2030-01-01T00:00:00Z");
            }
            _ => panic!("expected retention set"),
        }
    }

    #[test]
    fn parse_retention_args_set_validates_mode_and_date() {
        let parsed = parse_retention_args(&args(&[
            "retention",
            "set",
            "a/b/k",
            "--mode",
            "governance",
            "--retain-until",
            "2030-01-01T00:00:00Z",
            "--bypass",
        ]))
        .expect("retention args should parse");
        assert!(matches!(
            parsed,
            RetentionCommand::Set { ref mode, bypass: true, .. } if mode == "GOVERNANCE"
        ));
        assert!(
            parse_retention_args(&args(&[
                "retention",
                "set",
                "a/b/k",
                "--mode",
                "FOREVER",
                "--retain-until",
                "2030-01-01T00:00:00Z",
            ]))
            .is_err()
        );
        assert!(
            parse_retention_args(&args(&[
                "retention",
                "set",
                "a/b/k",
                "--mode",
                "GOVERNANCE",
                "--retain-until",
                "2030-01-01",
            ]))
            .is_err()
        );
    }

    #[test]
    fn parse_retention_args_info_works() {
        let parsed = parse_retention_args(&args(&["retention", "info", "a/b/k"]))
            .expect("retention args should parse");
        match parsed {
            RetentionCommand::Info { target } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("b"));
                assert_eq!(target.key.as_deref(), Some("k"));
            }
            _ => panic!("expected retention info"),
        }
    }
}
