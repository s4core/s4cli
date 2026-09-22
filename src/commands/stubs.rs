//! Placeholder commands kept for `mc` CLI compatibility: `idp`, `ilm`, `replicate`.

use super::Context;
use crate::output::JsonObject;
use crate::target::S3Target;

#[derive(Debug)]
pub enum IdpKind {
    OpenId,
    Ldap,
}

#[derive(Debug)]
pub enum IlmKind {
    Rule,
    Tier,
    Restore,
}

#[derive(Debug)]
pub enum ReplicateSubcommand {
    Add,
    Update,
    List,
    Status,
    Resync,
    Export,
    Import,
    Remove,
    Backlog,
}

#[derive(Debug)]
pub struct ReplicateCommand {
    pub subcommand: ReplicateSubcommand,
    pub target: Option<S3Target>,
}

pub fn idp(ctx: &Context, args: &[String]) -> Result<(), String> {
    let provider = match parse_idp_args(args)? {
        IdpKind::OpenId => "openid",
        IdpKind::Ldap => "ldap",
    };
    ctx.report(
        not_implemented("idp", "provider", provider, "idp management"),
        format!("idp {provider} is not implemented in this build"),
    );
    Ok(())
}

pub fn ilm(ctx: &Context, args: &[String]) -> Result<(), String> {
    let section = match parse_ilm_args(args)? {
        IlmKind::Rule => "rule",
        IlmKind::Tier => "tier",
        IlmKind::Restore => "restore",
    };
    ctx.report(
        not_implemented("ilm", "section", section, "ilm management"),
        format!("ilm {section} is not implemented in this build"),
    );
    Ok(())
}

pub fn replicate(ctx: &Context, args: &[String]) -> Result<(), String> {
    let cmd = parse_replicate_args(args)?;
    let sub = match cmd.subcommand {
        ReplicateSubcommand::Add => "add",
        ReplicateSubcommand::Update => "update",
        ReplicateSubcommand::List => "list",
        ReplicateSubcommand::Status => "status",
        ReplicateSubcommand::Resync => "resync",
        ReplicateSubcommand::Export => "export",
        ReplicateSubcommand::Import => "import",
        ReplicateSubcommand::Remove => "remove",
        ReplicateSubcommand::Backlog => "backlog",
    };
    let target = cmd
        .target
        .as_ref()
        .and_then(|t| t.bucket.as_ref().map(|b| format!("{}/{}", t.alias, b)))
        .unwrap_or_else(|| "<no-target>".to_string());
    ctx.report(
        not_implemented("replicate", "subcommand", sub, "replication management"),
        format!("replicate {sub} is not implemented in this build (target: {target})"),
    );
    Ok(())
}

fn not_implemented(command: &str, field: &str, value: &str, what: &str) -> JsonObject {
    JsonObject::new()
        .str("status", "not_implemented")
        .str("command", command)
        .str(field, value)
        .str(
            "message",
            &format!("{what} is not implemented in this build"),
        )
}

pub fn parse_idp_args(args: &[String]) -> Result<IdpKind, String> {
    const USAGE: &str = "usage: s4 idp <openid|ldap> ...";
    match args.get(1).map(String::as_str) {
        None | Some("help" | "h") => Err(USAGE.to_string()),
        Some("openid") => Ok(IdpKind::OpenId),
        Some("ldap") => Ok(IdpKind::Ldap),
        Some(other) => Err(format!("unknown idp subcommand: {other}")),
    }
}

pub fn parse_ilm_args(args: &[String]) -> Result<IlmKind, String> {
    const USAGE: &str = "usage: s4 ilm <rule|tier|restore> ...";
    match args.get(1).map(String::as_str) {
        None | Some("help" | "h") => Err(USAGE.to_string()),
        Some("rule") => Ok(IlmKind::Rule),
        Some("tier") => Ok(IlmKind::Tier),
        Some("restore") => Ok(IlmKind::Restore),
        Some(other) => Err(format!("unknown ilm subcommand: {other}")),
    }
}

pub fn parse_replicate_args(args: &[String]) -> Result<ReplicateCommand, String> {
    const USAGE: &str = "usage: s4 replicate <add|update|list|ls|status|resync|export|import|remove|rm|backlog> [target]";
    let subcommand = match args.get(1).map(String::as_str) {
        None | Some("help" | "h") => return Err(USAGE.to_string()),
        Some("add") => ReplicateSubcommand::Add,
        Some("update") => ReplicateSubcommand::Update,
        Some("list" | "ls") => ReplicateSubcommand::List,
        Some("status") => ReplicateSubcommand::Status,
        Some("resync") => ReplicateSubcommand::Resync,
        Some("export") => ReplicateSubcommand::Export,
        Some("import") => ReplicateSubcommand::Import,
        Some("remove" | "rm") => ReplicateSubcommand::Remove,
        Some("backlog") => ReplicateSubcommand::Backlog,
        Some(other) => return Err(format!("unknown replicate subcommand: {other}")),
    };
    let target = args.get(2).map(|v| S3Target::parse(v)).transpose()?;
    Ok(ReplicateCommand { subcommand, target })
}

#[cfg(test)]
mod tests {
    use super::{
        IdpKind, IlmKind, ReplicateSubcommand, parse_idp_args, parse_ilm_args, parse_replicate_args,
    };
    use crate::cli::args;

    #[test]
    fn parse_idp_args_openid_works() {
        let parsed = parse_idp_args(&args(&["idp", "openid"])).expect("idp args should parse");
        assert!(matches!(parsed, IdpKind::OpenId));
    }

    #[test]
    fn parse_idp_args_ldap_works() {
        let parsed = parse_idp_args(&args(&["idp", "ldap"])).expect("idp args should parse");
        assert!(matches!(parsed, IdpKind::Ldap));
    }

    #[test]
    fn parse_ilm_args_rule_works() {
        let parsed = parse_ilm_args(&args(&["ilm", "rule"])).expect("ilm args should parse");
        assert!(matches!(parsed, IlmKind::Rule));
    }

    #[test]
    fn parse_ilm_args_restore_works() {
        let parsed = parse_ilm_args(&args(&["ilm", "restore"])).expect("ilm args should parse");
        assert!(matches!(parsed, IlmKind::Restore));
    }

    #[test]
    fn parse_replicate_args_list_alias_works() {
        let parsed = parse_replicate_args(&args(&["replicate", "ls", "a/bucket"]))
            .expect("replicate args should parse");
        assert!(matches!(parsed.subcommand, ReplicateSubcommand::List));
        let target = parsed.target.expect("target expected");
        assert_eq!(target.alias, "a");
        assert_eq!(target.bucket.as_deref(), Some("bucket"));
    }

    #[test]
    fn parse_replicate_args_backlog_works() {
        let parsed = parse_replicate_args(&args(&["replicate", "backlog"]))
            .expect("replicate args should parse");
        assert!(matches!(parsed.subcommand, ReplicateSubcommand::Backlog));
    }
}
