//! `sql`: S3 Select (SelectObjectContent) over one or more objects.

use std::collections::HashMap;
use std::fs;

use super::Context;
use crate::cli::flag_value;
use crate::output::JsonObject;
use crate::s3::eventstream::parse_event_stream_records;
use crate::s3::{Request, xml};
use crate::target::S3Target;
use crate::util::TempPath;

#[derive(Debug, Clone)]
pub struct SqlOptions {
    pub query: String,
    pub recursive: bool,
    pub csv_input: Option<String>,
    pub json_input: Option<String>,
    pub compression: Option<String>,
    pub csv_output: Option<String>,
    pub csv_output_header: Option<String>,
    pub json_output: Option<String>,
}

impl Default for SqlOptions {
    fn default() -> Self {
        Self {
            query: "select * from S3Object".to_string(),
            recursive: false,
            csv_input: None,
            json_input: None,
            compression: None,
            csv_output: None,
            csv_output_header: None,
            json_output: None,
        }
    }
}

pub fn run(ctx: &Context, args: &[String]) -> Result<(), String> {
    let (opts, targets) = parse_sql_args(args)?;
    let request_xml = TempPath::new("sql")?;
    fs::write(request_xml.path(), build_select_request_xml(&opts)).map_err(|e| e.to_string())?;

    // mc prints the custom CSV header once, above all records.
    let mut header = opts
        .csv_output_header
        .as_deref()
        .filter(|h| !h.is_empty() && opts.json_output.is_none())
        .map(|h| format!("{h}\n"));

    for target in &targets {
        let client = ctx.client_for(target)?;
        let bucket = target.require_bucket("sql")?;
        let keys = if opts.recursive {
            client.list_object_keys(bucket, target.prefix())?
        } else {
            vec![target.require_key("sql")?.to_string()]
        };

        for key in keys {
            let body = client.send_bytes(
                &Request::new("POST", bucket)
                    .key(&key)
                    .subresource("select")
                    .param("select-type", "2")
                    .body(request_xml.path())
                    .header("Content-Type", "application/xml"),
            )?;
            let records = parse_event_stream_records(&body)?;
            let records = format!(
                "{}{}",
                header.take().unwrap_or_default(),
                String::from_utf8_lossy(&records)
            );
            if ctx.json {
                println!(
                    "{}",
                    JsonObject::new()
                        .str("bucket", bucket)
                        .str("key", &key)
                        .str("records", &records)
                );
            } else {
                print!("{records}");
            }
        }
    }
    Ok(())
}

pub fn parse_sql_args(args: &[String]) -> Result<(SqlOptions, Vec<S3Target>), String> {
    let mut opts = SqlOptions::default();
    let mut targets = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        let slot = match flag {
            "--recursive" | "-r" => {
                opts.recursive = true;
                i += 1;
                continue;
            }
            "--query" | "-e" => {
                opts.query = flag_value(args, i, "--query")?.to_string();
                i += 2;
                continue;
            }
            // SSE-C for S3 Select is not supported yet; fail instead of ignoring the key.
            "--enc-c" => return Err(format!("sql flag not implemented yet: {flag}")),
            "--csv-input" => &mut opts.csv_input,
            "--json-input" => &mut opts.json_input,
            "--compression" => &mut opts.compression,
            "--csv-output" => &mut opts.csv_output,
            "--csv-output-header" => &mut opts.csv_output_header,
            "--json-output" => &mut opts.json_output,
            f if f.starts_with('-') => return Err(format!("unknown sql flag: {f}")),
            _ => {
                targets.push(S3Target::parse(&args[i])?);
                i += 1;
                continue;
            }
        };
        *slot = Some(flag_value(args, i, flag)?.to_string());
        i += 2;
    }

    if targets.is_empty() {
        return Err("usage: s4 sql [FLAGS] <alias/bucket/key|prefix> [TARGET...]".to_string());
    }
    Ok((opts, targets))
}

/// Parses mc-style `k=v,k=v` serialization options.
fn parse_kv_options(spec: &str) -> HashMap<String, String> {
    spec.split(',')
        .filter_map(|item| {
            let (k, v) = item.split_once('=')?;
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

/// Appends `<Tag>value</Tag>` when `value` is present.
fn push_element(out: &mut String, tag: &str, value: Option<&String>) {
    if let Some(v) = value {
        out.push_str(&format!("<{tag}>{}</{tag}>", xml::escape(v)));
    }
}

fn map_csv_input(spec: &str) -> String {
    let kv = parse_kv_options(spec);
    let mut out = String::from("<CSV><FileHeaderInfo>");
    out.push_str(kv.get("fh").map(String::as_str).unwrap_or("NONE"));
    out.push_str("</FileHeaderInfo>");
    push_element(&mut out, "FieldDelimiter", kv.get("fd"));
    push_element(&mut out, "RecordDelimiter", kv.get("rd"));
    out.push_str("</CSV>");
    out
}

fn map_json_input(spec: &str) -> String {
    let kv = parse_kv_options(spec);
    let typ = kv.get("t").map(String::as_str).unwrap_or("DOCUMENT");
    format!("<JSON><Type>{}</Type></JSON>", xml::escape(typ))
}

fn map_csv_output(spec: Option<&str>) -> String {
    let kv = spec.map(parse_kv_options).unwrap_or_default();
    let mut out = String::from("<CSV>");
    push_element(&mut out, "FieldDelimiter", kv.get("fd"));
    push_element(&mut out, "RecordDelimiter", kv.get("rd"));
    out.push_str("</CSV>");
    out
}

fn map_json_output(spec: Option<&str>) -> String {
    let kv = spec.map(parse_kv_options).unwrap_or_default();
    let mut out = String::from("<JSON>");
    push_element(&mut out, "RecordDelimiter", kv.get("rd"));
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
        map_csv_output(opts.csv_output.as_deref())
    };

    let compression = opts.compression.as_deref().unwrap_or("NONE");

    format!(
        "<SelectObjectContentRequest><Expression>{}</Expression><ExpressionType>SQL</ExpressionType><InputSerialization>{}<CompressionType>{}</CompressionType></InputSerialization><OutputSerialization>{}</OutputSerialization></SelectObjectContentRequest>",
        xml::escape(&opts.query),
        input,
        xml::escape(compression),
        output
    )
}

#[cfg(test)]
mod tests {
    use super::{build_select_request_xml, parse_sql_args};
    use crate::cli::args;

    #[test]
    fn parse_sql_args_defaults_and_targets() {
        let (opts, targets) =
            parse_sql_args(&args(&["sql", "a/bucket/path.csv"])).expect("sql args should parse");
        assert_eq!(opts.query, "select * from S3Object");
        assert!(!opts.recursive);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].alias, "a");
        assert_eq!(targets[0].bucket.as_deref(), Some("bucket"));
        assert_eq!(targets[0].key.as_deref(), Some("path.csv"));
    }

    #[test]
    fn parse_sql_args_full_flags() {
        let (opts, targets) = parse_sql_args(&args(&[
            "sql",
            "--query",
            "select count(*) from S3Object",
            "-r",
            "--csv-input",
            "fh=USE,fd=;",
            "--compression",
            "GZIP",
            "--csv-output",
            "fd=;",
            "--csv-output-header",
            "c1,c2",
            "a/bucket/prefix",
        ]))
        .expect("sql args should parse");
        assert_eq!(opts.query, "select count(*) from S3Object");
        assert!(opts.recursive);
        assert_eq!(opts.csv_input.as_deref(), Some("fh=USE,fd=;"));
        assert_eq!(opts.compression.as_deref(), Some("GZIP"));
        assert_eq!(opts.csv_output.as_deref(), Some("fd=;"));
        assert_eq!(opts.csv_output_header.as_deref(), Some("c1,c2"));
        assert_eq!(targets[0].key.as_deref(), Some("prefix"));
    }

    #[test]
    fn parse_sql_args_rejects_unsupported_sse_c() {
        let err = parse_sql_args(&args(&["sql", "--enc-c", "a/b=Zm9v", "a/b/k"]))
            .expect_err("--enc-c must not be ignored");
        assert!(err.contains("--enc-c"), "{err}");
    }

    #[test]
    fn build_select_request_xml_contains_query_and_serialization() {
        let (opts, _) = parse_sql_args(&args(&[
            "sql",
            "--query",
            "select * from S3Object",
            "--json-output",
            "rd=\n",
            "a/b/k",
        ]))
        .expect("sql args should parse");
        let xml = build_select_request_xml(&opts);
        assert!(xml.contains("<Expression>select * from S3Object</Expression>"));
        assert!(xml.contains("<ExpressionType>SQL</ExpressionType>"));
        assert!(xml.contains("<JSON>"));
    }
}
