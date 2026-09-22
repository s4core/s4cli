//! Higher-level S3 operations built on top of [`S3Client`].

use std::fs;
use std::path::Path;

use super::client::{Request, S3Client};
use super::endpoint::uri_encode_path;
use super::xml;
use crate::python;
use crate::time;
use crate::util::TempPath;

/// Largest object a single CopyObject request can copy.
pub const MAX_COPY_OBJECT_SIZE: u64 = 5 * 1024 * 1024 * 1024;

/// One entry of a ListObjectsV2 response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    /// Without surrounding quotes.
    pub etag: String,
    /// Seconds since the epoch.
    pub last_modified: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectVersion {
    pub key: String,
    pub version_id: String,
}

impl S3Client<'_> {
    /// Lists every object whose key starts with `prefix`, following continuation tokens.
    pub fn list_objects(&self, bucket: &str, prefix: &str) -> Result<Vec<ObjectInfo>, String> {
        let mut objects = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut req = Request::new("GET", bucket).param("list-type", "2");
            if !prefix.is_empty() {
                req = req.param("prefix", prefix);
            }
            if let Some(token) = &continuation {
                req = req.param("continuation-token", token.as_str());
            }

            let body = self.send(&req)?;
            objects.extend(object_entries(&body));

            if !xml::is_truncated(&body) {
                break;
            }
            continuation = xml::first_tag_value(&body, "NextContinuationToken");
            if continuation.is_none() {
                break;
            }
        }

        Ok(objects)
    }

    pub fn list_object_keys(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, String> {
        Ok(self
            .list_objects(bucket, prefix)?
            .into_iter()
            .map(|o| o.key)
            .collect())
    }

    /// Lists every object version and delete marker in the bucket.
    pub fn list_object_versions(&self, bucket: &str) -> Result<Vec<ObjectVersion>, String> {
        let mut versions = Vec::new();
        let mut key_marker: Option<String> = None;
        let mut version_id_marker: Option<String> = None;

        loop {
            let mut req = Request::new("GET", bucket).subresource("versions");
            if let Some(marker) = &key_marker {
                req = req.param("key-marker", marker.as_str());
            }
            if let Some(marker) = &version_id_marker {
                req = req.param("version-id-marker", marker.as_str());
            }

            let body = self.send(&req)?;
            versions.extend(version_entries(&body, "Version"));
            versions.extend(version_entries(&body, "DeleteMarker"));

            if !xml::is_truncated(&body) {
                break;
            }
            key_marker = xml::first_tag_value(&body, "NextKeyMarker");
            version_id_marker = xml::first_tag_value(&body, "NextVersionIdMarker");
            if key_marker.is_none() {
                break;
            }
        }

        Ok(versions)
    }

    /// Deletes every object version so the bucket itself can be removed.
    pub fn purge_bucket_versions(
        &self,
        bucket: &str,
        bypass_governance: bool,
    ) -> Result<(), String> {
        for entry in self.list_object_versions(bucket)? {
            self.delete_object(
                bucket,
                &entry.key,
                Some(&entry.version_id),
                bypass_governance,
            )?;
        }
        Ok(())
    }

    pub fn delete_object(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
        bypass_governance: bool,
    ) -> Result<(), String> {
        let mut req = Request::new("DELETE", bucket).key(key);
        if let Some(version_id) = version_id {
            req = req.param("versionId", version_id);
        }
        if bypass_governance {
            req = req.bypass_governance();
        }
        self.send(&req)?;
        Ok(())
    }

    /// Server-side copy (`x-amz-copy-source`) into `dst_bucket/dst_key` on this
    /// endpoint; limited to [`MAX_COPY_OBJECT_SIZE`].
    pub fn copy_object(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> Result<(), String> {
        let copy_source = format!(
            "/{}/{}",
            uri_encode_path(src_bucket),
            uri_encode_path(src_key)
        );
        let req = Request::new("PUT", dst_bucket)
            .key(dst_key)
            .header("x-amz-copy-source", copy_source);
        self.send(&req)?;
        Ok(())
    }

    /// PUTs an XML document from `body` to a bucket (`key: None`) or object
    /// subresource such as `?cors` or `?retention`, with Content-MD5.
    pub fn put_subresource(
        &self,
        bucket: &str,
        key: Option<&str>,
        subresource: &str,
        body: &Path,
        bypass_governance: bool,
    ) -> Result<(), String> {
        let md5 = python::md5_file_base64(body)?;
        let mut req = Request::new("PUT", bucket)
            .subresource(subresource)
            .body(body)
            .header("Content-MD5", md5)
            .header("Content-Type", "application/xml");
        if let Some(key) = key {
            req = req.key(key);
        }
        if bypass_governance {
            req = req.bypass_governance();
        }
        self.send(&req)?;
        Ok(())
    }

    /// Like [`put_subresource`](Self::put_subresource) with an in-memory document.
    pub fn put_subresource_xml(
        &self,
        bucket: &str,
        key: Option<&str>,
        subresource: &str,
        document: &str,
        bypass_governance: bool,
    ) -> Result<(), String> {
        let temp = TempPath::new(subresource)?;
        fs::write(temp.path(), document).map_err(|e| e.to_string())?;
        self.put_subresource(bucket, key, subresource, temp.path(), bypass_governance)
    }
}

fn object_entries(body: &str) -> Vec<ObjectInfo> {
    xml::tag_values(body, "Contents")
        .iter()
        .filter_map(|block| {
            Some(ObjectInfo {
                key: xml::first_tag_value(block, "Key")?,
                size: xml::first_tag_value(block, "Size")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or_default(),
                etag: xml::first_tag_value(block, "ETag")
                    .map(|e| e.trim().trim_matches('"').to_string())
                    .unwrap_or_default(),
                last_modified: xml::first_tag_value(block, "LastModified")
                    .and_then(|v| time::parse_iso8601(&v)),
            })
        })
        .collect()
}

fn version_entries(xml_body: &str, tag: &str) -> Vec<ObjectVersion> {
    xml::tag_values(xml_body, tag)
        .iter()
        .filter_map(|block| {
            Some(ObjectVersion {
                key: xml::first_tag_value(block, "Key")?,
                version_id: xml::first_tag_value(block, "VersionId")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ObjectInfo, object_entries, version_entries};

    #[test]
    fn version_entries_works_for_versions_and_delete_markers() {
        let xml = "<ListVersionsResult><Version><Key>k1</Key><VersionId>v1</VersionId></Version><DeleteMarker><Key>k2</Key><VersionId>v2</VersionId></DeleteMarker></ListVersionsResult>";
        let versions = version_entries(xml, "Version");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].key, "k1");
        assert_eq!(versions[0].version_id, "v1");

        let delete_markers = version_entries(xml, "DeleteMarker");
        assert_eq!(delete_markers.len(), 1);
        assert_eq!(delete_markers[0].key, "k2");
        assert_eq!(delete_markers[0].version_id, "v2");
    }

    #[test]
    fn object_entries_reads_listing_metadata() {
        let xml = "<ListBucketResult><Contents><Key>a&amp;b.txt</Key><LastModified>2013-05-24T00:00:00.000Z</LastModified><ETag>&quot;abc&quot;</ETag><Size>24</Size></Contents><Contents><Key>c</Key></Contents></ListBucketResult>";
        assert_eq!(
            object_entries(xml),
            vec![
                ObjectInfo {
                    key: "a&b.txt".to_string(),
                    size: 24,
                    etag: "abc".to_string(),
                    last_modified: Some(1_369_353_600),
                },
                ObjectInfo {
                    key: "c".to_string(),
                    size: 0,
                    etag: String::new(),
                    last_modified: None,
                },
            ]
        );
    }
}
