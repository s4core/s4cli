//! AWS Signature Version 4: canonical request construction.
//!
//! The canonical request is built here; only the HMAC chain runs in python.

use std::collections::BTreeMap;
use std::path::Path;

use super::endpoint::uri_encode_query_component;
use crate::config::AliasConfig;
use crate::python;

/// SHA-256 of an empty payload.
const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

pub fn payload_hash(body: Option<&Path>) -> Result<String, String> {
    match body {
        Some(path) => python::sha256_file_hex(path),
        None => Ok(EMPTY_PAYLOAD_SHA256.to_string()),
    }
}

/// Percent-encodes query parameters and sorts them, as SigV4 requires.
fn encoded_params(params: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (uri_encode_query_component(k), uri_encode_query_component(v)))
        .collect();
    pairs.sort();
    pairs
}

/// SigV4 canonical query string: sorted `name=value` pairs, `=` kept for empty values.
pub fn canonical_query(params: &[(String, String)]) -> String {
    encoded_params(params)
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Query string for the request URL; valueless subresources are sent bare (`?cors`).
pub fn url_query(params: &[(String, String)]) -> String {
    encoded_params(params)
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                k.clone()
            } else {
                format!("{k}={v}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Adds a header to the signed set: lowercase name, value trimmed with inner
/// whitespace collapsed; repeated names are joined with commas.
pub fn add_signed_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    headers
        .entry(name.trim().to_ascii_lowercase())
        .and_modify(|existing| {
            existing.push(',');
            existing.push_str(&value);
        })
        .or_insert(value);
}

pub struct CanonicalRequest<'a> {
    pub method: &'a str,
    pub uri_path: &'a str,
    /// Output of [`canonical_query`].
    pub query: &'a str,
    /// Every signed header, keyed by lowercase name.
    pub headers: &'a BTreeMap<String, String>,
    pub payload_hash: &'a str,
}

impl CanonicalRequest<'_> {
    fn signed_headers(&self) -> String {
        self.headers.keys().cloned().collect::<Vec<_>>().join(";")
    }

    fn render(&self) -> String {
        let mut out = format!("{}\n{}\n{}\n", self.method, self.uri_path, self.query);
        for (name, value) in self.headers {
            out.push_str(&format!("{name}:{value}\n"));
        }
        out.push('\n');
        out.push_str(&self.signed_headers());
        out.push('\n');
        out.push_str(self.payload_hash);
        out
    }
}

/// `Authorization` header value for `req` signed at `amz_date` (`YYYYMMDDTHHMMSSZ`).
pub fn authorization(
    alias: &AliasConfig,
    amz_date: &str,
    req: &CanonicalRequest,
) -> Result<String, String> {
    let date_stamp = &amz_date[..8];
    let signature = python::sigv4_signature(
        &alias.secret_key,
        date_stamp,
        &alias.region,
        amz_date,
        &req.render(),
    )?;
    Ok(format!(
        "AWS4-HMAC-SHA256 Credential={}/{date_stamp}/{}/s3/aws4_request, SignedHeaders={}, Signature={signature}",
        alias.access_key,
        alias.region,
        req.signed_headers()
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CanonicalRequest, EMPTY_PAYLOAD_SHA256, add_signed_header, authorization, canonical_query,
        url_query,
    };
    use crate::config::AliasConfig;
    use std::collections::BTreeMap;

    fn params(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        let p = params(&[
            ("prefix", "photos/2024/"),
            ("list-type", "2"),
            ("continuation-token", "a+b/c="),
        ]);
        assert_eq!(
            canonical_query(&p),
            "continuation-token=a%2Bb%2Fc%3D&list-type=2&prefix=photos%2F2024%2F"
        );
    }

    #[test]
    fn subresources_are_bare_in_url_but_valued_in_canonical_form() {
        let p = params(&[("select", ""), ("select-type", "2")]);
        assert_eq!(canonical_query(&p), "select=&select-type=2");
        assert_eq!(url_query(&p), "select&select-type=2");
    }

    #[test]
    fn signed_header_values_are_normalized() {
        let mut headers = BTreeMap::new();
        add_signed_header(&mut headers, "X-Amz-Meta-A", "  one   two ");
        add_signed_header(&mut headers, "x-amz-meta-a", "three");
        assert_eq!(headers["x-amz-meta-a"], "one two,three");
    }

    fn aws_example_alias() -> AliasConfig {
        AliasConfig {
            endpoint: "https://examplebucket.s3.amazonaws.com".to_string(),
            access_key: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            region: "us-east-1".to_string(),
            path_style: false,
        }
    }

    fn example_headers(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        add_signed_header(&mut headers, "host", "examplebucket.s3.amazonaws.com");
        add_signed_header(&mut headers, "x-amz-content-sha256", EMPTY_PAYLOAD_SHA256);
        add_signed_header(&mut headers, "x-amz-date", "20130524T000000Z");
        for (name, value) in extra {
            add_signed_header(&mut headers, name, value);
        }
        headers
    }

    // Examples from the AWS "Signature Calculations for the Authorization
    // Header" documentation for S3.
    #[test]
    fn matches_aws_get_object_example() {
        let headers = example_headers(&[("range", "bytes=0-9")]);
        let auth = authorization(
            &aws_example_alias(),
            "20130524T000000Z",
            &CanonicalRequest {
                method: "GET",
                uri_path: "/test.txt",
                query: "",
                headers: &headers,
                payload_hash: EMPTY_PAYLOAD_SHA256,
            },
        )
        .expect("sign");
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn matches_aws_list_objects_example() {
        let headers = example_headers(&[]);
        let query = canonical_query(&params(&[("prefix", "J"), ("max-keys", "2")]));
        let auth = authorization(
            &aws_example_alias(),
            "20130524T000000Z",
            &CanonicalRequest {
                method: "GET",
                uri_path: "/",
                query: &query,
                headers: &headers,
                payload_hash: EMPTY_PAYLOAD_SHA256,
            },
        )
        .expect("sign");
        assert!(
            auth.ends_with(
                "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
            ),
            "{auth}"
        );
    }
}
