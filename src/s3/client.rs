//! Signed S3 requests executed through curl.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Output, Stdio};

use super::endpoint::Endpoint;
use super::http::HttpOptions;
use super::sign::{
    CanonicalRequest, add_signed_header, authorization, canonical_query, payload_hash, url_query,
};
use crate::config::AliasConfig;
use crate::time;
use crate::util::{PartialFile, TempPath};

/// Largest error body kept from a streamed response.
const MAX_ERROR_BODY: u64 = 64 * 1024;

/// One S3 request: method, path-style target, query parameters, optional body file and headers.
pub struct Request<'a> {
    method: &'a str,
    bucket: &'a str,
    key: Option<&'a str>,
    params: Vec<(String, String)>,
    body: Option<&'a Path>,
    headers: Vec<(String, String)>,
}

impl<'a> Request<'a> {
    /// An empty `bucket` targets the service root (e.g. ListBuckets).
    pub fn new(method: &'a str, bucket: &'a str) -> Self {
        Self {
            method,
            bucket,
            key: None,
            params: Vec::new(),
            body: None,
            headers: Vec::new(),
        }
    }

    pub fn key(mut self, key: &'a str) -> Self {
        self.key = Some(key);
        self
    }

    /// A query parameter; name and value are percent-encoded when sent.
    pub fn param(mut self, name: &str, value: impl Into<String>) -> Self {
        self.params.push((name.to_string(), value.into()));
        self
    }

    /// A valueless query parameter such as `?cors` or `?uploads`.
    pub fn subresource(self, name: &str) -> Self {
        self.param(name, "")
    }

    pub fn body(mut self, path: &'a Path) -> Self {
        self.body = Some(path);
        self
    }

    /// `x-amz-*` headers are included in the signature.
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn bypass_governance(self) -> Self {
        self.header("x-amz-bypass-governance-retention", "true")
    }
}

/// Where the response body goes.
enum Sink<'p> {
    /// Returned as text; for HEAD requests this is the response headers.
    Capture,
    File(&'p Path),
    /// Body is discarded and the response headers are returned instead.
    Headers,
}

/// A signed request ready to hand to curl.
struct Prepared {
    url: String,
    /// `Name: value` lines fed to curl on stdin, keeping credentials out of argv.
    header_lines: Vec<String>,
}

/// Executes requests against one alias.
pub struct S3Client<'a> {
    alias: &'a AliasConfig,
    http: &'a HttpOptions,
    debug: bool,
}

impl<'a> S3Client<'a> {
    pub fn new(alias: &'a AliasConfig, http: &'a HttpOptions, debug: bool) -> Self {
        Self { alias, http, debug }
    }

    pub fn alias(&self) -> &'a AliasConfig {
        self.alias
    }

    /// Sends the request and returns the response body as (lossy UTF-8) text.
    pub fn send(&self, req: &Request) -> Result<String, String> {
        self.execute(req, Sink::Capture)
    }

    /// Sends the request and writes the response body to `to`. `to` is replaced
    /// only after a successful response, so errors never clobber an existing file.
    pub fn download(&self, req: &Request, to: &Path) -> Result<(), String> {
        let partial = PartialFile::create(to)?;
        self.execute(req, Sink::File(partial.path()))?;
        partial.commit()
    }

    /// Sends the request and returns the raw response body.
    pub fn send_bytes(&self, req: &Request) -> Result<Vec<u8>, String> {
        let body = TempPath::new("body")?;
        self.execute(req, Sink::File(body.path()))?;
        fs::read(body.path()).map_err(|e| e.to_string())
    }

    /// Sends the request and returns the raw response headers.
    pub fn response_headers(&self, req: &Request) -> Result<String, String> {
        self.execute(req, Sink::Headers)
    }

    /// Starts the request and, once a 2xx status arrives, returns a reader that
    /// streams the response body without buffering it in memory or on disk.
    pub fn open(&self, req: &Request) -> Result<BodyReader, String> {
        let prepared = self.prepare(req)?;
        let mut cmd = self.curl(req, &prepared);
        // Response headers precede the body on stdout, so the status is known
        // before any body byte is handed out.
        cmd.arg("-D").arg("-").arg("--suppress-connect-headers");
        let mut child = self.spawn(cmd, req.method, &prepared)?;
        let stdout = child.stdout.take().expect("curl stdout is piped");
        let mut body = BodyReader {
            child,
            reader: BufReader::new(stdout),
            done: false,
        };

        match read_final_status(&mut body.reader) {
            Ok(Some(status)) if status.starts_with('2') => Ok(body),
            Ok(Some(status)) => {
                let mut error_body = Vec::new();
                let _ = (&mut body.reader)
                    .take(MAX_ERROR_BODY)
                    .read_to_end(&mut error_body);
                let stderr = body.abort();
                Err(format!(
                    "request failed with status {status}: body='{}' stderr='{}'",
                    String::from_utf8_lossy(&error_body).trim(),
                    stderr.trim()
                ))
            }
            Ok(None) | Err(_) => Err(format!("request execution failed: {}", body.abort().trim())),
        }
    }

    fn prepare(&self, req: &Request) -> Result<Prepared, String> {
        let endpoint = Endpoint::parse(&self.alias.endpoint)?;
        if !self.alias.path_style {
            return Err("only --path-style aliases are supported in this build".to_string());
        }
        let uri_path = endpoint.object_path(req.bucket, req.key);
        let payload_hash = payload_hash(req.body)?;
        let amz_date = time::amz_date(time::now_secs());

        let custom_headers = self
            .http
            .custom_headers
            .iter()
            .filter_map(|line| line.split_once(':'));
        let extra_headers = req
            .headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .chain(custom_headers);

        let mut signed = BTreeMap::new();
        add_signed_header(&mut signed, "host", &endpoint.host);
        add_signed_header(&mut signed, "x-amz-content-sha256", &payload_hash);
        add_signed_header(&mut signed, "x-amz-date", &amz_date);
        for (name, value) in extra_headers {
            // SigV4 requires every x-amz-* header that is sent to be signed.
            let is_amz = name.trim().to_ascii_lowercase().starts_with("x-amz-");
            if is_amz && !value.trim().is_empty() {
                add_signed_header(&mut signed, name, value);
            }
        }

        let query = canonical_query(&req.params);
        let authorization = authorization(
            self.alias,
            &amz_date,
            &CanonicalRequest {
                method: req.method,
                uri_path: &uri_path,
                query: &query,
                headers: &signed,
                payload_hash: &payload_hash,
            },
        )?;

        let mut header_lines = vec![
            format!("Host: {}", endpoint.host),
            format!("x-amz-date: {amz_date}"),
            format!("x-amz-content-sha256: {payload_hash}"),
            format!("Authorization: {authorization}"),
        ];
        header_lines.extend(req.headers.iter().map(|(n, v)| format!("{n}: {v}")));
        let has_content_type = req
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-type"));
        if req.body.is_some() && !has_content_type {
            // Otherwise curl's --data-binary labels the body as a form submission.
            header_lines.push("Content-Type: application/octet-stream".to_string());
        }
        header_lines.extend(self.http.custom_headers.iter().cloned());

        let mut url = format!("{}://{}{}", endpoint.scheme, endpoint.host, uri_path);
        if !req.params.is_empty() {
            url.push('?');
            url.push_str(&url_query(&req.params));
        }
        Ok(Prepared { url, header_lines })
    }

    fn curl(&self, req: &Request, prepared: &Prepared) -> Command {
        let mut cmd = Command::new("curl");
        self.http.apply_to_curl(&mut cmd, req.body.is_some());
        cmd.arg("-sS").arg(&prepared.url);
        if req.method != "HEAD" {
            cmd.arg("-X").arg(req.method);
        }
        cmd.arg("-H").arg("@-");
        if let Some(file) = req.body {
            cmd.arg("--data-binary").arg(format!("@{}", file.display()));
        }
        cmd
    }

    /// Spawns curl and writes the request headers to its stdin.
    fn spawn(&self, mut cmd: Command, method: &str, prepared: &Prepared) -> Result<Child, String> {
        if self.debug {
            eprintln!("[debug] request: {method} {}", prepared.url);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let mut headers = prepared.header_lines.join("\n");
            headers.push('\n');
            // A write error means curl already exited; its status reports why.
            let _ = stdin.write_all(headers.as_bytes());
        }
        Ok(child)
    }

    fn execute(&self, req: &Request, sink: Sink) -> Result<String, String> {
        let prepared = self.prepare(req)?;
        let mut cmd = self.curl(req, &prepared);
        if req.method == "HEAD" {
            // Use curl native HEAD mode instead of `-X HEAD` + body suppression.
            // This avoids curl(18) "transfer closed with bytes remaining" on servers
            // that return Content-Length for HEAD responses.
            cmd.arg("-I");
        } else {
            match sink {
                Sink::Capture => {}
                Sink::File(out) => {
                    cmd.arg("-o").arg(out);
                }
                Sink::Headers => {
                    cmd.arg("-D").arg("-").arg("-o").arg("/dev/null");
                }
            }
        }
        cmd.arg("-w").arg("\nHTTPSTATUS:%{http_code}");

        let child = self.spawn(cmd, req.method, &prepared)?;
        let output: Output = child.wait_with_output().map_err(|e| e.to_string())?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(format!("request execution failed: {}", stderr.trim()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (body, status) = stdout
            .rsplit_once("\nHTTPSTATUS:")
            .ok_or_else(|| "unable to parse HTTP status".to_string())?;
        let status = status.trim();
        if !status.starts_with('2') {
            let body = match sink {
                Sink::File(path) => fs::read(path)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default(),
                _ => body.to_string(),
            };
            return Err(format!(
                "request failed with status {status}: body='{}' stderr='{}'",
                body.trim(),
                stderr.trim()
            ));
        }

        Ok(body.to_string())
    }
}

/// Reads curl's `-D -` header blocks and returns the final (non-1xx) status code,
/// or `None` if the output ended before any status line.
fn read_final_status(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        let status = String::from_utf8_lossy(&line)
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 || line.trim_ascii().is_empty() {
                break;
            }
        }
        if !status.starts_with('1') {
            return Ok(Some(status));
        }
    }
}

/// Response body of a request started with [`S3Client::open`].
pub struct BodyReader {
    child: Child,
    reader: BufReader<ChildStdout>,
    done: bool,
}

impl BodyReader {
    /// Stops curl and returns whatever it wrote to stderr.
    fn abort(&mut self) -> String {
        self.done = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        stderr
    }
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(buf)?;
        if n == 0 && !buf.is_empty() && !self.done {
            // End of body: a failed transfer (e.g. dropped connection) must not
            // look like a complete object.
            self.done = true;
            let status = self.child.wait()?;
            if !status.success() {
                let mut stderr = String::new();
                if let Some(mut pipe) = self.child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Err(io::Error::other(format!(
                    "request execution failed: {}",
                    stderr.trim()
                )));
            }
        }
        Ok(n)
    }
}

impl Drop for BodyReader {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::read_final_status;
    use std::io::Cursor;

    #[test]
    fn read_final_status_skips_informational_blocks() {
        let raw =
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 404 Not Found\r\nContent-Length: 3\r\n\r\nabc";
        let mut reader = Cursor::new(&raw[..]);
        assert_eq!(
            read_final_status(&mut reader).expect("read").as_deref(),
            Some("404")
        );
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut reader, &mut rest).expect("rest");
        assert_eq!(rest, "abc");
    }

    #[test]
    fn read_final_status_reports_missing_status() {
        let mut reader = Cursor::new(&b""[..]);
        assert_eq!(read_final_status(&mut reader).expect("read"), None);
    }
}
