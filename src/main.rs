use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct AliasConfig {
    endpoint: String,
    access_key: String,
    secret_key: String,
    region: String,
    path_style: bool,
}

#[derive(Debug, Default)]
struct AppConfig {
    aliases: BTreeMap<String, AliasConfig>,
}

#[derive(Debug, Default)]
struct GlobalOpts {
    config_dir: Option<PathBuf>,
    json: bool,
    debug: bool,
    insecure: bool,
    resolve: Vec<String>,
    limit_upload: Option<String>,
    limit_download: Option<String>,
    custom_headers: Vec<String>,
}

#[derive(Debug)]
struct S3Target {
    alias: String,
    bucket: Option<String>,
    key: Option<String>,
}

#[derive(Debug, Default)]
struct SyncOptions {
    overwrite: bool,
    dry_run: bool,
    remove: bool,
    watch: bool,
    excludes: Vec<String>,
    newer_than: Option<u64>,
    older_than: Option<u64>,
}

#[derive(Debug)]
enum CorsCommand {
    Set { target: S3Target, file: PathBuf },
    Get { target: S3Target },
    Remove { target: S3Target },
}

#[derive(Debug)]
enum EncryptCommand {
    Set { target: S3Target, file: PathBuf },
    Clear { target: S3Target },
    Info { target: S3Target },
}

#[derive(Debug)]
enum EventCommand {
    Add { target: S3Target, file: PathBuf },
    Remove { target: S3Target, force: bool },
    List { target: S3Target },
}

#[derive(Debug)]
enum IdpKind {
    OpenId,
    Ldap,
}

#[derive(Debug)]
struct IdpCommand {
    kind: IdpKind,
}

#[derive(Debug)]
enum IlmKind {
    Rule,
    Tier,
    Restore,
}

#[derive(Debug)]
struct IlmCommand {
    kind: IlmKind,
}

#[derive(Debug)]
enum LegalHoldCommand {
    Set { target: S3Target },
    Clear { target: S3Target },
    Info { target: S3Target },
}

#[derive(Debug)]
enum RetentionCommand {
    Set {
        target: S3Target,
        mode: String,
        retain_until: String,
    },
    Clear {
        target: S3Target,
    },
    Info {
        target: S3Target,
    },
}

#[derive(Debug)]
enum ReplicateSubcommand {
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
struct ReplicateCommand {
    subcommand: ReplicateSubcommand,
    target: Option<S3Target>,
}

#[derive(Debug, Clone)]
struct SqlOptions {
    query: String,
    recursive: bool,
    csv_input: Option<String>,
    json_input: Option<String>,
    compression: Option<String>,
    csv_output: Option<String>,
    csv_output_header: Option<String>,
    json_output: Option<String>,
    enc_c: Vec<String>,
}

#[derive(Debug)]
struct Endpoint {
    scheme: String,
    host: String,
    base_path: String,
}

#[derive(Debug)]
struct SignatureParts {
    amz_date: String,
    authorization: String,
}

static CURL_INSECURE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Default, Clone)]
struct CurlGlobalOpts {
    resolve: Vec<String>,
    limit_upload: Option<String>,
    limit_download: Option<String>,
    custom_headers: Vec<String>,
}

static CURL_GLOBAL_OPTS: OnceLock<Mutex<CurlGlobalOpts>> = OnceLock::new();

fn curl_global_opts() -> &'static Mutex<CurlGlobalOpts> {
    CURL_GLOBAL_OPTS.get_or_init(|| Mutex::new(CurlGlobalOpts::default()))
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = env::args().collect();
    if args.len() == 1 {
        print_help();
        return Ok(());
    }
    args.remove(0);

    let (opts, rest) = parse_globals(args)?;
    if rest.is_empty() {
        print_help();
        return Ok(());
    }

    if rest[0] == "--help" || rest[0] == "-h" {
        print_help();
        return Ok(());
    }
    if rest[0] == "--version" || rest[0] == "-v" || rest[0] == "version" {
        println!("s4 {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let config_path = resolve_config_path(opts.config_dir.as_deref())?;
    let mut config = load_config(&config_path)?;

    if opts.debug {
        eprintln!("[debug] config: {}", config_path.display());
    }
    if opts.insecure {
        // Propagate to all curl invocations (including multipart paths).
        CURL_INSECURE.store(true, Ordering::Relaxed);
    }
    {
        let mut curl_opts = curl_global_opts().lock().map_err(|e| e.to_string())?;
        curl_opts.resolve = opts.resolve.clone();
        curl_opts.limit_upload = opts.limit_upload.clone();
        curl_opts.limit_download = opts.limit_download.clone();
        curl_opts.custom_headers = opts.custom_headers.clone();
    }

    match rest[0].as_str() {
        "alias" => handle_alias(&rest[1..], &mut config, &config_path, opts.json),
        "ls" | "mb" | "rb" | "put" | "get" | "rm" | "stat" | "cat" | "sync" | "mirror" | "cp"
        | "mv" | "find" | "tree" | "head" | "pipe" | "ping" | "ready" | "cors" | "encrypt"
        | "event" | "legalhold" | "retention" | "sql" | "idp" | "ilm" | "replicate" => {
            handle_s3_command(&rest, &config, opts.json, opts.debug)
        }
        _ => Err(format!("unknown command: {}", rest[0])),
    }
}

fn parse_globals(args: Vec<String>) -> Result<(GlobalOpts, Vec<String>), String> {
    let mut opts = GlobalOpts::default();
    let mut rest = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-C" | "--config-dir" => {
                let next = args.get(i + 1).ok_or("--config-dir expects a value")?;
                opts.config_dir = Some(PathBuf::from(next));
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
                opts.insecure = true;
                i += 1;
            }
            "--resolve" => {
                let value = args.get(i + 1).ok_or("--resolve expects a value")?;
                opts.resolve.push(value.to_string());
                i += 2;
            }
            "--limit-upload" => {
                let value = args.get(i + 1).ok_or("--limit-upload expects a value")?;
                opts.limit_upload = Some(value.to_string());
                i += 2;
            }
            "--limit-download" => {
                let value = args.get(i + 1).ok_or("--limit-download expects a value")?;
                opts.limit_download = Some(value.to_string());
                i += 2;
            }
            "--custom-header" | "-H" => {
                let value = args.get(i + 1).ok_or("--custom-header expects a value")?;
                opts.custom_headers.push(value.to_string());
                i += 2;
            }
            "--help" | "-h" | "--version" | "-v" => {
                rest.extend_from_slice(&args[i..]);
                break;
            }
            x if x.starts_with('-') => return Err(format!("unknown global flag: {x}")),
            _ => {
                rest.extend_from_slice(&args[i..]);
                break;
            }
        }
    }

    Ok((opts, rest))
}

fn handle_alias(
    args: &[String],
    config: &mut AppConfig,
    config_path: &Path,
    json: bool,
) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: s4 alias <set|ls|rm> ...".to_string());
    }

    match args[0].as_str() {
        "set" => {
            if args.len() < 5 {
                return Err("usage: s4 alias set <name> <endpoint> <access> <secret> [--region r] [--path-style]".to_string());
            }
            let mut region = "us-east-1".to_string();
            let mut path_style = false;
            let mut i = 5;
            while i < args.len() {
                match args[i].as_str() {
                    "--region" => {
                        region = args
                            .get(i + 1)
                            .ok_or("--region expects a value")?
                            .to_string();
                        i += 2;
                    }
                    "--path-style" => {
                        path_style = true;
                        i += 1;
                    }
                    other => return Err(format!("unknown alias set flag: {other}")),
                }
            }

            config.aliases.insert(
                args[1].clone(),
                AliasConfig {
                    endpoint: args[2].clone(),
                    access_key: args[3].clone(),
                    secret_key: args[4].clone(),
                    region,
                    path_style,
                },
            );
            save_config(config_path, config)?;
            if json {
                println!("{{\"status\":\"ok\",\"alias\":\"{}\"}}", args[1]);
            } else {
                println!("Alias '{}' saved", args[1]);
            }
            Ok(())
        }
        "ls" => {
            if json {
                print!("[");
                for (idx, (name, alias)) in config.aliases.iter().enumerate() {
                    if idx > 0 {
                        print!(",");
                    }
                    print!(
                        "{{\"name\":\"{}\",\"endpoint\":\"{}\",\"region\":\"{}\",\"path_style\":{}}}",
                        escape_json(name),
                        escape_json(&alias.endpoint),
                        escape_json(&alias.region),
                        alias.path_style
                    );
                }
                println!("]");
            } else {
                for (name, alias) in &config.aliases {
                    println!(
                        "{name}\t{}\t{}\tpath_style={}",
                        alias.endpoint, alias.region, alias.path_style
                    );
                }
            }
            Ok(())
        }
        "rm" => {
            let name = args.get(1).ok_or("usage: s4 alias rm <name>")?;
            let existed = config.aliases.remove(name).is_some();
            save_config(config_path, config)?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"alias\":\"{}\",\"removed\":{}}}",
                    escape_json(name),
                    existed
                );
            } else if existed {
                println!("Alias '{name}' removed");
            } else {
                println!("Alias '{name}' not found");
            }
            Ok(())
        }
        _ => Err("usage: s4 alias <set|ls|rm> ...".to_string()),
    }
}

fn handle_s3_command(
    args: &[String],
    config: &AppConfig,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    let command = &args[0];
    let target_idx = if command == "put" { 2 } else { 1 };
    if command != "sync"
        && command != "mirror"
        && command != "cp"
        && command != "mv"
        && command != "find"
        && command != "tree"
        && command != "head"
        && command != "pipe"
        && command != "ping"
        && command != "ready"
        && command != "cors"
        && command != "encrypt"
        && command != "event"
        && command != "idp"
        && command != "ilm"
        && command != "legalhold"
        && command != "replicate"
        && command != "retention"
        && command != "sql"
        && command != "mb"
        && args.len() <= target_idx
    {
        return Err(format!("usage: s4 {command} ..."));
    }

    if command == "cp" || command == "mv" {
        if args.len() < 3 {
            return Err(format!("usage: s4 {command} <source> <target>"));
        }
        return cmd_cp_mv(command, config, &args[1], &args[2], json, debug);
    }

    if command == "mb" {
        if args.len() < 2 {
            return Err("usage: s4 mb [--with-lock] <alias/bucket>".to_string());
        }
        let mut with_lock = false;
        let mut target_arg: Option<&String> = None;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--with-lock" => {
                    with_lock = true;
                    i += 1;
                }
                x if x.starts_with('-') => return Err(format!("unknown mb flag: {x}")),
                _ => {
                    target_arg = Some(&args[i]);
                    i += 1;
                }
            }
        }
        let target_val = target_arg.ok_or("usage: s4 mb [--with-lock] <alias/bucket>")?;
        let target = parse_target(target_val)?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        let bucket = req_bucket(&target, "mb")?;
        if with_lock {
            let headers = vec!["x-amz-bucket-object-lock-enabled: true".to_string()];
            s3_request_with_headers(alias, "PUT", &bucket, None, "", None, None, &headers, debug)?;
        } else {
            s3_request(alias, "PUT", &bucket, None, "", None, None, debug)?;
        }
        print_status(json, "created", &bucket);
        return Ok(());
    }

    if command == "find" {
        if args.len() < 2 {
            return Err("usage: s4 find <alias/bucket[/prefix]> [needle]".to_string());
        }
        let target = parse_target(&args[1])?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        let bucket = req_bucket(&target, "find")?;
        let prefix = target.key.clone().unwrap_or_default();
        let needle = args.get(2).cloned();
        return cmd_find(alias, &bucket, &prefix, needle.as_deref(), json, debug);
    }

    if command == "tree" {
        if args.len() < 2 {
            return Err("usage: s4 tree <alias/bucket[/prefix]>".to_string());
        }
        let target = parse_target(&args[1])?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        let bucket = req_bucket(&target, "tree")?;
        let prefix = target.key.clone().unwrap_or_default();
        return cmd_tree(alias, &bucket, &prefix, json, debug);
    }

    if command == "head" {
        if args.len() < 2 {
            return Err("usage: s4 head <alias/bucket/key> [lines]".to_string());
        }
        let target = parse_target(&args[1])?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        let bucket = req_bucket(&target, "head")?;
        let key = req_key(&target, "head")?;
        let lines = args
            .get(2)
            .map(|v| {
                v.parse::<usize>()
                    .map_err(|_| "head lines must be integer".to_string())
            })
            .transpose()?
            .unwrap_or(10);
        return cmd_head(alias, &bucket, &key, lines, debug);
    }

    if command == "pipe" {
        if args.len() < 2 {
            return Err("usage: s4 pipe <alias/bucket/key>".to_string());
        }
        let target = parse_target(&args[1])?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        let bucket = req_bucket(&target, "pipe")?;
        let key = req_key(&target, "pipe")?;
        return cmd_pipe(alias, &bucket, &key, json, debug);
    }

    if command == "ping" {
        if args.len() < 2 {
            return Err("usage: s4 ping <alias>".to_string());
        }
        let target = parse_target(&args[1])?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        return cmd_ping(&target.alias, alias, json, debug);
    }

    if command == "ready" {
        if args.len() < 2 {
            return Err("usage: s4 ready <alias>".to_string());
        }
        let target = parse_target(&args[1])?;
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        return cmd_ready(&target.alias, alias, json, debug);
    }

    if command == "cors" {
        let cors_cmd = parse_cors_args(args)?;
        return cmd_cors(config, cors_cmd, json, debug);
    }

    if command == "encrypt" {
        let encrypt_cmd = parse_encrypt_args(args)?;
        return cmd_encrypt(config, encrypt_cmd, json, debug);
    }

    if command == "event" {
        let event_cmd = parse_event_args(args)?;
        return cmd_event(config, event_cmd, json, debug);
    }

    if command == "idp" {
        let idp_cmd = parse_idp_args(args)?;
        return cmd_idp(idp_cmd, json);
    }

    if command == "ilm" {
        let ilm_cmd = parse_ilm_args(args)?;
        return cmd_ilm(ilm_cmd, json);
    }

    if command == "legalhold" {
        let lh_cmd = parse_legalhold_args(args)?;
        return cmd_legalhold(config, lh_cmd, json, debug);
    }

    if command == "retention" {
        let rt_cmd = parse_retention_args(args)?;
        return cmd_retention(config, rt_cmd, json, debug);
    }

    if command == "sql" {
        let (sql_opts, sql_targets) = parse_sql_args(args)?;
        return cmd_sql(config, &sql_opts, &sql_targets, json, debug);
    }

    if command == "replicate" {
        let rep_cmd = parse_replicate_args(args)?;
        return cmd_replicate(rep_cmd, json);
    }

    if command == "sync" || command == "mirror" {
        let (sync_opts, src, dst) = parse_sync_args(args)?;
        return cmd_sync(config, &src, &dst, &sync_opts, json, debug);
    }

    let target = parse_target(&args[target_idx])?;
    let alias = config
        .aliases
        .get(&target.alias)
        .ok_or_else(|| format!("unknown alias: {}", target.alias))?;

    match command.as_str() {
        "ls" => cmd_ls(alias, &target, json, debug),
        "rb" => {
            let bucket = req_bucket(&target, "rb")?;
            if let Err(err) = s3_request(alias, "DELETE", &bucket, None, "", None, None, debug) {
                if err.contains("BucketNotEmpty") {
                    purge_bucket_versions(alias, &bucket, debug)?;
                    s3_request(alias, "DELETE", &bucket, None, "", None, None, debug)?;
                } else {
                    return Err(err);
                }
            }
            print_status(json, "deleted", &bucket);
            Ok(())
        }
        "put" => {
            if args.len() < 3 {
                return Err("usage: s4 put <source_file> <alias/bucket/key>".to_string());
            }
            let source = PathBuf::from(&args[1]);
            if !source.exists() {
                return Err(format!("source file not found: {}", source.display()));
            }
            let bucket = req_bucket(&target, "put")?;
            let key = req_key(&target, "put")?;
            upload_file_to_s3(alias, &bucket, &key, &source, debug)?;
            if json {
                println!(
                    "{{\"uploaded\":{{\"bucket\":\"{}\",\"key\":\"{}\"}}}}",
                    escape_json(&bucket),
                    escape_json(&key)
                );
            } else {
                println!("Uploaded '{}' to '{}/{}'", source.display(), bucket, key);
            }
            Ok(())
        }
        "get" => {
            if args.len() < 3 {
                return Err("usage: s4 get <alias/bucket/key> <destination_file>".to_string());
            }
            let bucket = req_bucket(&target, "get")?;
            let key = req_key(&target, "get")?;
            let destination = PathBuf::from(&args[2]);
            if let Some(parent) = destination.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
            }
            s3_request(
                alias,
                "GET",
                &bucket,
                Some(&key),
                "",
                None,
                Some(&destination),
                debug,
            )?;
            if json {
                println!(
                    "{{\"downloaded\":{{\"bucket\":\"{}\",\"key\":\"{}\",\"to\":\"{}\"}}}}",
                    escape_json(&bucket),
                    escape_json(&key),
                    escape_json(&destination.display().to_string())
                );
            } else {
                println!(
                    "Downloaded '{}/{}' to '{}'",
                    bucket,
                    key,
                    destination.display()
                );
            }
            Ok(())
        }
        "rm" => {
            let bucket = req_bucket(&target, "rm")?;
            let key = req_key(&target, "rm")?;
            match s3_request(alias, "DELETE", &bucket, Some(&key), "", None, None, debug) {
                Ok(_) => {}
                Err(err) => {
                    if should_retry_with_governance_bypass(&err) {
                        let headers = vec!["x-amz-bypass-governance-retention: true".to_string()];
                        s3_request_with_headers(
                            alias,
                            "DELETE",
                            &bucket,
                            Some(&key),
                            "",
                            None,
                            None,
                            &headers,
                            debug,
                        )?;
                    } else {
                        return Err(err);
                    }
                }
            }
            if json {
                println!(
                    "{{\"deleted\":{{\"bucket\":\"{}\",\"key\":\"{}\"}}}}",
                    escape_json(&bucket),
                    escape_json(&key)
                );
            } else {
                println!("Deleted '{}/{}'", bucket, key);
            }
            Ok(())
        }
        "stat" => {
            let bucket = req_bucket(&target, "stat")?;
            let key = req_key(&target, "stat")?;
            let headers = s3_request(alias, "HEAD", &bucket, Some(&key), "", None, None, debug)?;
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"key\":\"{}\",\"headers\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key),
                    escape_json(&headers)
                );
            } else {
                println!("{}", headers);
            }
            Ok(())
        }
        "cat" => {
            let bucket = req_bucket(&target, "cat")?;
            let key = req_key(&target, "cat")?;
            let body = s3_request(alias, "GET", &bucket, Some(&key), "", None, None, debug)?;
            print!("{}", body);
            Ok(())
        }
        "sync" | "mirror" => unreachable!(),
        "cp" | "mv" | "find" | "tree" | "head" | "pipe" | "ping" | "ready" | "cors" | "encrypt"
        | "event" => {
            unreachable!()
        }
        _ => Err(format!("unsupported command: {command}")),
    }
}

fn parse_ilm_args(args: &[String]) -> Result<IlmCommand, String> {
    if args.len() < 2 {
        return Err("usage: s4 ilm <rule|tier|restore> ...".to_string());
    }
    let kind = match args[1].as_str() {
        "rule" => IlmKind::Rule,
        "tier" => IlmKind::Tier,
        "restore" => IlmKind::Restore,
        "help" | "h" => return Err("usage: s4 ilm <rule|tier|restore> ...".to_string()),
        other => return Err(format!("unknown ilm subcommand: {other}")),
    };
    Ok(IlmCommand { kind })
}

fn cmd_ilm(cmd: IlmCommand, json: bool) -> Result<(), String> {
    let section = match cmd.kind {
        IlmKind::Rule => "rule",
        IlmKind::Tier => "tier",
        IlmKind::Restore => "restore",
    };
    if json {
        println!(
            "{{\"status\":\"not_implemented\",\"command\":\"ilm\",\"section\":\"{}\",\"message\":\"ilm management is not implemented in this build\"}}",
            section
        );
    } else {
        println!("ilm {} is not implemented in this build", section);
    }
    Ok(())
}

fn parse_idp_args(args: &[String]) -> Result<IdpCommand, String> {
    if args.len() < 2 {
        return Err("usage: s4 idp <openid|ldap> ...".to_string());
    }
    let kind = match args[1].as_str() {
        "openid" => IdpKind::OpenId,
        "ldap" => IdpKind::Ldap,
        "help" | "h" => return Err("usage: s4 idp <openid|ldap> ...".to_string()),
        other => return Err(format!("unknown idp subcommand: {other}")),
    };
    Ok(IdpCommand { kind })
}

fn cmd_idp(cmd: IdpCommand, json: bool) -> Result<(), String> {
    let provider = match cmd.kind {
        IdpKind::OpenId => "openid",
        IdpKind::Ldap => "ldap",
    };
    if json {
        println!(
            "{{\"status\":\"not_implemented\",\"command\":\"idp\",\"provider\":\"{}\",\"message\":\"idp management is not implemented in this build\"}}",
            provider
        );
    } else {
        println!("idp {} is not implemented in this build", provider);
    }
    Ok(())
}

fn parse_cors_args(args: &[String]) -> Result<CorsCommand, String> {
    if args.len() < 3 {
        return Err("usage: s4 cors <set|get|remove> ...".to_string());
    }
    match args[1].as_str() {
        "set" => {
            if args.len() < 4 {
                return Err("usage: s4 cors set <alias/bucket> <cors_xml_file>".to_string());
            }
            let target = parse_target(&args[2])?;
            let file = PathBuf::from(&args[3]);
            Ok(CorsCommand::Set { target, file })
        }
        "get" => {
            let target = parse_target(&args[2])?;
            Ok(CorsCommand::Get { target })
        }
        "remove" => {
            let target = parse_target(&args[2])?;
            Ok(CorsCommand::Remove { target })
        }
        "help" | "h" => Err("usage: s4 cors <set|get|remove> ...".to_string()),
        other => Err(format!("unknown cors subcommand: {other}")),
    }
}

fn cmd_cors(config: &AppConfig, cmd: CorsCommand, json: bool, debug: bool) -> Result<(), String> {
    match cmd {
        CorsCommand::Set { target, file } => {
            if !file.exists() {
                return Err(format!("cors file not found: {}", file.display()));
            }
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "cors set")?;
            s3_request(
                alias,
                "PUT",
                &bucket,
                None,
                "cors",
                Some(&file),
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"cors set\",\"bucket\":\"{}\"}}",
                    escape_json(&bucket)
                );
            } else {
                println!("CORS set for bucket '{}'", bucket);
            }
            Ok(())
        }
        CorsCommand::Get { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "cors get")?;
            let body = s3_request(alias, "GET", &bucket, None, "cors", None, None, debug)?;
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"cors\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&body)
                );
            } else {
                print!("{}", body);
            }
            Ok(())
        }
        CorsCommand::Remove { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "cors remove")?;
            s3_request(alias, "DELETE", &bucket, None, "cors", None, None, debug)?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"cors remove\",\"bucket\":\"{}\"}}",
                    escape_json(&bucket)
                );
            } else {
                println!("CORS removed for bucket '{}'", bucket);
            }
            Ok(())
        }
    }
}

fn parse_encrypt_args(args: &[String]) -> Result<EncryptCommand, String> {
    if args.len() < 3 {
        return Err("usage: s4 encrypt <set|clear|info> ...".to_string());
    }
    match args[1].as_str() {
        "set" => {
            if args.len() < 4 {
                return Err(
                    "usage: s4 encrypt set <alias/bucket> <encryption_xml_file>".to_string()
                );
            }
            let target = parse_target(&args[2])?;
            let file = PathBuf::from(&args[3]);
            Ok(EncryptCommand::Set { target, file })
        }
        "clear" => {
            let target = parse_target(&args[2])?;
            Ok(EncryptCommand::Clear { target })
        }
        "info" => {
            let target = parse_target(&args[2])?;
            Ok(EncryptCommand::Info { target })
        }
        "help" | "h" => Err("usage: s4 encrypt <set|clear|info> ...".to_string()),
        other => Err(format!("unknown encrypt subcommand: {other}")),
    }
}

fn cmd_encrypt(
    config: &AppConfig,
    cmd: EncryptCommand,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    match cmd {
        EncryptCommand::Set { target, file } => {
            if !file.exists() {
                return Err(format!("encryption file not found: {}", file.display()));
            }
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "encrypt set")?;
            s3_request(
                alias,
                "PUT",
                &bucket,
                None,
                "encryption",
                Some(&file),
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"encrypt set\",\"bucket\":\"{}\"}}",
                    escape_json(&bucket)
                );
            } else {
                println!("Encryption set for bucket '{}'", bucket);
            }
            Ok(())
        }
        EncryptCommand::Clear { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "encrypt clear")?;
            s3_request(
                alias,
                "DELETE",
                &bucket,
                None,
                "encryption",
                None,
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"encrypt clear\",\"bucket\":\"{}\"}}",
                    escape_json(&bucket)
                );
            } else {
                println!("Encryption cleared for bucket '{}'", bucket);
            }
            Ok(())
        }
        EncryptCommand::Info { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "encrypt info")?;
            let body = s3_request(alias, "GET", &bucket, None, "encryption", None, None, debug)?;
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"encryption\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&body)
                );
            } else {
                print!("{}", body);
            }
            Ok(())
        }
    }
}

fn parse_event_args(args: &[String]) -> Result<EventCommand, String> {
    if args.len() < 3 {
        return Err("usage: s4 event <add|remove|rm|list|ls> ...".to_string());
    }
    match args[1].as_str() {
        "add" => {
            if args.len() < 4 {
                return Err(
                    "usage: s4 event add <alias/bucket> <notification_xml_file>".to_string()
                );
            }
            let target = parse_target(&args[2])?;
            let file = PathBuf::from(&args[3]);
            Ok(EventCommand::Add { target, file })
        }
        "remove" | "rm" => {
            let target = parse_target(&args[2])?;
            let force = args.iter().any(|a| a == "--force");
            Ok(EventCommand::Remove { target, force })
        }
        "list" | "ls" => {
            let target = parse_target(&args[2])?;
            Ok(EventCommand::List { target })
        }
        "help" | "h" => Err("usage: s4 event <add|remove|rm|list|ls> ...".to_string()),
        other => Err(format!("unknown event subcommand: {other}")),
    }
}

fn cmd_event(config: &AppConfig, cmd: EventCommand, json: bool, debug: bool) -> Result<(), String> {
    match cmd {
        EventCommand::Add { target, file } => {
            if !file.exists() {
                return Err(format!("notification file not found: {}", file.display()));
            }
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "event add")?;
            s3_request(
                alias,
                "PUT",
                &bucket,
                None,
                "notification",
                Some(&file),
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"event add\",\"bucket\":\"{}\"}}",
                    escape_json(&bucket)
                );
            } else {
                println!("Notification config set for bucket '{}'", bucket);
            }
            Ok(())
        }
        EventCommand::Remove { target, force } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "event remove")?;
            s3_request(
                alias,
                "PUT",
                &bucket,
                None,
                "notification",
                None,
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"event remove\",\"bucket\":\"{}\",\"force\":{}}}",
                    escape_json(&bucket),
                    force
                );
            } else {
                println!(
                    "Notification config removed for bucket '{}' (force: {})",
                    bucket, force
                );
            }
            Ok(())
        }
        EventCommand::List { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "event list")?;
            let body = s3_request(
                alias,
                "GET",
                &bucket,
                None,
                "notification",
                None,
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"notification\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&body)
                );
            } else {
                print!("{}", body);
            }
            Ok(())
        }
    }
}

fn parse_legalhold_args(args: &[String]) -> Result<LegalHoldCommand, String> {
    if args.len() < 3 {
        return Err("usage: s4 legalhold <set|clear|info> <alias/bucket/key>".to_string());
    }
    match args[1].as_str() {
        "set" => Ok(LegalHoldCommand::Set {
            target: parse_target(&args[2])?,
        }),
        "clear" => Ok(LegalHoldCommand::Clear {
            target: parse_target(&args[2])?,
        }),
        "info" => Ok(LegalHoldCommand::Info {
            target: parse_target(&args[2])?,
        }),
        "help" | "h" => Err("usage: s4 legalhold <set|clear|info> <alias/bucket/key>".to_string()),
        other => Err(format!("unknown legalhold subcommand: {other}")),
    }
}

fn content_md5_header(file_path: &Path) -> Result<String, String> {
    let script = r#"
import base64, hashlib, pathlib, sys
p = pathlib.Path(sys.argv[1])
data = p.read_bytes()
print(base64.b64encode(hashlib.md5(data).digest()).decode())
"#;
    let out = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(file_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "failed to compute content-md5: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn cmd_legalhold(
    config: &AppConfig,
    cmd: LegalHoldCommand,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    match cmd {
        LegalHoldCommand::Set { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "legalhold set")?;
            let key = req_key(&target, "legalhold set")?;
            let body = "<LegalHold><Status>ON</Status></LegalHold>";
            let temp = env::temp_dir().join(format!("s4-legalhold-{}-on.xml", std::process::id()));
            fs::write(&temp, body).map_err(|e| e.to_string())?;
            let md5 = content_md5_header(&temp)?;
            let headers = vec![format!("Content-MD5: {}", md5)];
            let res = s3_request_with_headers(
                alias,
                "PUT",
                &bucket,
                Some(&key),
                "legal-hold",
                Some(&temp),
                None,
                &headers,
                debug,
            );
            let _ = fs::remove_file(&temp);
            res?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"legalhold set\",\"bucket\":\"{}\",\"key\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key)
                );
            } else {
                println!("Legal hold set for '{}/{}'", bucket, key);
            }
            Ok(())
        }
        LegalHoldCommand::Clear { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "legalhold clear")?;
            let key = req_key(&target, "legalhold clear")?;
            let body = "<LegalHold><Status>OFF</Status></LegalHold>";
            let temp = env::temp_dir().join(format!("s4-legalhold-{}-off.xml", std::process::id()));
            fs::write(&temp, body).map_err(|e| e.to_string())?;
            let md5 = content_md5_header(&temp)?;
            let headers = vec![format!("Content-MD5: {}", md5)];
            let res = s3_request_with_headers(
                alias,
                "PUT",
                &bucket,
                Some(&key),
                "legal-hold",
                Some(&temp),
                None,
                &headers,
                debug,
            );
            let _ = fs::remove_file(&temp);
            res?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"legalhold clear\",\"bucket\":\"{}\",\"key\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key)
                );
            } else {
                println!("Legal hold cleared for '{}/{}'", bucket, key);
            }
            Ok(())
        }
        LegalHoldCommand::Info { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "legalhold info")?;
            let key = req_key(&target, "legalhold info")?;
            let body = s3_request(
                alias,
                "GET",
                &bucket,
                Some(&key),
                "legal-hold",
                None,
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"key\":\"{}\",\"legalhold\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key),
                    escape_json(&body)
                );
            } else {
                print!("{}", body);
            }
            Ok(())
        }
    }
}

fn parse_retention_args(args: &[String]) -> Result<RetentionCommand, String> {
    if args.len() < 3 {
        return Err("usage: s4 retention <set|clear|info> ...".to_string());
    }
    match args[1].as_str() {
        "set" => {
            if args.len() < 4 {
                return Err("usage: s4 retention set <alias/bucket/key> --mode <GOVERNANCE|COMPLIANCE> --retain-until <RFC3339>".to_string());
            }
            let target = parse_target(&args[2])?;
            let mut mode: Option<String> = None;
            let mut retain_until: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--mode" => {
                        let v = args.get(i + 1).ok_or("--mode expects a value")?;
                        mode = Some(v.to_string());
                        i += 2;
                    }
                    "--retain-until" => {
                        let v = args.get(i + 1).ok_or("--retain-until expects a value")?;
                        retain_until = Some(v.to_string());
                        i += 2;
                    }
                    f if f.starts_with('-') => {
                        return Err(format!("unknown retention set flag: {f}"));
                    }
                    other => return Err(format!("unexpected retention set argument: {other}")),
                }
            }
            let mode = mode.ok_or("retention set requires --mode")?;
            let retain_until = retain_until.ok_or("retention set requires --retain-until")?;
            Ok(RetentionCommand::Set {
                target,
                mode,
                retain_until,
            })
        }
        "clear" => Ok(RetentionCommand::Clear {
            target: parse_target(&args[2])?,
        }),
        "info" => Ok(RetentionCommand::Info {
            target: parse_target(&args[2])?,
        }),
        "help" | "h" => Err("usage: s4 retention <set|clear|info> ...".to_string()),
        other => Err(format!("unknown retention subcommand: {other}")),
    }
}

fn cmd_retention(
    config: &AppConfig,
    cmd: RetentionCommand,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    match cmd {
        RetentionCommand::Set {
            target,
            mode,
            retain_until,
        } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "retention set")?;
            let key = req_key(&target, "retention set")?;
            let body = format!(
                "<Retention><Mode>{}</Mode><RetainUntilDate>{}</RetainUntilDate></Retention>",
                mode, retain_until
            );
            let temp = env::temp_dir().join(format!("s4-retention-{}-set.xml", std::process::id()));
            fs::write(&temp, body).map_err(|e| e.to_string())?;
            let md5 = content_md5_header(&temp)?;
            let headers = vec![format!("Content-MD5: {}", md5)];
            let res = s3_request_with_headers(
                alias,
                "PUT",
                &bucket,
                Some(&key),
                "retention",
                Some(&temp),
                None,
                &headers,
                debug,
            );
            let _ = fs::remove_file(&temp);
            res?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"retention set\",\"bucket\":\"{}\",\"key\":\"{}\",\"mode\":\"{}\",\"retain_until\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key),
                    escape_json(&mode),
                    escape_json(&retain_until)
                );
            } else {
                println!(
                    "Retention set for '{}/{}' mode={} retain-until={}",
                    bucket, key, mode, retain_until
                );
            }
            Ok(())
        }
        RetentionCommand::Clear { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "retention clear")?;
            let key = req_key(&target, "retention clear")?;

            // S3/MinIO clear path is PUT ObjectRetention update, not DELETE.
            let now_out = Command::new("python3")
                .arg("-c")
                .arg("import datetime; print((datetime.datetime.utcnow()+datetime.timedelta(minutes=1)).strftime('%Y-%m-%dT%H:%M:%SZ'))")
                .output()
                .map_err(|e| e.to_string())?;
            if !now_out.status.success() {
                return Err(format!(
                    "failed to produce retention clear timestamp: {}",
                    String::from_utf8_lossy(&now_out.stderr).trim()
                ));
            }
            let retain_until = String::from_utf8_lossy(&now_out.stdout).trim().to_string();
            let body = format!(
                "<Retention><Mode>GOVERNANCE</Mode><RetainUntilDate>{}</RetainUntilDate></Retention>",
                retain_until
            );
            let temp =
                env::temp_dir().join(format!("s4-retention-{}-clear.xml", std::process::id()));
            fs::write(&temp, body).map_err(|e| e.to_string())?;
            let md5 = content_md5_header(&temp)?;
            let headers = vec![
                format!("Content-MD5: {}", md5),
                "x-amz-bypass-governance-retention: true".to_string(),
            ];
            let res = s3_request_with_headers(
                alias,
                "PUT",
                &bucket,
                Some(&key),
                "retention",
                Some(&temp),
                None,
                &headers,
                debug,
            );
            let _ = fs::remove_file(&temp);
            res?;
            if json {
                println!(
                    "{{\"status\":\"ok\",\"command\":\"retention clear\",\"bucket\":\"{}\",\"key\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key)
                );
            } else {
                println!("Retention cleared for '{}/{}'", bucket, key);
            }
            Ok(())
        }
        RetentionCommand::Info { target } => {
            let alias = config
                .aliases
                .get(&target.alias)
                .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
            let bucket = req_bucket(&target, "retention info")?;
            let key = req_key(&target, "retention info")?;
            let body = s3_request(
                alias,
                "GET",
                &bucket,
                Some(&key),
                "retention",
                None,
                None,
                debug,
            )?;
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"key\":\"{}\",\"retention\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key),
                    escape_json(&body)
                );
            } else {
                print!("{}", body);
            }
            Ok(())
        }
    }
}

fn parse_replicate_args(args: &[String]) -> Result<ReplicateCommand, String> {
    if args.len() < 2 {
        return Err("usage: s4 replicate <add|update|list|ls|status|resync|export|import|remove|rm|backlog> [target]".to_string());
    }
    let subcommand = match args[1].as_str() {
        "add" => ReplicateSubcommand::Add,
        "update" => ReplicateSubcommand::Update,
        "list" | "ls" => ReplicateSubcommand::List,
        "status" => ReplicateSubcommand::Status,
        "resync" => ReplicateSubcommand::Resync,
        "export" => ReplicateSubcommand::Export,
        "import" => ReplicateSubcommand::Import,
        "remove" | "rm" => ReplicateSubcommand::Remove,
        "backlog" => ReplicateSubcommand::Backlog,
        "help" | "h" => return Err("usage: s4 replicate <add|update|list|ls|status|resync|export|import|remove|rm|backlog> [target]".to_string()),
        other => return Err(format!("unknown replicate subcommand: {other}")),
    };
    let target = args.get(2).map(|v| parse_target(v)).transpose()?;
    Ok(ReplicateCommand { subcommand, target })
}

fn cmd_replicate(cmd: ReplicateCommand, json: bool) -> Result<(), String> {
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
    if json {
        println!(
            "{{\"status\":\"not_implemented\",\"command\":\"replicate\",\"subcommand\":\"{}\",\"message\":\"replication management is not implemented in this build\"}}",
            sub
        );
    } else {
        let target = cmd
            .target
            .as_ref()
            .and_then(|t| t.bucket.as_ref().map(|b| format!("{}/{}", t.alias, b)))
            .unwrap_or_else(|| "<no-target>".to_string());
        println!(
            "replicate {} is not implemented in this build (target: {})",
            sub, target
        );
    }
    Ok(())
}

fn parse_sql_args(args: &[String]) -> Result<(SqlOptions, Vec<S3Target>), String> {
    let mut opts = SqlOptions {
        query: "select * from S3Object".to_string(),
        recursive: false,
        csv_input: None,
        json_input: None,
        compression: None,
        csv_output: None,
        csv_output_header: None,
        json_output: None,
        enc_c: Vec::new(),
    };

    let mut targets = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--query" | "-e" => {
                let v = args.get(i + 1).ok_or("--query expects a value")?;
                opts.query = v.to_string();
                i += 2;
            }
            "--recursive" | "-r" => {
                opts.recursive = true;
                i += 1;
            }
            "--csv-input" => {
                let v = args.get(i + 1).ok_or("--csv-input expects a value")?;
                opts.csv_input = Some(v.to_string());
                i += 2;
            }
            "--json-input" => {
                let v = args.get(i + 1).ok_or("--json-input expects a value")?;
                opts.json_input = Some(v.to_string());
                i += 2;
            }
            "--compression" => {
                let v = args.get(i + 1).ok_or("--compression expects a value")?;
                opts.compression = Some(v.to_string());
                i += 2;
            }
            "--csv-output" => {
                let v = args.get(i + 1).ok_or("--csv-output expects a value")?;
                opts.csv_output = Some(v.to_string());
                i += 2;
            }
            "--csv-output-header" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--csv-output-header expects a value")?;
                opts.csv_output_header = Some(v.to_string());
                i += 2;
            }
            "--json-output" => {
                let v = args.get(i + 1).ok_or("--json-output expects a value")?;
                opts.json_output = Some(v.to_string());
                i += 2;
            }
            "--enc-c" => {
                let v = args.get(i + 1).ok_or("--enc-c expects a value")?;
                opts.enc_c.push(v.to_string());
                i += 2;
            }
            f if f.starts_with('-') => return Err(format!("unknown sql flag: {f}")),
            _ => {
                targets.push(parse_target(&args[i])?);
                i += 1;
            }
        }
    }

    if targets.is_empty() {
        return Err("usage: s4 sql [FLAGS] <alias/bucket/key|prefix> [TARGET...]".to_string());
    }

    Ok((opts, targets))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn parse_kv_options(spec: &str) -> HashMap<String, String> {
    spec.split(',')
        .filter_map(|item| {
            let (k, v) = item.split_once('=')?;
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

fn map_csv_input(spec: &str) -> String {
    let kv = parse_kv_options(spec);
    let mut out = String::new();
    out.push_str("<CSV><FileHeaderInfo>");
    out.push_str(kv.get("fh").map(|v| v.as_str()).unwrap_or("NONE"));
    out.push_str("</FileHeaderInfo>");
    if let Some(v) = kv.get("fd") {
        out.push_str("<FieldDelimiter>");
        out.push_str(&xml_escape(v));
        out.push_str("</FieldDelimiter>");
    }
    if let Some(v) = kv.get("rd") {
        out.push_str("<RecordDelimiter>");
        out.push_str(&xml_escape(v));
        out.push_str("</RecordDelimiter>");
    }
    out.push_str("</CSV>");
    out
}

fn map_json_input(spec: &str) -> String {
    let kv = parse_kv_options(spec);
    let typ = kv.get("t").map(|v| v.as_str()).unwrap_or("DOCUMENT");
    format!("<JSON><Type>{}</Type></JSON>", xml_escape(typ))
}

fn map_csv_output(spec: Option<&str>, header: Option<&str>) -> String {
    let kv = spec.map(parse_kv_options).unwrap_or_default();
    let mut out = String::new();
    out.push_str("<CSV>");
    if let Some(v) = kv.get("fd") {
        out.push_str("<FieldDelimiter>");
        out.push_str(&xml_escape(v));
        out.push_str("</FieldDelimiter>");
    }
    if let Some(v) = kv.get("rd") {
        out.push_str("<RecordDelimiter>");
        out.push_str(&xml_escape(v));
        out.push_str("</RecordDelimiter>");
    }
    if let Some(v) = header {
        out.push_str("<QuoteFields>");
        if v.is_empty() {
            out.push_str("ASNEEDED");
        } else {
            out.push_str("ALWAYS");
        }
        out.push_str("</QuoteFields>");
    }
    out.push_str("</CSV>");
    out
}

fn map_json_output(spec: Option<&str>) -> String {
    let kv = spec.map(parse_kv_options).unwrap_or_default();
    let mut out = String::new();
    out.push_str("<JSON>");
    if let Some(v) = kv.get("rd") {
        out.push_str("<RecordDelimiter>");
        out.push_str(&xml_escape(v));
        out.push_str("</RecordDelimiter>");
    }
    out.push_str("</JSON>");
    out
}

fn build_select_request_xml(opts: &SqlOptions) -> String {
    let input = if let Some(csv) = &opts.csv_input {
        map_csv_input(csv)
    } else if let Some(json) = &opts.json_input {
        map_json_input(json)
    } else {
        "<CSV><FileHeaderInfo>NONE</FileHeaderInfo></CSV>".to_string()
    };

    let output = if opts.json_output.is_some() {
        map_json_output(opts.json_output.as_deref())
    } else {
        map_csv_output(
            opts.csv_output.as_deref(),
            opts.csv_output_header.as_deref(),
        )
    };

    let compression = opts.compression.as_deref().unwrap_or("NONE").to_string();

    format!(
        "<SelectObjectContentRequest><Expression>{}</Expression><ExpressionType>SQL</ExpressionType><InputSerialization>{}<CompressionType>{}</CompressionType></InputSerialization><OutputSerialization>{}</OutputSerialization></SelectObjectContentRequest>",
        xml_escape(&opts.query),
        input,
        xml_escape(&compression),
        output
    )
}

fn s3_request_bytes_with_headers(
    alias: &AliasConfig,
    method: &str,
    bucket: &str,
    key: Option<&str>,
    query: &str,
    upload_file: Option<&Path>,
    extra_headers: &[String],
    debug: bool,
) -> Result<Vec<u8>, String> {
    let endpoint = parse_endpoint(&alias.endpoint)?;
    let mut uri_path = endpoint.base_path.clone();

    if alias.path_style {
        if !bucket.is_empty() {
            uri_path.push('/');
            uri_path.push_str(&uri_encode_segment(bucket));
        }
        if let Some(k) = key {
            uri_path.push('/');
            uri_path.push_str(&uri_encode_path(k));
        }
    } else {
        return Err("only --path-style aliases are supported in this build".to_string());
    }
    if uri_path.is_empty() {
        uri_path = "/".to_string();
    }

    let canonical_query = normalize_sigv4_query(query);
    let payload_hash = payload_hash(upload_file)?;
    let sign = sign_v4(
        method,
        &uri_path,
        &canonical_query,
        &endpoint.host,
        &alias.region,
        &alias.access_key,
        &alias.secret_key,
        &payload_hash,
    )?;

    let mut url = format!("{}://{}{}", endpoint.scheme, endpoint.host, uri_path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let body_path = env::temp_dir().join(format!("s4-body-{}-{}", std::process::id(), ts));

    let mut cmd = Command::new("curl");
    apply_curl_global_flags(&mut cmd, upload_file.is_some(), true);
    cmd.arg("-sS")
        .arg("-X")
        .arg(method)
        .arg(&url)
        .arg("-H")
        .arg(format!("Host: {}", endpoint.host))
        .arg("-H")
        .arg(format!("x-amz-date: {}", sign.amz_date))
        .arg("-H")
        .arg(format!("x-amz-content-sha256: {}", payload_hash))
        .arg("-H")
        .arg(format!("Authorization: {}", sign.authorization));
    for header in extra_headers {
        cmd.arg("-H").arg(header);
    }
    if let Some(file) = upload_file {
        cmd.arg("--data-binary").arg(format!("@{}", file.display()));
    }
    cmd.arg("-o")
        .arg(&body_path)
        .arg("-w")
        .arg("HTTPSTATUS:%{http_code}");

    if debug {
        eprintln!("[debug] request(bytes): {} {}", method, url);
    }

    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let _ = fs::remove_file(&body_path);
        return Err(format!(
            "request execution failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let status_text = String::from_utf8_lossy(&out.stdout).to_string();
    let status = status_text.trim().strip_prefix("HTTPSTATUS:").unwrap_or("");
    let body = fs::read(&body_path).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&body_path);

    if !status.starts_with('2') {
        return Err(format!("request failed with status {}", status));
    }
    Ok(body)
}

fn parse_event_stream_records(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 16 <= data.len() {
        let total_len =
            u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let headers_len =
            u32::from_be_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
        if total_len == 0 || i + total_len > data.len() || 12 + headers_len + 4 > total_len {
            break;
        }
        let headers_start = i + 12;
        let payload_start = headers_start + headers_len;
        let payload_end = i + total_len - 4;
        if payload_start > payload_end || payload_end > data.len() {
            break;
        }
        let headers = &data[headers_start..payload_start];
        let payload = &data[payload_start..payload_end];

        let mut event_type: Option<String> = None;
        let mut j = 0usize;
        while j < headers.len() {
            if j + 2 > headers.len() {
                break;
            }
            let nlen = headers[j] as usize;
            j += 1;
            if j + nlen + 1 > headers.len() {
                break;
            }
            let name = String::from_utf8_lossy(&headers[j..j + nlen]).to_string();
            j += nlen;
            let htype = headers[j];
            j += 1;
            match htype {
                7 => {
                    if j + 2 > headers.len() {
                        break;
                    }
                    let slen = u16::from_be_bytes([headers[j], headers[j + 1]]) as usize;
                    j += 2;
                    if j + slen > headers.len() {
                        break;
                    }
                    let val = String::from_utf8_lossy(&headers[j..j + slen]).to_string();
                    j += slen;
                    if name == ":event-type" {
                        event_type = Some(val);
                    }
                }
                _ => break,
            }
        }

        if matches!(event_type.as_deref(), Some("Records")) {
            out.extend_from_slice(payload);
        }
        i += total_len;
    }
    if out.is_empty() {
        out.extend_from_slice(data);
    }
    out
}

fn cmd_sql(
    config: &AppConfig,
    opts: &SqlOptions,
    targets: &[S3Target],
    json: bool,
    debug: bool,
) -> Result<(), String> {
    let request_xml = build_select_request_xml(opts);
    let temp_xml = env::temp_dir().join(format!("s4-sql-{}-req.xml", std::process::id()));
    fs::write(&temp_xml, request_xml).map_err(|e| e.to_string())?;

    for target in targets {
        let alias = config
            .aliases
            .get(&target.alias)
            .ok_or_else(|| format!("unknown alias: {}", target.alias))?;
        let bucket = req_bucket(target, "sql")?;

        let keys: Vec<String> = if opts.recursive {
            let prefix = target.key.clone().unwrap_or_default();
            list_object_keys(alias, &bucket, &prefix, debug)?
        } else {
            vec![req_key(target, "sql")?]
        };

        for key in keys {
            let body = s3_request_bytes_with_headers(
                alias,
                "POST",
                &bucket,
                Some(&key),
                "select&select-type=2",
                Some(&temp_xml),
                &[],
                debug,
            )?;
            let records = parse_event_stream_records(&body);
            if json {
                println!(
                    "{{\"bucket\":\"{}\",\"key\":\"{}\",\"records\":\"{}\"}}",
                    escape_json(&bucket),
                    escape_json(&key),
                    escape_json(&String::from_utf8_lossy(&records))
                );
            } else {
                print!("{}", String::from_utf8_lossy(&records));
            }
        }
    }

    let _ = fs::remove_file(&temp_xml);
    Ok(())
}

fn parse_sync_args(args: &[String]) -> Result<(SyncOptions, S3Target, S3Target), String> {
    if args.len() < 3 {
        return Err(
            "usage: s4 sync|mirror [FLAGS] <src_alias/bucket[/prefix]> <dst_alias/bucket[/prefix]>"
                .to_string(),
        );
    }

    let mut opts = SyncOptions::default();
    let mut positional: Vec<&String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--overwrite" => {
                opts.overwrite = true;
                i += 1;
            }
            "--dry-run" => {
                opts.dry_run = true;
                i += 1;
            }
            "--remove" => {
                opts.remove = true;
                i += 1;
            }
            "--exclude" => {
                let value = args.get(i + 1).ok_or("--exclude expects a value")?;
                opts.excludes.push(value.to_string());
                i += 2;
            }
            "--newer-than" => {
                let value = args.get(i + 1).ok_or("--newer-than expects a value")?;
                opts.newer_than = Some(parse_human_duration(value)?);
                i += 2;
            }
            "--older-than" => {
                let value = args.get(i + 1).ok_or("--older-than expects a value")?;
                opts.older_than = Some(parse_human_duration(value)?);
                i += 2;
            }
            "--watch" | "-w" => {
                opts.watch = true;
                i += 1;
            }
            f if f.starts_with('-') => {
                return Err(format!("sync/mirror flag not implemented yet: {f}"));
            }
            _ => {
                positional.push(&args[i]);
                i += 1;
            }
        }
    }

    if positional.len() != 2 {
        return Err(
            "usage: s4 sync|mirror [FLAGS] <src_alias/bucket[/prefix]> <dst_alias/bucket[/prefix]>"
                .to_string(),
        );
    }

    let src = parse_target(positional[0])?;
    let dst = parse_target(positional[1])?;
    Ok((opts, src, dst))
}

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

fn parse_human_duration(input: &str) -> Result<u64, String> {
    if input.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let mut total = 0u64;
    let mut value = 0u64;
    let mut has_unit = false;
    for c in input.chars() {
        if c.is_ascii_digit() {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add((c as u8 - b'0') as u64))
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
        total = total
            .checked_add(
                value
                    .checked_mul(unit)
                    .ok_or_else(|| "duration overflow".to_string())?,
            )
            .ok_or_else(|| "duration overflow".to_string())?;
        value = 0;
        has_unit = true;
    }
    if !has_unit || value != 0 {
        return Err("duration must end with unit (d/h/m/s)".to_string());
    }
    Ok(total)
}

fn object_age_seconds(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    debug: bool,
) -> Result<Option<u64>, String> {
    let headers = s3_request(alias, "HEAD", bucket, Some(key), "", None, None, debug)?;
    let mut last_modified: Option<String> = None;
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("last-modified:") {
            if let Some((_, value)) = line.split_once(':') {
                last_modified = Some(value.trim().to_string());
                break;
            }
        }
    }
    let Some(last_modified) = last_modified else {
        return Ok(None);
    };
    let out = Command::new("python3")
        .arg("-c")
        .arg(
            "import sys,time,email.utils; dt=email.utils.parsedate_to_datetime(sys.argv[1]); print(int(time.time()-dt.timestamp()))",
        )
        .arg(&last_modified)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "failed to parse Last-Modified header: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let age = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    Ok(Some(age))
}

fn watch_interval() -> Duration {
    let seconds = env::var("S4_SYNC_WATCH_INTERVAL_SEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2);
    Duration::from_secs(seconds.max(1))
}

fn cmd_sync_once(
    src_alias: &AliasConfig,
    dst_alias: &AliasConfig,
    source: &S3Target,
    destination: &S3Target,
    options: &SyncOptions,
    json: bool,
    debug: bool,
) -> Result<(usize, usize), String> {
    let src_bucket = req_bucket(source, "sync")?;
    let dst_bucket = req_bucket(destination, "sync")?;
    let src_prefix = source.key.clone().unwrap_or_default();
    let dst_prefix = destination.key.clone().unwrap_or_default();

    let keys = list_object_keys(src_alias, &src_bucket, &src_prefix, debug)?;
    let mut filtered_keys: Vec<String> = Vec::new();
    for key in keys {
        if is_excluded(&key, &options.excludes) {
            continue;
        }
        if options.newer_than.is_some() || options.older_than.is_some() {
            let age = object_age_seconds(src_alias, &src_bucket, &key, debug)?;
            let Some(age) = age else {
                continue;
            };
            if let Some(newer_than) = options.newer_than {
                if age > newer_than {
                    continue;
                }
            }
            if let Some(older_than) = options.older_than {
                if age < older_than {
                    continue;
                }
            }
        }
        filtered_keys.push(key);
    }

    let mut copied = 0usize;
    let mut removed = 0usize;

    if options.dry_run {
        for key in &filtered_keys {
            let dest_key = sync_destination_key(key, &src_prefix, &dst_prefix);
            if !json {
                println!(
                    "[dry-run] copy {}/{} -> {}/{}",
                    src_bucket, key, dst_bucket, dest_key
                );
            }
            copied += 1;
        }
    } else {
        let temp_root = env::temp_dir().join(format!("s4-sync-{}", std::process::id()));
        fs::create_dir_all(&temp_root).map_err(|e| e.to_string())?;

        for (idx, key) in filtered_keys.iter().enumerate() {
            let dest_key = sync_destination_key(key, &src_prefix, &dst_prefix);
            let temp_file = temp_root.join(format!("obj-{idx}"));
            s3_request(
                src_alias,
                "GET",
                &src_bucket,
                Some(key),
                "",
                None,
                Some(&temp_file),
                debug,
            )?;
            upload_file_to_s3(dst_alias, &dst_bucket, &dest_key, &temp_file, debug)?;
            copied += 1;
        }

        fs::remove_dir_all(&temp_root).ok();
    }

    if options.remove {
        let dst_keys = list_object_keys(dst_alias, &dst_bucket, &dst_prefix, debug)?;
        let expected: HashSet<String> = filtered_keys
            .iter()
            .map(|k| sync_destination_key(k, &src_prefix, &dst_prefix))
            .collect();
        for key in dst_keys {
            if !expected.contains(&key) {
                if options.dry_run {
                    if !json {
                        println!("[dry-run] remove {}/{}", dst_bucket, key);
                    }
                } else {
                    s3_request(
                        dst_alias,
                        "DELETE",
                        &dst_bucket,
                        Some(&key),
                        "",
                        None,
                        None,
                        debug,
                    )?;
                }
                removed += 1;
            }
        }
    }

    Ok((copied, removed))
}

fn cmd_sync(
    config: &AppConfig,
    source: &S3Target,
    destination: &S3Target,
    options: &SyncOptions,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    let src_alias = config
        .aliases
        .get(&source.alias)
        .ok_or_else(|| format!("unknown alias: {}", source.alias))?;
    let dst_alias = config
        .aliases
        .get(&destination.alias)
        .ok_or_else(|| format!("unknown alias: {}", destination.alias))?;

    loop {
        let (copied, removed) = cmd_sync_once(
            src_alias,
            dst_alias,
            source,
            destination,
            options,
            json,
            debug,
        )?;

        let src_bucket = req_bucket(source, "sync")?;
        let dst_bucket = req_bucket(destination, "sync")?;

        if json {
            println!(
                "{{\"status\":\"ok\",\"copied\":{},\"removed\":{},\"dry_run\":{},\"watch\":{},\"src\":\"{}\",\"dst\":\"{}\"}}",
                copied,
                removed,
                options.dry_run,
                options.watch,
                escape_json(&format!("{}/{}", source.alias, src_bucket)),
                escape_json(&format!("{}/{}", destination.alias, dst_bucket))
            );
        } else {
            println!(
                "Synced {} object(s) from {}/{} to {}/{} (removed: {}, dry-run: {}, watch: {})",
                copied,
                source.alias,
                src_bucket,
                destination.alias,
                dst_bucket,
                removed,
                options.dry_run,
                options.watch
            );
        }

        if !options.watch {
            break;
        }
        sleep(watch_interval());
    }

    Ok(())
}

fn cmd_cp_mv(
    command: &str,
    config: &AppConfig,
    source: &str,
    target: &str,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    let src = classify_ref(config, source);
    let dst = classify_ref(config, target);

    match (&src, &dst) {
        (ObjectRef::Local(src_path), ObjectRef::S3(dst_s3)) => {
            let body_path = PathBuf::from(src_path);
            if !body_path.exists() {
                return Err(format!("source file not found: {}", body_path.display()));
            }
            upload_file_to_s3(
                &dst_s3.alias,
                &dst_s3.bucket,
                &dst_s3.key,
                &body_path,
                debug,
            )?;
            if command == "mv" {
                fs::remove_file(&body_path).map_err(|e| e.to_string())?;
            }
        }
        (ObjectRef::S3(src_s3), ObjectRef::Local(dst_path)) => {
            let out = PathBuf::from(dst_path);
            if let Some(parent) = out.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
            }
            s3_request(
                &src_s3.alias,
                "GET",
                &src_s3.bucket,
                Some(&src_s3.key),
                "",
                None,
                Some(&out),
                debug,
            )?;
            if command == "mv" {
                s3_request(
                    &src_s3.alias,
                    "DELETE",
                    &src_s3.bucket,
                    Some(&src_s3.key),
                    "",
                    None,
                    None,
                    debug,
                )?;
            }
        }
        (ObjectRef::S3(src_s3), ObjectRef::S3(dst_s3)) => {
            copy_object_s3_to_s3(src_s3, dst_s3, debug)?;
            if command == "mv" {
                s3_request(
                    &src_s3.alias,
                    "DELETE",
                    &src_s3.bucket,
                    Some(&src_s3.key),
                    "",
                    None,
                    None,
                    debug,
                )?;
            }
        }
        (ObjectRef::Local(src_path), ObjectRef::Local(dst_path)) => {
            fs::copy(src_path, dst_path).map_err(|e| e.to_string())?;
            if command == "mv" {
                fs::remove_file(src_path).map_err(|e| e.to_string())?;
            }
        }
    }

    if json {
        println!(
            "{{\"status\":\"ok\",\"command\":\"{}\",\"source\":\"{}\",\"target\":\"{}\"}}",
            escape_json(command),
            escape_json(source),
            escape_json(target)
        );
    } else {
        println!("{}: {} -> {}", command, source, target);
    }
    Ok(())
}

#[derive(Clone)]
struct S3ObjectRef {
    alias: AliasConfig,
    bucket: String,
    key: String,
}

enum ObjectRef {
    S3(S3ObjectRef),
    Local(String),
}

fn classify_ref(config: &AppConfig, value: &str) -> ObjectRef {
    if let Ok(t) = parse_target(value) {
        if let Some(alias) = config.aliases.get(&t.alias) {
            if let (Some(bucket), Some(key)) = (t.bucket, t.key) {
                return ObjectRef::S3(S3ObjectRef {
                    alias: alias.clone(),
                    bucket,
                    key,
                });
            }
        }
    }
    ObjectRef::Local(value.to_string())
}

fn copy_object_s3_to_s3(src: &S3ObjectRef, dst: &S3ObjectRef, debug: bool) -> Result<(), String> {
    let copy_source = format!(
        "/{}/{}",
        uri_encode_segment(&src.bucket),
        uri_encode_path(&src.key)
    );
    let headers = vec![format!("x-amz-copy-source: {}", copy_source)];
    s3_request_with_headers(
        &dst.alias,
        "PUT",
        &dst.bucket,
        Some(&dst.key),
        "",
        None,
        None,
        &headers,
        debug,
    )?;
    Ok(())
}

fn cmd_find(
    alias: &AliasConfig,
    bucket: &str,
    prefix: &str,
    needle: Option<&str>,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    let keys = list_object_keys(alias, bucket, prefix, debug)?;
    for key in keys {
        if let Some(n) = needle {
            if !key.contains(n) {
                continue;
            }
        }
        if json {
            println!(
                "{{\"bucket\":\"{}\",\"key\":\"{}\"}}",
                escape_json(bucket),
                escape_json(&key)
            );
        } else {
            println!("{}", key);
        }
    }
    Ok(())
}

fn cmd_tree(
    alias: &AliasConfig,
    bucket: &str,
    prefix: &str,
    _json: bool,
    debug: bool,
) -> Result<(), String> {
    let mut keys = list_object_keys(alias, bucket, prefix, debug)?;
    keys.sort();
    println!("{}/", bucket);
    for key in keys {
        let depth = key.matches('/').count();
        let indent = "  ".repeat(depth + 1);
        let name = key.rsplit('/').next().unwrap_or(&key);
        println!("{}{}", indent, name);
    }
    Ok(())
}

fn cmd_head(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    lines: usize,
    debug: bool,
) -> Result<(), String> {
    let body = s3_request(alias, "GET", bucket, Some(key), "", None, None, debug)?;
    for line in body.lines().take(lines) {
        println!("{}", line);
    }
    Ok(())
}

fn cmd_ping(alias_name: &str, alias: &AliasConfig, json: bool, debug: bool) -> Result<(), String> {
    let start = Instant::now();
    let _ = s3_request(alias, "GET", "", None, "", None, None, debug)?;
    let ms = start.elapsed().as_millis();

    if json {
        println!(
            "{{\"alias\":\"{}\",\"status\":\"ok\",\"latency_ms\":{}}}",
            escape_json(alias_name),
            ms
        );
    } else {
        println!("{} is alive ({} ms)", alias_name, ms);
    }
    Ok(())
}

fn looks_ready_xml(body: &str) -> bool {
    body.contains("<ListAllMyBucketsResult") || body.contains("<Error")
}

fn cmd_ready(alias_name: &str, alias: &AliasConfig, json: bool, debug: bool) -> Result<(), String> {
    let body = s3_request(alias, "GET", "", None, "", None, None, debug)?;
    if !looks_ready_xml(&body) {
        return Err("ready check got unexpected response body".to_string());
    }

    if json {
        println!(
            "{{\"alias\":\"{}\",\"ready\":true}}",
            escape_json(alias_name)
        );
    } else {
        println!("{} is ready", alias_name);
    }
    Ok(())
}

fn cmd_pipe(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    json: bool,
    debug: bool,
) -> Result<(), String> {
    let mut stdin_bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut stdin_bytes)
        .map_err(|e| e.to_string())?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temp_path = env::temp_dir().join(format!("s4-pipe-{}-{}", std::process::id(), ts));
    fs::write(&temp_path, &stdin_bytes).map_err(|e| e.to_string())?;

    let upload_result = upload_file_to_s3(alias, bucket, key, &temp_path, debug);
    let _ = fs::remove_file(&temp_path);
    upload_result?;

    if json {
        println!(
            "{{\"uploaded\":{{\"bucket\":\"{}\",\"key\":\"{}\",\"source\":\"stdin\"}}}}",
            escape_json(bucket),
            escape_json(key)
        );
    } else {
        println!("Uploaded STDIN to '{}/{}'", bucket, key);
    }
    Ok(())
}

fn cmd_ls(alias: &AliasConfig, target: &S3Target, json: bool, debug: bool) -> Result<(), String> {
    match &target.bucket {
        None => {
            let body = s3_request(alias, "GET", "", None, "", None, None, debug)?;
            if json {
                println!("{{\"xml\":\"{}\"}}", escape_json(&body));
            } else {
                println!("{body}");
            }
        }
        Some(bucket) => {
            let body = s3_request(alias, "GET", bucket, None, "list-type=2", None, None, debug)?;
            if json {
                println!("{{\"xml\":\"{}\"}}", escape_json(&body));
            } else {
                println!("{body}");
            }
        }
    }
    Ok(())
}

fn list_object_keys(
    alias: &AliasConfig,
    bucket: &str,
    prefix: &str,
    debug: bool,
) -> Result<Vec<String>, String> {
    let mut keys = Vec::new();
    let mut continuation: Option<String> = None;

    loop {
        let mut query = String::from("list-type=2");
        if !prefix.is_empty() {
            query.push_str("&prefix=");
            query.push_str(&uri_encode_path(prefix));
        }
        if let Some(token) = continuation.as_ref() {
            query.push_str("&continuation-token=");
            query.push_str(&uri_encode_path(token));
        }

        let body = s3_request(alias, "GET", bucket, None, &query, None, None, debug)?;
        keys.extend(
            extract_tag_values(&body, "Key")
                .into_iter()
                .map(|k| xml_unescape(&k)),
        );

        let is_truncated = extract_tag_values(&body, "IsTruncated")
            .into_iter()
            .next()
            .unwrap_or_else(|| "false".to_string())
            .trim()
            .eq("true");

        if is_truncated {
            continuation = extract_tag_values(&body, "NextContinuationToken")
                .into_iter()
                .next()
                .map(|v| xml_unescape(&v));
            if continuation.is_none() {
                break;
            }
        } else {
            break;
        }
    }

    Ok(keys)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObjectVersion {
    key: String,
    version_id: String,
}

fn list_object_versions(
    alias: &AliasConfig,
    bucket: &str,
    debug: bool,
) -> Result<Vec<ObjectVersion>, String> {
    let mut versions = Vec::new();
    let mut key_marker: Option<String> = None;
    let mut version_id_marker: Option<String> = None;

    loop {
        let mut query = String::from("versions=");
        if let Some(marker) = key_marker.as_ref() {
            query.push_str("&key-marker=");
            query.push_str(&uri_encode_query_component(marker));
        }
        if let Some(marker) = version_id_marker.as_ref() {
            query.push_str("&version-id-marker=");
            query.push_str(&uri_encode_query_component(marker));
        }

        let body = s3_request(alias, "GET", bucket, None, &query, None, None, debug)?;
        versions.extend(extract_version_entries(&body, "Version"));
        versions.extend(extract_version_entries(&body, "DeleteMarker"));

        let is_truncated = extract_tag_values(&body, "IsTruncated")
            .into_iter()
            .next()
            .unwrap_or_else(|| "false".to_string())
            .trim()
            .eq("true");

        if !is_truncated {
            break;
        }

        key_marker = extract_tag_values(&body, "NextKeyMarker")
            .into_iter()
            .next()
            .map(|v| xml_unescape(&v));
        version_id_marker = extract_tag_values(&body, "NextVersionIdMarker")
            .into_iter()
            .next()
            .map(|v| xml_unescape(&v));

        if key_marker.is_none() {
            break;
        }
    }

    Ok(versions)
}

fn extract_tag_blocks(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");

    let mut out = Vec::new();
    let mut remaining = xml;

    while let Some(start) = remaining.find(&open) {
        let after_open = &remaining[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        out.push(after_open[..end].to_string());
        remaining = &after_open[end + close.len()..];
    }

    out
}

fn extract_version_entries(xml: &str, tag: &str) -> Vec<ObjectVersion> {
    let mut out = Vec::new();
    for block in extract_tag_blocks(xml, tag) {
        let key = extract_tag_values(&block, "Key")
            .into_iter()
            .next()
            .map(|v| xml_unescape(&v));
        let version_id = extract_tag_values(&block, "VersionId")
            .into_iter()
            .next()
            .map(|v| xml_unescape(&v));
        if let (Some(key), Some(version_id)) = (key, version_id) {
            out.push(ObjectVersion { key, version_id });
        }
    }
    out
}

fn purge_bucket_versions(alias: &AliasConfig, bucket: &str, debug: bool) -> Result<(), String> {
    for entry in list_object_versions(alias, bucket, debug)? {
        let query = format!(
            "versionId={}",
            uri_encode_query_component(&entry.version_id)
        );
        match s3_request(
            alias,
            "DELETE",
            bucket,
            Some(&entry.key),
            &query,
            None,
            None,
            debug,
        ) {
            Ok(_) => {}
            Err(err) if should_retry_with_governance_bypass(&err) => {
                let headers = vec!["x-amz-bypass-governance-retention: true".to_string()];
                s3_request_with_headers(
                    alias,
                    "DELETE",
                    bucket,
                    Some(&entry.key),
                    &query,
                    None,
                    None,
                    &headers,
                    debug,
                )?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn sync_destination_key(source_key: &str, src_prefix: &str, dst_prefix: &str) -> String {
    let normalized_src = src_prefix.trim_matches('/');
    let mut relative = source_key.to_string();

    if !normalized_src.is_empty() {
        if source_key == normalized_src {
            relative.clear();
        } else if let Some(rest) = source_key.strip_prefix(&(normalized_src.to_string() + "/")) {
            relative = rest.to_string();
        }
    }

    let normalized_dst = dst_prefix.trim_matches('/');
    if normalized_dst.is_empty() {
        return relative;
    }
    if relative.is_empty() {
        return normalized_dst.to_string();
    }

    format!("{normalized_dst}/{relative}")
}

fn extract_tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");

    let mut out = Vec::new();
    let mut remaining = xml;

    while let Some(start) = remaining.find(&open) {
        let after_open = &remaining[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        out.push(after_open[..end].to_string());
        remaining = &after_open[end + close.len()..];
    }

    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn should_retry_with_governance_bypass(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("accessdenied")
        || lower.contains("retention")
        || lower.contains("governance")
        || (lower.contains("invalidrequest") && lower.contains("worm"))
        || lower.contains("worm protected")
}

fn req_bucket(target: &S3Target, cmd: &str) -> Result<String, String> {
    target
        .bucket
        .clone()
        .ok_or_else(|| format!("{cmd} requires alias/bucket"))
}

fn req_key(target: &S3Target, cmd: &str) -> Result<String, String> {
    target
        .key
        .clone()
        .ok_or_else(|| format!("{cmd} requires alias/bucket/key"))
}

fn normalize_sigv4_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    query
        .split('&')
        .map(|part| {
            if part.is_empty() {
                String::new()
            } else if part.contains('=') {
                part.to_string()
            } else {
                format!("{}=", part)
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn s3_request(
    alias: &AliasConfig,
    method: &str,
    bucket: &str,
    key: Option<&str>,
    query: &str,
    upload_file: Option<&Path>,
    output_file: Option<&Path>,
    debug: bool,
) -> Result<String, String> {
    s3_request_with_headers(
        alias,
        method,
        bucket,
        key,
        query,
        upload_file,
        output_file,
        &[],
        debug,
    )
}

fn normalize_resolve_entry(entry: &str) -> String {
    if entry.contains('=') {
        entry.replacen('=', ":", 1)
    } else {
        entry.to_string()
    }
}

fn apply_curl_global_flags(cmd: &mut Command, is_upload: bool, is_download: bool) {
    if CURL_INSECURE.load(Ordering::Relaxed) {
        cmd.arg("-k");
    }
    if let Ok(opts) = curl_global_opts().lock() {
        for resolve in &opts.resolve {
            cmd.arg("--resolve").arg(normalize_resolve_entry(resolve));
        }
        if is_upload {
            if let Some(limit_upload) = &opts.limit_upload {
                cmd.arg("--limit-rate").arg(limit_upload);
            }
        } else if is_download {
            if let Some(limit_download) = &opts.limit_download {
                cmd.arg("--limit-rate").arg(limit_download);
            }
        }
        for header in &opts.custom_headers {
            cmd.arg("-H").arg(header);
        }
    }
}

fn s3_request_with_headers(
    alias: &AliasConfig,
    method: &str,
    bucket: &str,
    key: Option<&str>,
    query: &str,
    upload_file: Option<&Path>,
    output_file: Option<&Path>,
    extra_headers: &[String],
    debug: bool,
) -> Result<String, String> {
    let endpoint = parse_endpoint(&alias.endpoint)?;
    let mut uri_path = endpoint.base_path.clone();

    if alias.path_style {
        if !bucket.is_empty() {
            uri_path.push('/');
            uri_path.push_str(&uri_encode_segment(bucket));
        }
        if let Some(k) = key {
            uri_path.push('/');
            uri_path.push_str(&uri_encode_path(k));
        }
    } else {
        return Err("only --path-style aliases are supported in this build".to_string());
    }

    if uri_path.is_empty() {
        uri_path = "/".to_string();
    }

    let canonical_query = normalize_sigv4_query(query);
    let payload_hash = payload_hash(upload_file)?;
    let sign = sign_v4(
        method,
        &uri_path,
        &canonical_query,
        &endpoint.host,
        &alias.region,
        &alias.access_key,
        &alias.secret_key,
        &payload_hash,
    )?;

    let mut url = format!("{}://{}{}", endpoint.scheme, endpoint.host, uri_path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }

    let mut cmd = Command::new("curl");
    apply_curl_global_flags(&mut cmd, upload_file.is_some(), output_file.is_some());
    cmd.arg("-sS").arg(&url);
    if method != "HEAD" {
        cmd.arg("-X").arg(method);
    }
    cmd.arg("-H")
        .arg(format!("Host: {}", endpoint.host))
        .arg("-H")
        .arg(format!("x-amz-date: {}", sign.amz_date))
        .arg("-H")
        .arg(format!("x-amz-content-sha256: {}", payload_hash))
        .arg("-H")
        .arg(format!("Authorization: {}", sign.authorization));

    for header in extra_headers {
        cmd.arg("-H").arg(header);
    }

    if let Some(file) = upload_file {
        cmd.arg("--data-binary").arg(format!("@{}", file.display()));
    }

    if method == "HEAD" {
        // Use curl native HEAD mode instead of `-X HEAD` + body suppression.
        // This avoids curl(18) "transfer closed with bytes remaining" on servers
        // that return Content-Length for HEAD responses.
        cmd.arg("-I");
    } else if let Some(out) = output_file {
        cmd.arg("-o").arg(out);
    }

    if debug {
        eprintln!("[debug] request: {} {}", method, url);
    }

    cmd.arg("-w").arg("\nHTTPSTATUS:%{http_code}");

    let output = cmd.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("request execution failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let (body, status_part) = stdout
        .rsplit_once("\nHTTPSTATUS:")
        .ok_or_else(|| "unable to parse HTTP status".to_string())?;
    let status = status_part.trim();
    if !status.starts_with('2') {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!(
            "request failed with status {status}: body='{}' stderr='{}'",
            body.trim(),
            stderr.trim()
        ));
    }

    Ok(body.to_string())
}

fn sign_v4(
    method: &str,
    uri_path: &str,
    query: &str,
    host: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    payload_hash: &str,
) -> Result<SignatureParts, String> {
    let py = r#"
import sys, hmac, hashlib, datetime
method, path, query, host, region, access, secret, payload_hash = sys.argv[1:]
service = 's3'
amz_date = datetime.datetime.utcnow().strftime('%Y%m%dT%H%M%SZ')
date_stamp = amz_date[:8]
canonical_headers = f'host:{host}\n' + f'x-amz-content-sha256:{payload_hash}\n' + f'x-amz-date:{amz_date}\n'
signed_headers = 'host;x-amz-content-sha256;x-amz-date'
canonical_request = '\n'.join([method, path, query, canonical_headers, signed_headers, payload_hash])
algorithm = 'AWS4-HMAC-SHA256'
credential_scope = f'{date_stamp}/{region}/{service}/aws4_request'
string_to_sign = '\n'.join([algorithm, amz_date, credential_scope, hashlib.sha256(canonical_request.encode()).hexdigest()])
def sign(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()
k_date = sign(('AWS4' + secret).encode(), date_stamp)
k_region = sign(k_date, region)
k_service = sign(k_region, service)
k_signing = sign(k_service, 'aws4_request')
signature = hmac.new(k_signing, string_to_sign.encode(), hashlib.sha256).hexdigest()
auth = f'{algorithm} Credential={access}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}'
print(amz_date)
print(auth)
"#;

    let out = Command::new("python3")
        .arg("-c")
        .arg(py)
        .arg(method)
        .arg(uri_path)
        .arg(query)
        .arg(host)
        .arg(region)
        .arg(access_key)
        .arg(secret_key)
        .arg(payload_hash)
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }

    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(ToString::to_string)
        .collect();
    if lines.len() < 2 {
        return Err("signature helper returned unexpected output".to_string());
    }

    Ok(SignatureParts {
        amz_date: lines[0].clone(),
        authorization: lines[1].clone(),
    })
}

fn payload_hash(upload_file: Option<&Path>) -> Result<String, String> {
    if let Some(path) = upload_file {
        let out = Command::new("python3")
            .arg("-c")
            .arg("import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())")
            .arg(path)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Ok("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string())
    }
}

const MULTIPART_THRESHOLD_BYTES: u64 = 16 * 1024 * 1024;
const MULTIPART_PART_SIZE_BYTES: usize = 8 * 1024 * 1024;

fn upload_file_to_s3(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    path: &Path,
    debug: bool,
) -> Result<(), String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size < MULTIPART_THRESHOLD_BYTES {
        s3_request(alias, "PUT", bucket, Some(key), "", Some(path), None, debug)?;
        return Ok(());
    }

    multipart_upload_file(alias, bucket, key, path, debug)
}

fn multipart_upload_file(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    path: &Path,
    debug: bool,
) -> Result<(), String> {
    let init_xml = s3_request(
        alias,
        "POST",
        bucket,
        Some(key),
        "uploads",
        None,
        None,
        debug,
    )?;
    let upload_id = extract_tag_values(&init_xml, "UploadId")
        .into_iter()
        .next()
        .map(|v| xml_unescape(&v))
        .ok_or_else(|| "multipart init did not return UploadId".to_string())?;

    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut part_number = 1usize;
    let mut etags: Vec<(usize, String)> = Vec::new();

    loop {
        let mut chunk = vec![0u8; MULTIPART_PART_SIZE_BYTES];
        let n = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        chunk.truncate(n);

        let temp_part = env::temp_dir().join(format!(
            "s4-mpu-part-{}-{}-{}",
            std::process::id(),
            part_number,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        ));
        fs::write(&temp_part, &chunk).map_err(|e| e.to_string())?;

        let uploaded = upload_part(
            alias,
            bucket,
            key,
            &upload_id,
            part_number,
            &temp_part,
            debug,
        );
        let _ = fs::remove_file(&temp_part);
        let etag = match uploaded {
            Ok(v) => v,
            Err(e) => {
                let _ = abort_multipart(alias, bucket, key, &upload_id, debug);
                return Err(e);
            }
        };

        etags.push((part_number, etag));
        part_number += 1;
    }

    if etags.is_empty() {
        let _ = abort_multipart(alias, bucket, key, &upload_id, debug);
        return Err("multipart upload had no parts".to_string());
    }

    let complete_xml = build_complete_multipart_xml(&etags);
    let complete_path = env::temp_dir().join(format!(
        "s4-mpu-complete-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    fs::write(&complete_path, complete_xml).map_err(|e| e.to_string())?;

    let query = format!("uploadId={}", uri_encode_query_component(&upload_id));
    let complete_res = s3_request(
        alias,
        "POST",
        bucket,
        Some(key),
        &query,
        Some(&complete_path),
        None,
        debug,
    );
    let _ = fs::remove_file(&complete_path);

    if let Err(err) = complete_res {
        let _ = abort_multipart(alias, bucket, key, &upload_id, debug);
        return Err(err);
    }

    Ok(())
}

fn upload_part(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    upload_id: &str,
    part_number: usize,
    file_path: &Path,
    debug: bool,
) -> Result<String, String> {
    let endpoint = parse_endpoint(&alias.endpoint)?;
    let mut uri_path = endpoint.base_path.clone();
    if !bucket.is_empty() {
        uri_path.push('/');
        uri_path.push_str(&uri_encode_segment(bucket));
    }
    uri_path.push('/');
    uri_path.push_str(&uri_encode_path(key));

    let query = format!(
        "partNumber={}&uploadId={}",
        part_number,
        uri_encode_query_component(upload_id)
    );
    let payload_hash = payload_hash(Some(file_path))?;
    let sign = sign_v4(
        "PUT",
        &uri_path,
        &query,
        &endpoint.host,
        &alias.region,
        &alias.access_key,
        &alias.secret_key,
        &payload_hash,
    )?;

    let url = format!(
        "{}://{}{}?{}",
        endpoint.scheme, endpoint.host, uri_path, query
    );
    let mut cmd = Command::new("curl");
    apply_curl_global_flags(&mut cmd, true, false);
    cmd.arg("-sS")
        .arg("-X")
        .arg("PUT")
        .arg(&url)
        .arg("-H")
        .arg(format!("Host: {}", endpoint.host))
        .arg("-H")
        .arg(format!("x-amz-date: {}", sign.amz_date))
        .arg("-H")
        .arg(format!("x-amz-content-sha256: {}", payload_hash))
        .arg("-H")
        .arg(format!("Authorization: {}", sign.authorization))
        .arg("--data-binary")
        .arg(format!("@{}", file_path.display()))
        .arg("-D")
        .arg("-")
        .arg("-o")
        .arg("/dev/null")
        .arg("-w")
        .arg(
            "
HTTPSTATUS:%{http_code}",
        );

    if debug {
        eprintln!("[debug] multipart upload part request: PUT {}", url);
    }

    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "multipart part request execution failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let (headers, status_part) = stdout
        .rsplit_once(
            "
HTTPSTATUS:",
        )
        .ok_or_else(|| "unable to parse multipart part status".to_string())?;
    let status = status_part.trim();
    if !status.starts_with('2') {
        return Err(format!("multipart part failed with status {}", status));
    }

    for line in headers.lines() {
        let l = line.trim();
        if l.to_ascii_lowercase().starts_with("etag:") {
            let v = l
                .split_once(':')
                .map(|(_, r)| r.trim().trim_matches('"').to_string())
                .unwrap_or_default();
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    Err("multipart part response missing ETag".to_string())
}

fn abort_multipart(
    alias: &AliasConfig,
    bucket: &str,
    key: &str,
    upload_id: &str,
    debug: bool,
) -> Result<(), String> {
    let query = format!("uploadId={}", uri_encode_query_component(upload_id));
    let _ = s3_request(
        alias,
        "DELETE",
        bucket,
        Some(key),
        &query,
        None,
        None,
        debug,
    )?;
    Ok(())
}

fn build_complete_multipart_xml(etags: &[(usize, String)]) -> String {
    let mut out = String::from("<CompleteMultipartUpload>");
    for (part, etag) in etags {
        out.push_str("<Part>");
        out.push_str(&format!("<PartNumber>{}</PartNumber>", part));
        out.push_str(&format!("<ETag>\"{}\"</ETag>", escape_xml(etag)));
        out.push_str("</Part>");
    }
    out.push_str("</CompleteMultipartUpload>");
    out
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn uri_encode_query_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn parse_endpoint(raw: &str) -> Result<Endpoint, String> {
    let (scheme, rest) = if let Some(v) = raw.strip_prefix("http://") {
        ("http", v)
    } else if let Some(v) = raw.strip_prefix("https://") {
        ("https", v)
    } else {
        return Err("endpoint must start with http:// or https://".to_string());
    };

    let mut parts = rest.splitn(2, '/');
    let host = parts.next().unwrap_or("").to_string();
    if host.is_empty() {
        return Err("endpoint host is empty".to_string());
    }
    let base_path = match parts.next() {
        Some(v) if !v.is_empty() => format!("/{}", v.trim_end_matches('/')),
        _ => "".to_string(),
    };

    Ok(Endpoint {
        scheme: scheme.to_string(),
        host,
        base_path,
    })
}

fn resolve_config_path(custom_dir: Option<&Path>) -> Result<PathBuf, String> {
    match custom_dir {
        Some(p) => Ok(p.join("config.toml")),
        None => {
            let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
            Ok(PathBuf::from(home).join(".s4").join("config.toml"))
        }
    }
}

fn load_config(path: &Path) -> Result<AppConfig, String> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }

    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut s = String::new();
    file.read_to_string(&mut s).map_err(|e| e.to_string())?;
    parse_config(&s)
}

fn save_config(path: &Path, cfg: &AppConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let text = serialize_config(cfg);
    fs::write(path, text).map_err(|e| e.to_string())
}

fn parse_config(text: &str) -> Result<AppConfig, String> {
    let mut cfg = AppConfig::default();
    for (ln, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 6 {
            return Err(format!("invalid config at line {}", ln + 1));
        }
        cfg.aliases.insert(
            parts[0].to_string(),
            AliasConfig {
                endpoint: parts[1].to_string(),
                access_key: parts[2].to_string(),
                secret_key: parts[3].to_string(),
                region: parts[4].to_string(),
                path_style: parts[5] == "1",
            },
        );
    }
    Ok(cfg)
}

fn serialize_config(cfg: &AppConfig) -> String {
    let mut out = String::new();
    for (name, a) in &cfg.aliases {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            name,
            a.endpoint,
            a.access_key,
            a.secret_key,
            a.region,
            if a.path_style { "1" } else { "0" }
        ));
    }
    out
}

fn parse_target(input: &str) -> Result<S3Target, String> {
    let mut parts = input.splitn(3, '/');
    let alias = parts
        .next()
        .ok_or_else(|| "target must start with alias".to_string())?
        .to_string();
    if alias.is_empty() {
        return Err("target alias is empty".to_string());
    }
    let bucket = parts.next().map(ToString::to_string);
    let key = parts.next().map(ToString::to_string);
    Ok(S3Target { alias, bucket, key })
}

fn uri_encode_segment(s: &str) -> String {
    uri_encode_path(s)
}

fn uri_encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' || c == '/' {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn print_status(json: bool, field: &str, value: &str) {
    if json {
        println!("{{\"{}\":\"{}\"}}", escape_json(field), escape_json(value));
    } else {
        println!("{field}: {value}");
    }
}

fn print_help() {
    println!(
        "s4 - S3 client utility in Rust

USAGE:
  s4 [FLAGS] COMMAND [ARGS]

COMMANDS:
  alias      manage aliases in local config
  ls         list buckets/objects
  mb         make bucket
  rb         remove bucket
  legalhold  manage legal hold for object(s) (set/clear/info)
  retention  manage retention for object(s) (set/clear/info)
  sql        run SQL queries on objects
  replicate  manage server-side bucket replication [placeholder]
  put        upload object
  get        download object
  rm         remove object
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

#[cfg(test)]
mod tests {
    use super::{
        AliasConfig, AppConfig, CorsCommand, EncryptCommand, EventCommand, IdpKind, IlmKind,
        LegalHoldCommand, ReplicateSubcommand, RetentionCommand, build_complete_multipart_xml,
        build_select_request_xml, extract_tag_blocks, extract_tag_values, extract_version_entries,
        is_excluded, looks_ready_xml, normalize_resolve_entry, normalize_sigv4_query, parse_config,
        parse_cors_args, parse_encrypt_args, parse_event_args, parse_event_stream_records,
        parse_globals, parse_human_duration, parse_idp_args, parse_ilm_args, parse_legalhold_args,
        parse_replicate_args, parse_retention_args, parse_sql_args, parse_sync_args, parse_target,
        serialize_config, should_retry_with_governance_bypass, sync_destination_key,
        uri_encode_path, uri_encode_query_component, wildcard_match, xml_unescape,
    };
    use std::collections::BTreeMap;

    #[test]
    fn parse_target_with_key() {
        let t = parse_target("local/bucket/folder/file.txt").expect("target should parse");
        assert_eq!(t.alias, "local");
        assert_eq!(t.bucket.as_deref(), Some("bucket"));
        assert_eq!(t.key.as_deref(), Some("folder/file.txt"));
    }

    #[test]
    fn roundtrip_config() {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "local".to_string(),
            AliasConfig {
                endpoint: "http://127.0.0.1:9000".to_string(),
                access_key: "minio".to_string(),
                secret_key: "minio123".to_string(),
                region: "us-east-1".to_string(),
                path_style: true,
            },
        );
        let cfg = AppConfig { aliases };

        let text = serialize_config(&cfg);
        let parsed = parse_config(&text).expect("config should parse");
        assert_eq!(parsed.aliases.len(), 1);
        let alias = parsed.aliases.get("local").expect("alias exists");
        assert!(alias.path_style);
        assert_eq!(alias.region, "us-east-1");
    }

    #[test]
    fn uri_encode_works() {
        assert_eq!(uri_encode_path("a b/c"), "a%20b/c");
    }

    #[test]
    fn extract_tag_blocks_works() {
        let xml =
            "<Root><Version><Key>a.txt</Key></Version><Version><Key>b.txt</Key></Version></Root>";
        let blocks = extract_tag_blocks(xml, "Version");
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("<Key>a.txt</Key>"));
        assert!(blocks[1].contains("<Key>b.txt</Key>"));
    }

    #[test]
    fn extract_version_entries_works_for_versions_and_delete_markers() {
        let xml = "<ListVersionsResult><Version><Key>k1</Key><VersionId>v1</VersionId></Version><DeleteMarker><Key>k2</Key><VersionId>v2</VersionId></DeleteMarker></ListVersionsResult>";
        let versions = extract_version_entries(xml, "Version");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].key, "k1");
        assert_eq!(versions[0].version_id, "v1");

        let delete_markers = extract_version_entries(xml, "DeleteMarker");
        assert_eq!(delete_markers.len(), 1);
        assert_eq!(delete_markers[0].key, "k2");
        assert_eq!(delete_markers[0].version_id, "v2");
    }

    #[test]
    fn extract_xml_keys() {
        let xml = "<ListBucketResult><Contents><Key>a.txt</Key></Contents><Contents><Key>dir/b.txt</Key></Contents></ListBucketResult>";
        let keys = extract_tag_values(xml, "Key");
        assert_eq!(keys, vec!["a.txt".to_string(), "dir/b.txt".to_string()]);
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
    fn governance_bypass_retry_matches_worm_and_retention_errors() {
        assert!(should_retry_with_governance_bypass("AccessDenied"));
        assert!(should_retry_with_governance_bypass("retention policy"));
        assert!(should_retry_with_governance_bypass("governance mode"));
        assert!(should_retry_with_governance_bypass(
            "InvalidRequest: Object is WORM protected and cannot be overwritten"
        ));
        assert!(!should_retry_with_governance_bypass("NoSuchBucket"));
    }

    #[test]
    fn xml_unescape_works() {
        assert_eq!(xml_unescape("a&amp;b&quot;c"), "a&b\"c");
    }

    #[test]
    fn looks_ready_xml_accepts_known_payloads() {
        assert!(looks_ready_xml(
            "<ListAllMyBucketsResult></ListAllMyBucketsResult>"
        ));
        assert!(looks_ready_xml("<Error><Code>AccessDenied</Code></Error>"));
        assert!(!looks_ready_xml("not-xml"));
    }

    #[test]
    fn build_complete_multipart_xml_contains_parts() {
        let xml =
            build_complete_multipart_xml(&[(1, "etag-1".to_string()), (2, "etag-2".to_string())]);
        assert!(xml.contains("<PartNumber>1</PartNumber>"));
        assert!(xml.contains("<ETag>\"etag-2\"</ETag>"));
    }

    #[test]
    fn normalize_sigv4_query_adds_empty_values_for_subresources() {
        assert_eq!(normalize_sigv4_query("cors"), "cors=");
        assert_eq!(normalize_sigv4_query("uploads"), "uploads=");
        assert_eq!(
            normalize_sigv4_query("list-type=2&prefix=a"),
            "list-type=2&prefix=a"
        );
    }

    #[test]
    fn normalize_resolve_entry_supports_equals_and_colon_formats() {
        assert_eq!(
            normalize_resolve_entry("minio.local:9000=127.0.0.1"),
            "minio.local:9000:127.0.0.1"
        );
        assert_eq!(
            normalize_resolve_entry("minio.local:9000:127.0.0.1"),
            "minio.local:9000:127.0.0.1"
        );
    }

    #[test]
    fn uri_encode_query_component_works() {
        assert_eq!(uri_encode_query_component("a b/+"), "a%20b%2F%2B");
    }

    #[test]
    fn wildcard_match_works() {
        assert!(wildcard_match("*.tmp", "a.tmp"));
        assert!(wildcard_match("foo/*/bar", "foo/x/bar"));
        assert!(!wildcard_match("*.tmp", "a.txt"));
    }

    #[test]
    fn parse_sync_args_with_flags() {
        let args = vec![
            "mirror".to_string(),
            "--dry-run".to_string(),
            "--remove".to_string(),
            "-w".to_string(),
            "--exclude".to_string(),
            "*.tmp".to_string(),
            "a/src/prefix".to_string(),
            "b/dst/prefix".to_string(),
        ];
        let (opts, src, dst) = parse_sync_args(&args).expect("sync args should parse");
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
        let args = vec![
            "sync".to_string(),
            "--newer-than".to_string(),
            "10d".to_string(),
            "--older-than".to_string(),
            "1h".to_string(),
            "a/src".to_string(),
            "b/dst".to_string(),
        ];
        let (opts, _, _) = parse_sync_args(&args).expect("sync args should parse");
        assert!(!opts.watch);
        assert_eq!(opts.newer_than, Some(864000));
        assert_eq!(opts.older_than, Some(3600));
    }

    #[test]
    fn parse_cors_args_set_works() {
        let args = vec![
            "cors".to_string(),
            "set".to_string(),
            "a/bucket".to_string(),
            "cors.xml".to_string(),
        ];
        let parsed = parse_cors_args(&args).expect("cors args should parse");
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
        let args = vec![
            "cors".to_string(),
            "get".to_string(),
            "a/bucket".to_string(),
        ];
        let parsed = parse_cors_args(&args).expect("cors args should parse");
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
        let args = vec![
            "encrypt".to_string(),
            "set".to_string(),
            "a/bucket".to_string(),
            "enc.xml".to_string(),
        ];
        let parsed = parse_encrypt_args(&args).expect("encrypt args should parse");
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
        let args = vec![
            "encrypt".to_string(),
            "info".to_string(),
            "a/bucket".to_string(),
        ];
        let parsed = parse_encrypt_args(&args).expect("encrypt args should parse");
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
        let args = vec![
            "event".to_string(),
            "add".to_string(),
            "a/bucket".to_string(),
            "event.xml".to_string(),
        ];
        let parsed = parse_event_args(&args).expect("event args should parse");
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
        let args = vec![
            "event".to_string(),
            "rm".to_string(),
            "a/bucket".to_string(),
            "--force".to_string(),
        ];
        let parsed = parse_event_args(&args).expect("event args should parse");
        match parsed {
            EventCommand::Remove { target, force } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("bucket"));
                assert!(force);
            }
            _ => panic!("expected event remove"),
        }
    }

    #[test]
    fn parse_idp_args_openid_works() {
        let args = vec!["idp".to_string(), "openid".to_string()];
        let parsed = parse_idp_args(&args).expect("idp args should parse");
        match parsed.kind {
            IdpKind::OpenId => {}
            _ => panic!("expected openid"),
        }
    }

    #[test]
    fn parse_idp_args_ldap_works() {
        let args = vec!["idp".to_string(), "ldap".to_string()];
        let parsed = parse_idp_args(&args).expect("idp args should parse");
        match parsed.kind {
            IdpKind::Ldap => {}
            _ => panic!("expected ldap"),
        }
    }

    #[test]
    fn parse_ilm_args_rule_works() {
        let args = vec!["ilm".to_string(), "rule".to_string()];
        let parsed = parse_ilm_args(&args).expect("ilm args should parse");
        match parsed.kind {
            IlmKind::Rule => {}
            _ => panic!("expected rule"),
        }
    }

    #[test]
    fn parse_ilm_args_restore_works() {
        let args = vec!["ilm".to_string(), "restore".to_string()];
        let parsed = parse_ilm_args(&args).expect("ilm args should parse");
        match parsed.kind {
            IlmKind::Restore => {}
            _ => panic!("expected restore"),
        }
    }

    #[test]
    fn parse_legalhold_args_set_works() {
        let args = vec![
            "legalhold".to_string(),
            "set".to_string(),
            "a/b/k".to_string(),
        ];
        let parsed = parse_legalhold_args(&args).expect("legalhold args should parse");
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
        let args = vec![
            "legalhold".to_string(),
            "info".to_string(),
            "a/b/k".to_string(),
        ];
        let parsed = parse_legalhold_args(&args).expect("legalhold args should parse");
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
    fn parse_replicate_args_list_alias_works() {
        let args = vec![
            "replicate".to_string(),
            "ls".to_string(),
            "a/bucket".to_string(),
        ];
        let parsed = parse_replicate_args(&args).expect("replicate args should parse");
        match parsed.subcommand {
            ReplicateSubcommand::List => {}
            _ => panic!("expected list"),
        }
        let target = parsed.target.expect("target expected");
        assert_eq!(target.alias, "a");
        assert_eq!(target.bucket.as_deref(), Some("bucket"));
    }

    #[test]
    fn parse_replicate_args_backlog_works() {
        let args = vec!["replicate".to_string(), "backlog".to_string()];
        let parsed = parse_replicate_args(&args).expect("replicate args should parse");
        match parsed.subcommand {
            ReplicateSubcommand::Backlog => {}
            _ => panic!("expected backlog"),
        }
    }

    #[test]
    fn parse_retention_args_set_works() {
        let args = vec![
            "retention".to_string(),
            "set".to_string(),
            "a/b/k".to_string(),
            "--mode".to_string(),
            "GOVERNANCE".to_string(),
            "--retain-until".to_string(),
            "2030-01-01T00:00:00Z".to_string(),
        ];
        let parsed = parse_retention_args(&args).expect("retention args should parse");
        match parsed {
            RetentionCommand::Set {
                target,
                mode,
                retain_until,
            } => {
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
    fn parse_retention_args_info_works() {
        let args = vec![
            "retention".to_string(),
            "info".to_string(),
            "a/b/k".to_string(),
        ];
        let parsed = parse_retention_args(&args).expect("retention args should parse");
        match parsed {
            RetentionCommand::Info { target } => {
                assert_eq!(target.alias, "a");
                assert_eq!(target.bucket.as_deref(), Some("b"));
                assert_eq!(target.key.as_deref(), Some("k"));
            }
            _ => panic!("expected retention info"),
        }
    }

    #[test]
    fn parse_sql_args_defaults_and_targets() {
        let args = vec!["sql".to_string(), "a/bucket/path.csv".to_string()];
        let (opts, targets) = parse_sql_args(&args).expect("sql args should parse");
        assert_eq!(opts.query, "select * from S3Object");
        assert!(!opts.recursive);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].alias, "a");
        assert_eq!(targets[0].bucket.as_deref(), Some("bucket"));
        assert_eq!(targets[0].key.as_deref(), Some("path.csv"));
    }

    #[test]
    fn parse_sql_args_full_flags() {
        let args = vec![
            "sql".to_string(),
            "--query".to_string(),
            "select count(*) from S3Object".to_string(),
            "-r".to_string(),
            "--csv-input".to_string(),
            "fh=USE,fd=;".to_string(),
            "--compression".to_string(),
            "GZIP".to_string(),
            "--csv-output".to_string(),
            "fd=;".to_string(),
            "--csv-output-header".to_string(),
            "c1,c2".to_string(),
            "--enc-c".to_string(),
            "a/bucket=Zm9v".to_string(),
            "a/bucket/prefix".to_string(),
        ];
        let (opts, targets) = parse_sql_args(&args).expect("sql args should parse");
        assert_eq!(opts.query, "select count(*) from S3Object");
        assert!(opts.recursive);
        assert_eq!(opts.csv_input.as_deref(), Some("fh=USE,fd=;"));
        assert_eq!(opts.compression.as_deref(), Some("GZIP"));
        assert_eq!(opts.csv_output.as_deref(), Some("fd=;"));
        assert_eq!(opts.csv_output_header.as_deref(), Some("c1,c2"));
        assert_eq!(opts.enc_c, vec!["a/bucket=Zm9v".to_string()]);
        assert_eq!(targets[0].key.as_deref(), Some("prefix"));
    }

    #[test]
    fn build_select_request_xml_contains_query_and_serialization() {
        let args = vec![
            "sql".to_string(),
            "--query".to_string(),
            "select * from S3Object".to_string(),
            "--json-output".to_string(),
            "rd=\n".to_string(),
            "a/b/k".to_string(),
        ];
        let (opts, _) = parse_sql_args(&args).expect("sql args should parse");
        let xml = build_select_request_xml(&opts);
        assert!(xml.contains("<Expression>select * from S3Object</Expression>"));
        assert!(xml.contains("<ExpressionType>SQL</ExpressionType>"));
        assert!(xml.contains("<JSON>"));
    }

    #[test]
    fn parse_event_stream_records_returns_payload_for_records_event() {
        fn mk_header(name: &str, value: &str) -> Vec<u8> {
            let mut h = Vec::new();
            h.push(name.len() as u8);
            h.extend_from_slice(name.as_bytes());
            h.push(7);
            h.extend_from_slice(&(value.len() as u16).to_be_bytes());
            h.extend_from_slice(value.as_bytes());
            h
        }
        let payload = b"row1,row2\n";
        let mut headers = Vec::new();
        headers.extend_from_slice(&mk_header(":message-type", "event"));
        headers.extend_from_slice(&mk_header(":event-type", "Records"));

        let total_len = 12 + headers.len() + payload.len() + 4;
        let mut msg = Vec::new();
        msg.extend_from_slice(&(total_len as u32).to_be_bytes());
        msg.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        msg.extend_from_slice(&[0, 0, 0, 0]);
        msg.extend_from_slice(&headers);
        msg.extend_from_slice(payload);
        msg.extend_from_slice(&[0, 0, 0, 0]);

        let out = parse_event_stream_records(&msg);
        assert_eq!(out, payload);
    }
    #[test]
    fn parse_globals_extended_flags() {
        let (opts, rest) = parse_globals(vec![
            "--insecure".to_string(),
            "--resolve".to_string(),
            "minio.local:9000=127.0.0.1".to_string(),
            "--limit-upload".to_string(),
            "1M".to_string(),
            "--limit-download".to_string(),
            "2M".to_string(),
            "-H".to_string(),
            "x-test: one".to_string(),
            "--custom-header".to_string(),
            "x-test2: two".to_string(),
            "ls".to_string(),
            "a/b".to_string(),
        ])
        .expect("parse globals should succeed");
        assert!(opts.insecure);
        assert_eq!(opts.resolve, vec!["minio.local:9000=127.0.0.1".to_string()]);
        assert_eq!(opts.limit_upload.as_deref(), Some("1M"));
        assert_eq!(opts.limit_download.as_deref(), Some("2M"));
        assert_eq!(
            opts.custom_headers,
            vec!["x-test: one".to_string(), "x-test2: two".to_string()]
        );
        assert_eq!(rest, vec!["ls".to_string(), "a/b".to_string()]);
    }
}
