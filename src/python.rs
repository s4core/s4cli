//! Every python3 invocation lives here: HMAC signing and file hashing.
//!
//! Together with curl (see `s3::client`) this is the only external runtime
//! dependency; replacing it with native crates only touches this module.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Runs a python3 snippet with optional stdin and returns its stdout, or its
/// stderr on failure.
fn run<I, S>(script: &str, args: I, stdin: Option<&[u8]>) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(script)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run python3: {e}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

const SIGNATURE_SCRIPT: &str = r#"
import sys, hmac, hashlib
secret, date_stamp, region, amz_date, canonical_request = sys.stdin.buffer.read().decode().split('\0')
scope = f'{date_stamp}/{region}/s3/aws4_request'
string_to_sign = '\n'.join(['AWS4-HMAC-SHA256', amz_date, scope, hashlib.sha256(canonical_request.encode()).hexdigest()])
def sign(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()
key = sign(sign(sign(sign(('AWS4' + secret).encode(), date_stamp), region), 's3'), 'aws4_request')
print(hmac.new(key, string_to_sign.encode(), hashlib.sha256).hexdigest())
"#;

/// Hex SigV4 signature of `canonical_request`. All inputs go through stdin so
/// the secret key never shows up in the process list.
pub fn sigv4_signature(
    secret_key: &str,
    date_stamp: &str,
    region: &str,
    amz_date: &str,
    canonical_request: &str,
) -> Result<String, String> {
    let input = [secret_key, date_stamp, region, amz_date, canonical_request].join("\0");
    let out = run(SIGNATURE_SCRIPT, [] as [&str; 0], Some(input.as_bytes()))?;
    let signature = out.trim();
    if signature.len() != 64 {
        return Err("signature helper returned unexpected output".to_string());
    }
    Ok(signature.to_string())
}

pub fn sha256_file_hex(path: &Path) -> Result<String, String> {
    let script = r#"
import hashlib, sys
h = hashlib.sha256()
with open(sys.argv[1], 'rb') as f:
    for chunk in iter(lambda: f.read(1 << 20), b''):
        h.update(chunk)
print(h.hexdigest())
"#;
    Ok(run(script, [path], None)?.trim().to_string())
}

pub fn md5_file_base64(path: &Path) -> Result<String, String> {
    let script = r#"
import base64, hashlib, pathlib, sys
data = pathlib.Path(sys.argv[1]).read_bytes()
print(base64.b64encode(hashlib.md5(data).digest()).decode())
"#;
    let out = run(script, [path], None)
        .map_err(|e| format!("failed to compute content-md5: {}", e.trim()))?;
    Ok(out.trim().to_string())
}
