//! S3 protocol layer: request signing, transport and higher-level operations.
//!
//! Layering: `commands` → `ops`/`multipart` (S3 API semantics) → `client`
//! (URL building, signing, curl) → `python`/curl.

mod client;
mod endpoint;
pub mod eventstream;
mod http;
mod multipart;
mod ops;
mod sign;
pub mod xml;

pub use client::{Request, S3Client};
pub use http::HttpOptions;
pub use ops::{MAX_COPY_OBJECT_SIZE, ObjectInfo};
