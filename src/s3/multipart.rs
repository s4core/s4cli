//! Object uploads, switching to multipart upload for large or unbounded input.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use super::client::{Request, S3Client};
use super::xml;
use crate::util::TempPath;

const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
const MIN_PART_SIZE: u64 = 8 * 1024 * 1024;
const MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;
const MAX_PARTS: u64 = 10_000;

impl S3Client<'_> {
    /// Uploads a local file, using multipart upload above the threshold.
    pub fn upload_file(&self, bucket: &str, key: &str, path: &Path) -> Result<(), String> {
        let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
        if size < MULTIPART_THRESHOLD {
            self.send(
                &Request::new("PUT", bucket)
                    .key(key)
                    .body(path)
                    .header("Content-Type", content_type_for(key)),
            )?;
            return Ok(());
        }
        let file = File::open(path).map_err(|e| e.to_string())?;
        self.multipart_upload(bucket, key, file, Some(size))
    }

    /// Uploads a stream of unknown length (e.g. stdin) without holding it all in memory.
    pub fn upload_reader(
        &self,
        bucket: &str,
        key: &str,
        mut reader: impl Read,
    ) -> Result<(), String> {
        let mut head = Vec::new();
        (&mut reader)
            .take(MULTIPART_THRESHOLD)
            .read_to_end(&mut head)
            .map_err(|e| e.to_string())?;
        if (head.len() as u64) < MULTIPART_THRESHOLD {
            let temp = TempPath::new("upload")?;
            fs::write(temp.path(), &head).map_err(|e| e.to_string())?;
            return self.upload_file(bucket, key, temp.path());
        }
        self.multipart_upload(bucket, key, io::Cursor::new(head).chain(reader), None)
    }

    fn multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        reader: impl Read,
        total: Option<u64>,
    ) -> Result<(), String> {
        let init_xml = self.send(
            &Request::new("POST", bucket)
                .key(key)
                .subresource("uploads")
                .header("Content-Type", content_type_for(key)),
        )?;
        let upload_id = xml::first_tag_value(&init_xml, "UploadId")
            .ok_or_else(|| "multipart init did not return UploadId".to_string())?;

        let result = self
            .upload_parts(bucket, key, &upload_id, reader, total)
            .and_then(|etags| self.complete_multipart(bucket, key, &upload_id, &etags));
        if result.is_err() {
            let abort = Request::new("DELETE", bucket)
                .key(key)
                .param("uploadId", upload_id.as_str());
            let _ = self.send(&abort);
        }
        result
    }

    fn upload_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        mut reader: impl Read,
        total: Option<u64>,
    ) -> Result<Vec<(u64, String)>, String> {
        let mut etags = Vec::new();

        for part_number in 1..=MAX_PARTS {
            let part = TempPath::new("mpu-part")?;
            let mut part_file = File::create(part.path()).map_err(|e| e.to_string())?;
            let written = io::copy(
                &mut (&mut reader).take(part_size(total, part_number)),
                &mut part_file,
            )
            .map_err(|e| e.to_string())?;
            drop(part_file);
            if written == 0 {
                break;
            }

            let headers = self.response_headers(
                &Request::new("PUT", bucket)
                    .key(key)
                    .param("partNumber", part_number.to_string())
                    .param("uploadId", upload_id)
                    .body(part.path()),
            )?;
            let etag = parse_etag(&headers)
                .ok_or_else(|| "multipart part response missing ETag".to_string())?;
            etags.push((part_number, etag));
        }

        if etags.is_empty() {
            return Err("multipart upload had no parts".to_string());
        }
        let mut probe = [0u8; 1];
        if etags.len() as u64 == MAX_PARTS
            && reader.read(&mut probe).map_err(|e| e.to_string())? > 0
        {
            return Err(format!(
                "upload exceeds the {MAX_PARTS}-part multipart limit"
            ));
        }
        Ok(etags)
    }

    fn complete_multipart(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        etags: &[(u64, String)],
    ) -> Result<(), String> {
        let body = TempPath::new("mpu-complete")?;
        fs::write(body.path(), build_complete_multipart_xml(etags)).map_err(|e| e.to_string())?;
        self.send(
            &Request::new("POST", bucket)
                .key(key)
                .param("uploadId", upload_id)
                .body(body.path())
                .header("Content-Type", "application/xml"),
        )?;
        Ok(())
    }
}

/// Size of part `part_number` (1-based) for an upload of `total` bytes, or of a
/// stream of unknown length.
fn part_size(total: Option<u64>, part_number: u64) -> u64 {
    match total {
        Some(total) => total.div_ceil(MAX_PARTS).max(MIN_PART_SIZE),
        // Double every 1000 parts so an unbounded stream still fits in 10000 parts.
        None => (MIN_PART_SIZE << ((part_number - 1) / 1000)).min(MAX_PART_SIZE),
    }
}

/// Content type for an uploaded object, guessed from its key's extension.
fn content_type_for(key: &str) -> &'static str {
    let ext = key
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

fn parse_etag(headers: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (name, value) = line.trim().split_once(':')?;
        let value = value.trim().trim_matches('"');
        (name.eq_ignore_ascii_case("etag") && !value.is_empty()).then(|| value.to_string())
    })
}

fn build_complete_multipart_xml(etags: &[(u64, String)]) -> String {
    let mut out = String::from("<CompleteMultipartUpload>");
    for (part, etag) in etags {
        out.push_str("<Part>");
        out.push_str(&format!("<PartNumber>{part}</PartNumber>"));
        out.push_str(&format!("<ETag>\"{}\"</ETag>", xml::escape(etag)));
        out.push_str("</Part>");
    }
    out.push_str("</CompleteMultipartUpload>");
    out
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PART_SIZE, MAX_PARTS, MIN_PART_SIZE, build_complete_multipart_xml, content_type_for,
        parse_etag, part_size,
    };

    #[test]
    fn build_complete_multipart_xml_contains_parts() {
        let xml =
            build_complete_multipart_xml(&[(1, "etag-1".to_string()), (2, "etag-2".to_string())]);
        assert!(xml.contains("<PartNumber>1</PartNumber>"));
        assert!(xml.contains("<ETag>\"etag-2\"</ETag>"));
    }

    #[test]
    fn parse_etag_finds_quoted_header() {
        let headers = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: \"abc\"\r\n\r\n";
        assert_eq!(parse_etag(headers).as_deref(), Some("abc"));
        assert_eq!(parse_etag("HTTP/1.1 200 OK\r\n"), None);
    }

    #[test]
    fn part_size_fits_large_files_into_part_limit() {
        assert_eq!(part_size(Some(20 * 1024 * 1024), 1), MIN_PART_SIZE);
        let five_tib = 5u64 << 40;
        assert!(part_size(Some(five_tib), 1) * MAX_PARTS >= five_tib);
    }

    #[test]
    fn part_size_grows_for_unbounded_streams() {
        assert_eq!(part_size(None, 1), MIN_PART_SIZE);
        assert_eq!(part_size(None, 1001), 2 * MIN_PART_SIZE);
        let capacity: u64 = (1..=MAX_PARTS).map(|n| part_size(None, n)).sum();
        assert!(capacity >= 5u64 << 40);
        assert!((1..=MAX_PARTS).all(|n| part_size(None, n) <= MAX_PART_SIZE));
    }

    #[test]
    fn content_type_is_guessed_from_extension() {
        assert_eq!(content_type_for("dir/a.TXT"), "text/plain");
        assert_eq!(content_type_for("img.jpeg"), "image/jpeg");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }
}
