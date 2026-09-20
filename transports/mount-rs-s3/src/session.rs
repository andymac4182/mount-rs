//! In-process S3 request session.
//!
//! The session is intentionally independent of HTTP. It maps one parsed S3
//! request to one response, owns no listener, and uses the shared
//! mount-rs-core::FsDriver contract for all storage.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use md5::{Digest as Md5Digest, Md5};
use mount_rs_core::{
    FileType, FsDriver, MkdirOptions, Stats,
    path::{dirname, normalize_path},
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

use crate::protocol::{
    self, ByteRange, ListObjectsXml, ListPartsXml, ListedObject, ListedPart, MAX_PART_SIZE,
    MAX_XML_BYTES, MIN_PART_SIZE, MULTIPART_PREFIX, ObjectTarget, Operation, S3Failure, S3Response,
    S3Result, compare_utf8, conditional_match, content_encoding_chunked, decode_continuation_token,
    delete_result_xml, encode_continuation_token, error_response, header_content_length,
    header_md5, initiate_multipart_xml, is_staging_key, list_buckets_xml, list_objects_xml,
    list_parts_xml, parse_complete_document, parse_delete_document, parse_meta_mtime,
    parse_object_key, parse_request_target, s3_error, unquote_etag, xml_response,
};
use crate::sigv4::{self, Credentials, HeaderEntry, SigV4Failure, header_list, header_value};

const DEFAULT_BUCKET: &str = "mountx";
const DEFAULT_READ_CHUNK: usize = 128 * 1024;
const DEFAULT_MAX_BODY: usize = 512 * 1024 * 1024;
const MANIFEST_NAME: &str = "upload.json";

struct ListObjectsRequest<'a> {
    bucket: &'a str,
    driver: Arc<dyn FsDriver>,
    prefix: &'a str,
    delimiter: Option<&'a str>,
    max_keys: usize,
    continuation: Option<&'a str>,
    start_after: Option<&'a str>,
    fetch_owner: bool,
    encoding_url: bool,
}

struct UploadBody<'a> {
    head: &'a S3RequestHead,
    body: &'a [u8],
    verified: Option<&'a sigv4::VerifiedRequest>,
}

#[derive(Debug, Clone)]
pub struct S3SessionOptions {
    pub credentials: Option<Credentials>,
    pub region: Option<String>,
    pub max_body_bytes: usize,
    pub max_xml_bytes: usize,
    pub read_chunk_bytes: usize,
}

impl Default for S3SessionOptions {
    fn default() -> Self {
        Self {
            credentials: None,
            region: None,
            max_body_bytes: DEFAULT_MAX_BODY,
            max_xml_bytes: MAX_XML_BYTES,
            read_chunk_bytes: DEFAULT_READ_CHUNK,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3RequestHead {
    pub method: String,
    pub target: String,
    pub headers: Vec<HeaderEntry>,
}

impl S3RequestHead {
    pub fn new(method: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            target: target.into(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(HeaderEntry::new(name, value));
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3Request {
    pub head: S3RequestHead,
    pub body: Vec<u8>,
}

impl S3Request {
    pub fn new(
        method: impl Into<String>,
        target: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        let body = body.into();
        let mut head = S3RequestHead::new(method, target);
        head = head.with_header("content-length", body.len().to_string());
        Self { head, body }
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3SessionStats {
    pub requests: u64,
    pub replies: u64,
    pub errors: u64,
    pub operations: BTreeMap<String, u64>,
}

#[derive(Clone)]
pub struct S3Session {
    pub buckets: Arc<BTreeMap<String, Arc<dyn FsDriver>>>,
    pub options: S3SessionOptions,
    stats: Arc<Mutex<S3SessionStats>>,
    next_request_id: Arc<std::sync::atomic::AtomicU64>,
}

impl S3Session {
    pub fn new<D>(driver: D) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::new_with_options(driver, S3SessionOptions::default())
    }

    pub fn new_with_options<D>(driver: D, options: S3SessionOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        let mut buckets = BTreeMap::new();
        buckets.insert(
            DEFAULT_BUCKET.to_owned(),
            Arc::new(driver) as Arc<dyn FsDriver>,
        );
        Self::from_buckets_with_options(buckets, options)
    }

    pub fn from_buckets(buckets: impl IntoIterator<Item = (String, Arc<dyn FsDriver>)>) -> Self {
        Self::from_buckets_with_options(buckets, S3SessionOptions::default())
    }

    pub fn from_buckets_with_options(
        buckets: impl IntoIterator<Item = (String, Arc<dyn FsDriver>)>,
        options: S3SessionOptions,
    ) -> Self {
        Self {
            buckets: Arc::new(buckets.into_iter().collect()),
            options,
            stats: Arc::new(Mutex::new(S3SessionStats::default())),
            next_request_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    pub async fn stats(&self) -> S3SessionStats {
        self.stats.lock().await.clone()
    }

    pub fn bucket_names(&self) -> Vec<String> {
        let mut names = self.buckets.keys().cloned().collect::<Vec<_>>();
        names.sort_by(|left, right| compare_utf8(left, right));
        names
    }

    pub async fn handle(&self, request: S3Request) -> S3Response {
        self.handle_request(request.head, request.body).await
    }

    /// Answer exactly one in-process request. Errors are converted to an S3
    /// response rather than escaping to the HTTP layer.
    pub async fn handle_request(&self, head: S3RequestHead, body: Vec<u8>) -> S3Response {
        {
            let mut stats = self.stats.lock().await;
            stats.requests += 1;
        }
        let request_id = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request_id = format!("mountx-{request_id:016x}");
        let resource = Some(head.target.as_str());
        let result = self.dispatch(&head, &body).await;
        let response = match result {
            Ok(mut response) => {
                response
                    .headers
                    .push(("x-amz-request-id".to_owned(), request_id));
                if head.method.eq_ignore_ascii_case("HEAD") {
                    response.body.clear();
                }
                response
            }
            Err(error) => error_response(&error.error(), &request_id, resource),
        };
        let mut stats = self.stats.lock().await;
        stats.replies += 1;
        if response.status >= 400 {
            stats.errors += 1;
        }
        response
    }

    async fn dispatch(&self, head: &S3RequestHead, body: &[u8]) -> S3Result<S3Response> {
        if body.len() > self.options.max_body_bytes {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let target = parse_request_target(&head.target)?;
        let verified = self.authorize(head, &target, body)?;
        let operation = protocol::route_request(&head.method, &target, &head.headers)?;
        self.count_operation(operation_name(&operation)).await;
        if matches!(&operation, Operation::DeleteObject(target) if target.path.is_empty()) {
            return Ok(S3Response::empty(204));
        }
        let bucket_name = operation_bucket(&operation);
        let driver = if let Some(bucket) = bucket_name {
            self.buckets
                .get(bucket)
                .ok_or_else(|| S3Failure::s3("NoSuchBucket"))?
                .clone()
        } else {
            self.buckets
                .values()
                .next()
                .cloned()
                .ok_or_else(|| S3Failure::s3("NoSuchBucket"))?
        };
        match operation {
            Operation::ListBuckets => self.list_buckets().await,
            Operation::HeadBucket { .. } => Ok(S3Response::empty(200).header(
                "x-amz-bucket-region",
                self.options
                    .region
                    .clone()
                    .unwrap_or_else(|| "us-east-1".to_owned()),
            )),
            Operation::ListObjectsV2 {
                bucket,
                prefix,
                delimiter,
                max_keys,
                continuation_token,
                start_after,
                fetch_owner,
                encoding_type_url,
            } => {
                self.list_objects(ListObjectsRequest {
                    bucket: &bucket,
                    driver,
                    prefix: &prefix,
                    delimiter: delimiter.as_deref(),
                    max_keys,
                    continuation: continuation_token.as_deref(),
                    start_after: start_after.as_deref(),
                    fetch_owner,
                    encoding_url: encoding_type_url,
                })
                .await
            }
            Operation::GetObject(target) => self.get_object(driver, &target, head, false).await,
            Operation::HeadObject(target) => self.get_object(driver, &target, head, true).await,
            Operation::PutObject(target) => {
                self.put_object(driver, &target, head, body, verified.as_ref())
                    .await
            }
            Operation::DeleteObject(target) => self.delete_object(driver, &target).await,
            Operation::DeleteObjects { .. } => {
                self.delete_objects(driver, head, body, verified.as_ref())
                    .await
            }
            Operation::CopyObject {
                destination,
                source,
                replace_metadata,
            } => {
                self.copy_object(driver, &destination, &source, replace_metadata, head)
                    .await
            }
            Operation::CreateMultipartUpload(target) => {
                self.create_multipart(driver, &target, head).await
            }
            Operation::UploadPart {
                target,
                upload_id,
                part_number,
            } => {
                self.upload_part(
                    driver,
                    &target,
                    &upload_id,
                    part_number,
                    UploadBody {
                        head,
                        body,
                        verified: verified.as_ref(),
                    },
                )
                .await
            }
            Operation::CompleteMultipartUpload { target, upload_id } => {
                self.complete_multipart(
                    driver,
                    &target,
                    &upload_id,
                    UploadBody {
                        head,
                        body,
                        verified: verified.as_ref(),
                    },
                )
                .await
            }
            Operation::AbortMultipartUpload { target, upload_id } => {
                self.abort_multipart(driver, &target, &upload_id).await
            }
            Operation::ListParts {
                target,
                upload_id,
                max_parts,
                marker,
            } => {
                self.list_parts(driver, &target, &upload_id, max_parts, marker)
                    .await
            }
        }
    }

    fn authorize(
        &self,
        head: &S3RequestHead,
        target: &protocol::ParsedTarget,
        body: &[u8],
    ) -> S3Result<Option<sigv4::VerifiedRequest>> {
        let Some(credentials) = &self.options.credentials else {
            return Ok(None);
        };
        sigv4::verify_request(sigv4::VerifyRequest {
            method: &head.method,
            path: &target.path,
            query: &target.query,
            headers: &head.headers,
            body,
            credentials,
            expected_region: self.options.region.as_deref(),
            now_ms: now_ms(),
        })
        .map(Some)
        .map_err(|failure| {
            S3Failure::S3(sigv4_error(
                failure,
                target
                    .query
                    .iter()
                    .any(|entry| entry.name.starts_with("X-Amz-")),
            ))
        })
    }

    async fn count_operation(&self, operation: &str) {
        let mut stats = self.stats.lock().await;
        *stats.operations.entry(operation.to_owned()).or_default() += 1;
    }

    async fn list_buckets(&self) -> S3Result<S3Response> {
        let mut buckets = Vec::new();
        for name in self.bucket_names() {
            let date = match self
                .buckets
                .get(&name)
                .expect("bucket name came from map")
                .stat("/")
                .await
            {
                Ok(stats) => format_iso_date(stats.mtime_ms),
                Err(_) => format_iso_date(0),
            };
            buckets.push((name, date));
        }
        Ok(xml_response(200, list_buckets_xml(&buckets), ""))
    }

    async fn get_object(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
        is_head: bool,
    ) -> S3Result<S3Response> {
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        let is_directory = stats.file_type() == FileType::Directory;
        if target.directory != is_directory || (!target.directory && !stats.is_file()) {
            return Err(S3Failure::s3("NoSuchKey"));
        }
        let etag = object_etag(&stats);
        if let Some(status) = evaluate_get_conditionals(&stats, &etag, &head.headers, &head.method)
        {
            return Ok(S3Response::empty(status).header("etag", protocol::etag_header(&etag)));
        }
        let mut selected_range = None;
        if let Some(raw_range) = header_value(&head.headers, "range") {
            let range = protocol::parse_range(&raw_range, stats.size);
            if let Some(if_range) = header_value(&head.headers, "if-range") {
                if if_range_matches(&if_range, &etag, stats.mtime_ms) {
                    selected_range = range;
                }
            } else {
                selected_range = range;
            }
            if range.is_none() && header_value(&head.headers, "if-range").is_none() {
                return Ok(S3Response::empty(416)
                    .header("content-range", format!("bytes */{}", stats.size)));
            }
        }
        let status = if selected_range.is_some() { 206 } else { 200 };
        let mut response = S3Response::empty(status);
        response.headers.extend(protocol::object_headers(
            &etag,
            if target.directory { 0 } else { stats.size },
            stats.mtime_ms,
            selected_range,
            stats.size,
        ));
        if !is_head && !target.directory {
            response.body = read_bytes_range(
                driver,
                &target.path,
                selected_range,
                stats.size,
                self.options.read_chunk_bytes,
            )
            .await?;
        }
        Ok(response)
    }

    async fn put_object(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
        body: &[u8],
        verified: Option<&sigv4::VerifiedRequest>,
    ) -> S3Result<S3Response> {
        validate_declared_length(&head.headers, body.len())?;
        let existing = driver.stat(&target.path).await.ok();
        check_put_conditionals(existing.as_ref(), &head.headers)?;
        let body = decode_request_body(
            &head.headers,
            body,
            self.options.max_body_bytes,
            verified,
            self.options.credentials.as_ref(),
        )?;
        let requested_mtime =
            parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref());
        if target.directory {
            if !body.is_empty() {
                return Err(S3Failure::s3("InvalidRequest"));
            }
            driver
                .mkdir(
                    &target.path,
                    MkdirOptions {
                        recursive: true,
                        mode: Some(0o777),
                    },
                )
                .await
                .map_err(S3Failure::Fs)?;
            if let Some(mtime) = requested_mtime {
                apply_mtime(&driver, &target.path, mtime).await?;
            }
            let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
            return Ok(S3Response::empty(200)
                .header("etag", protocol::etag_header(&object_etag(&stats)))
                .header("content-length", "0"));
        }
        ensure_parent(&driver, &target.path).await?;
        write_bytes(
            &driver,
            &target.path,
            &body,
            existing.is_none() && check_create_only(&head.headers),
        )
        .await?;
        if let Some(mtime) = requested_mtime {
            apply_mtime(&driver, &target.path, mtime).await?;
        }
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        Ok(S3Response::empty(200)
            .header("etag", protocol::etag_header(&object_etag(&stats)))
            .header("content-length", "0"))
    }

    async fn delete_object(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
    ) -> S3Result<S3Response> {
        let stats = match driver.stat(&target.path).await {
            Ok(stats) => stats,
            Err(error)
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                return Ok(S3Response::empty(204));
            }
            Err(error) => return Err(S3Failure::Fs(error)),
        };
        let is_directory = stats.file_type() == FileType::Directory;
        if target.directory {
            if !is_directory {
                return Ok(S3Response::empty(204));
            }
            driver.rmdir(&target.path).await.map_err(S3Failure::Fs)?;
        } else {
            if is_directory {
                return Ok(S3Response::empty(204));
            }
            driver.unlink(&target.path).await.map_err(S3Failure::Fs)?;
        }
        Ok(S3Response::empty(204))
    }

    async fn delete_objects(
        &self,
        driver: Arc<dyn FsDriver>,
        head: &S3RequestHead,
        body: &[u8],
        verified: Option<&sigv4::VerifiedRequest>,
    ) -> S3Result<S3Response> {
        validate_declared_length(&head.headers, body.len())?;
        let body = decode_request_body(
            &head.headers,
            body,
            self.options.max_body_bytes,
            verified,
            self.options.credentials.as_ref(),
        )?;
        if let Some(content_md5) = header_md5(&head.headers) {
            let digest = Md5::digest(&body);
            let expected = BASE64.encode(digest);
            if expected != content_md5 {
                return Err(S3Failure::s3("BadDigest"));
            }
        }
        let (keys, quiet) = parse_delete_document(&body, self.options.max_xml_bytes)?;
        let mut deleted = Vec::new();
        let mut errors = Vec::new();
        for key in keys {
            if is_staging_key(&key) {
                deleted.push(key);
                continue;
            }
            let target = match parse_object_key("", &key) {
                Ok(target) => target,
                Err(error) => {
                    errors.push((key, error.error()));
                    continue;
                }
            };
            match self.delete_object(driver.clone(), &target).await {
                Ok(_) => deleted.push(key),
                Err(error) => errors.push((key, error.error())),
            }
        }
        Ok(xml_response(
            200,
            delete_result_xml(&deleted, &errors, quiet),
            "",
        ))
    }

    async fn copy_object(
        &self,
        destination_driver: Arc<dyn FsDriver>,
        destination: &ObjectTarget,
        source: &ObjectTarget,
        replace_metadata: bool,
        head: &S3RequestHead,
    ) -> S3Result<S3Response> {
        if destination.directory {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let source_driver = self
            .buckets
            .get(&source.bucket)
            .ok_or_else(|| S3Failure::s3("NoSuchBucket"))?
            .clone();
        let source_stats = source_driver
            .stat(&source.path)
            .await
            .map_err(S3Failure::Fs)?;
        if !source_stats.is_file() || source.directory {
            return Err(S3Failure::s3("NoSuchKey"));
        }
        let source_etag = object_etag(&source_stats);
        check_copy_conditionals(&source_stats, &source_etag, &head.headers)?;
        let same_object = source.bucket == destination.bucket && source.path == destination.path;
        if same_object && !replace_metadata {
            return Err(S3Failure::S3(protocol::error_with_message(
                "InvalidRequest",
                "This copy request is illegal because you are trying to copy an object to itself without changing the metadata.",
            )));
        }
        let bytes = read_bytes_range(
            source_driver,
            &source.path,
            None,
            source_stats.size,
            self.options.read_chunk_bytes,
        )
        .await?;
        ensure_parent(&destination_driver, &destination.path).await?;
        write_bytes(&destination_driver, &destination.path, &bytes, false).await?;
        let mtime = if replace_metadata {
            parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref())
                .unwrap_or_else(now_ms)
        } else {
            source_stats.mtime_ms
        };
        apply_mtime(&destination_driver, &destination.path, mtime).await?;
        let destination_stats = destination_driver
            .stat(&destination.path)
            .await
            .map_err(S3Failure::Fs)?;
        Ok(xml_response(
            200,
            protocol::copy_object_xml(
                &object_etag(&destination_stats),
                &format_iso_date(destination_stats.mtime_ms),
            ),
            "",
        ))
    }

    async fn list_objects(&self, request: ListObjectsRequest<'_>) -> S3Result<S3Response> {
        let ListObjectsRequest {
            bucket,
            driver,
            prefix,
            delimiter,
            max_keys,
            continuation,
            start_after,
            fetch_owner,
            encoding_url,
        } = request;
        if !listable_prefix(prefix) {
            return Ok(xml_response(
                200,
                list_objects_xml(ListObjectsXml {
                    bucket,
                    prefix,
                    delimiter,
                    continuation,
                    next: None,
                    start_after,
                    max_keys,
                    key_count: 0,
                    truncated: false,
                    encoding_url,
                    objects: &[],
                    common_prefixes: &[],
                }),
                "",
            ));
        }
        let after = if let Some(token) = continuation {
            decode_continuation_token(token).ok_or_else(|| {
                S3Failure::S3(protocol::error_with_message(
                    "InvalidArgument",
                    "The continuation token provided is incorrect",
                ))
            })?
        } else {
            start_after.unwrap_or_default().to_owned()
        };
        let entries = collect_listing(driver.clone()).await?;
        let mut candidates = Vec::new();
        let mut seen_prefixes = std::collections::HashSet::new();
        for entry in entries {
            if !entry.key.starts_with(prefix)
                || compare_utf8(&entry.key, &after) != std::cmp::Ordering::Greater
            {
                continue;
            }
            if let Some(delimiter) = delimiter {
                let rest = &entry.key[prefix.len()..];
                if let Some(index) = rest.find(delimiter) {
                    let common = format!("{}{}", prefix, &rest[..index + delimiter.len()]);
                    if seen_prefixes.insert(common.clone()) {
                        candidates.push(ListCandidate::Prefix(common));
                    }
                    continue;
                }
            }
            let stats = driver.stat(&entry.path).await.map_err(S3Failure::Fs)?;
            candidates.push(ListCandidate::Object {
                key: entry.key,
                stats,
            });
        }
        candidates.sort_by(|left, right| compare_utf8(left.key(), right.key()));
        let truncated = max_keys > 0 && candidates.len() > max_keys;
        let page = candidates.into_iter().take(max_keys).collect::<Vec<_>>();
        let next = truncated
            .then(|| {
                page.last()
                    .map(|candidate| encode_continuation_token(candidate.key()))
            })
            .flatten();
        let mut objects = Vec::new();
        let mut prefixes = Vec::new();
        for candidate in page {
            match candidate {
                ListCandidate::Prefix(prefix) => prefixes.push(prefix),
                ListCandidate::Object { key, stats, .. } => objects.push(ListedObject {
                    key,
                    last_modified: format_iso_date(stats.mtime_ms),
                    etag: object_etag(&stats),
                    size: if stats.is_directory() { 0 } else { stats.size },
                    owner: fetch_owner,
                }),
            }
        }
        Ok(xml_response(
            200,
            list_objects_xml(ListObjectsXml {
                bucket,
                prefix,
                delimiter,
                continuation,
                next: next.as_deref(),
                start_after,
                max_keys,
                key_count: objects.len() + prefixes.len(),
                truncated,
                encoding_url,
                objects: &objects,
                common_prefixes: &prefixes,
            }),
            "",
        ))
    }

    async fn create_multipart(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        head: &S3RequestHead,
    ) -> S3Result<S3Response> {
        if target.directory {
            return Err(S3Failure::S3(protocol::error_with_message(
                "InvalidRequest",
                "A key ending in / names a directory and cannot be uploaded in parts.",
            )));
        }
        let upload_id = new_upload_id();
        let directory = upload_directory(&upload_id);
        driver
            .mkdir(
                &directory,
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o700),
                },
            )
            .await
            .map_err(S3Failure::Fs)?;
        let manifest = UploadManifest {
            key: target.key.clone(),
            mtime_ms: parse_meta_mtime(header_value(&head.headers, "x-amz-meta-mtime").as_deref()),
        };
        let bytes = serde_json::to_vec(&manifest)
            .map_err(|error| S3Failure::Internal(error.to_string()))?;
        write_bytes(
            &driver,
            &format!("{directory}/{MANIFEST_NAME}"),
            &bytes,
            false,
        )
        .await?;
        Ok(xml_response(
            200,
            initiate_multipart_xml(&target.bucket, &target.key, &upload_id),
            "",
        ))
    }

    async fn upload_part(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        part_number: u32,
        request: UploadBody<'_>,
    ) -> S3Result<S3Response> {
        let _manifest = read_manifest(&driver, upload_id, &target.key).await?;
        validate_declared_length(&request.head.headers, request.body.len())?;
        let body = decode_request_body(
            &request.head.headers,
            request.body,
            self.options.max_body_bytes,
            request.verified,
            self.options.credentials.as_ref(),
        )?;
        if body.len() as u64 > MAX_PART_SIZE {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let path = part_path(upload_id, part_number);
        write_bytes(&driver, &path, &body, false)
            .await
            .map_err(|error| match error {
                S3Failure::Fs(ref fs_error)
                    if matches!(
                        fs_error.code,
                        mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                    ) =>
                {
                    S3Failure::s3("NoSuchUpload")
                }
                other => other,
            })?;
        let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
        Ok(S3Response::empty(200)
            .header("etag", protocol::etag_header(&object_etag(&stats)))
            .header("content-length", "0"))
    }

    async fn complete_multipart(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        request: UploadBody<'_>,
    ) -> S3Result<S3Response> {
        validate_declared_length(&request.head.headers, request.body.len())?;
        let body = decode_request_body(
            &request.head.headers,
            request.body,
            self.options.max_body_bytes,
            request.verified,
            self.options.credentials.as_ref(),
        )?;
        let requested = parse_complete_document(&body, self.options.max_xml_bytes)?;
        let manifest = read_manifest(&driver, upload_id, &target.key).await?;
        let mut previous = 0_u32;
        let mut parts = Vec::new();
        for (index, (number, etag)) in requested.iter().enumerate() {
            if *number <= previous {
                return Err(S3Failure::s3("InvalidPartOrder"));
            }
            previous = *number;
            let path = part_path(upload_id, *number);
            let stats = driver.stat(&path).await.map_err(|error| {
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) {
                    S3Failure::s3("InvalidPart")
                } else {
                    S3Failure::Fs(error)
                }
            })?;
            if !stats.is_file() || unquote_etag(etag) != object_etag(&stats) {
                return Err(S3Failure::s3("InvalidPart"));
            }
            if index + 1 < requested.len() && stats.size < MIN_PART_SIZE {
                return Err(S3Failure::s3("EntityTooSmall"));
            }
            parts.push(path);
        }
        let mut assembled = Vec::new();
        for path in parts {
            let stats = driver.stat(&path).await.map_err(S3Failure::Fs)?;
            assembled.extend(
                read_bytes_range(
                    driver.clone(),
                    &path,
                    None,
                    stats.size,
                    self.options.read_chunk_bytes,
                )
                .await?,
            );
        }
        ensure_parent(&driver, &target.path).await?;
        write_bytes(&driver, &target.path, &assembled, false).await?;
        if let Some(mtime) = manifest.mtime_ms {
            apply_mtime(&driver, &target.path, mtime).await?;
        }
        let stats = driver.stat(&target.path).await.map_err(S3Failure::Fs)?;
        remove_tree(&driver, &upload_directory(upload_id)).await?;
        let location = format!("/{}/{}", target.bucket, target.key);
        Ok(xml_response(
            200,
            protocol::complete_multipart_xml(
                &location,
                &target.bucket,
                &target.key,
                &object_etag(&stats),
            ),
            "",
        ))
    }

    async fn abort_multipart(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
    ) -> S3Result<S3Response> {
        let _ = read_manifest(&driver, upload_id, &target.key).await?;
        remove_tree(&driver, &upload_directory(upload_id)).await?;
        Ok(S3Response::empty(204))
    }

    async fn list_parts(
        &self,
        driver: Arc<dyn FsDriver>,
        target: &ObjectTarget,
        upload_id: &str,
        max_parts: usize,
        marker: u32,
    ) -> S3Result<S3Response> {
        let _ = read_manifest(&driver, upload_id, &target.key).await?;
        let directory = upload_directory(upload_id);
        let mut parts = Vec::new();
        for entry in driver.readdir(&directory).await.map_err(S3Failure::Fs)? {
            let Some(number) = entry
                .name
                .strip_prefix("part-")
                .and_then(|number| number.parse::<u32>().ok())
            else {
                continue;
            };
            if number <= marker || number > 10_000 || !entry.is_file() {
                continue;
            }
            let path = format!("{directory}/{}", entry.name);
            if let Ok(stats) = driver.stat(&path).await {
                parts.push(ListedPart {
                    number,
                    modified: format_iso_date(stats.mtime_ms),
                    etag: object_etag(&stats),
                    size: stats.size,
                });
            }
        }
        parts.sort_by_key(|part| part.number);
        let truncated = parts.len() > max_parts;
        let page = parts.into_iter().take(max_parts).collect::<Vec<_>>();
        let next = truncated
            .then(|| page.last().map(|part| part.number))
            .flatten();
        Ok(xml_response(
            200,
            list_parts_xml(ListPartsXml {
                bucket: &target.bucket,
                key: &target.key,
                upload_id,
                marker,
                max_parts,
                truncated,
                next_marker: next,
                parts: &page,
            }),
            "",
        ))
    }

    pub async fn close(&self) -> S3Result<()> {
        for driver in self.buckets.values() {
            let root = format!("/{MULTIPART_PREFIX}");
            let entries = match driver.readdir(&root).await {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.code,
                        mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(S3Failure::Fs(error)),
            };
            for entry in entries {
                let path = format!("{root}/{}", entry.name);
                if entry.is_directory() {
                    remove_tree(driver, &path).await?;
                } else {
                    let _ = driver.unlink(&path).await;
                }
            }
            let _ = driver.rmdir(&root).await;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ListingEntry {
    key: String,
    path: String,
}

#[derive(Debug)]
enum ListCandidate {
    Prefix(String),
    Object { key: String, stats: Stats },
}

impl ListCandidate {
    fn key(&self) -> &str {
        match self {
            Self::Prefix(value) => value,
            Self::Object { key, .. } => key,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct UploadManifest {
    key: String,
    mtime_ms: Option<i64>,
}

fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::ListBuckets => "ListBuckets",
        Operation::HeadBucket { .. } => "HeadBucket",
        Operation::ListObjectsV2 { .. } => "ListObjectsV2",
        Operation::GetObject(_) => "GetObject",
        Operation::HeadObject(_) => "HeadObject",
        Operation::PutObject(_) => "PutObject",
        Operation::DeleteObject(_) => "DeleteObject",
        Operation::DeleteObjects { .. } => "DeleteObjects",
        Operation::CopyObject { .. } => "CopyObject",
        Operation::CreateMultipartUpload(_) => "CreateMultipartUpload",
        Operation::UploadPart { .. } => "UploadPart",
        Operation::CompleteMultipartUpload { .. } => "CompleteMultipartUpload",
        Operation::AbortMultipartUpload { .. } => "AbortMultipartUpload",
        Operation::ListParts { .. } => "ListParts",
    }
}

fn operation_bucket(operation: &Operation) -> Option<&str> {
    match operation {
        Operation::ListBuckets => None,
        Operation::HeadBucket { bucket }
        | Operation::ListObjectsV2 { bucket, .. }
        | Operation::DeleteObjects { bucket } => Some(bucket),
        Operation::GetObject(target)
        | Operation::HeadObject(target)
        | Operation::PutObject(target)
        | Operation::DeleteObject(target)
        | Operation::CreateMultipartUpload(target)
        | Operation::UploadPart { target, .. }
        | Operation::CompleteMultipartUpload { target, .. }
        | Operation::AbortMultipartUpload { target, .. }
        | Operation::ListParts { target, .. } => Some(&target.bucket),
        Operation::CopyObject { destination, .. } => Some(&destination.bucket),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn format_iso_date(timestamp_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn object_etag(stats: &Stats) -> String {
    let input = format!(
        "{}:{}:{}:{}",
        stats.dev, stats.ino, stats.size, stats.mtime_ms
    );
    let digest = Sha256::digest(input.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}-1", &hex[..32])
}

fn upload_directory(upload_id: &str) -> String {
    format!("/{MULTIPART_PREFIX}/{upload_id}")
}

fn part_path(upload_id: &str, part_number: u32) -> String {
    format!("{}/part-{part_number}", upload_directory(upload_id))
}

fn new_upload_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_upload_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

async fn ensure_parent(driver: &Arc<dyn FsDriver>, path: &str) -> S3Result<()> {
    driver
        .mkdir(
            &dirname(path),
            MkdirOptions {
                recursive: true,
                mode: Some(0o777),
            },
        )
        .await
        .map(|_| ())
        .map_err(S3Failure::Fs)
}

async fn write_bytes(
    driver: &Arc<dyn FsDriver>,
    path: &str,
    bytes: &[u8],
    exclusive: bool,
) -> S3Result<()> {
    let flags = if exclusive { "wx" } else { "w" };
    let handle = driver
        .open(path, flags, 0o666)
        .await
        .map_err(S3Failure::Fs)?;
    let mut offset = 0_u64;
    while offset < bytes.len() as u64 {
        let written = handle
            .write(&bytes[offset as usize..], Some(offset))
            .await
            .map_err(S3Failure::Fs)?;
        if written == 0 {
            let _ = handle.close().await;
            return Err(S3Failure::s3("InternalError"));
        }
        offset += written as u64;
    }
    handle.close().await.map_err(S3Failure::Fs)?;
    Ok(())
}

async fn read_bytes_range(
    driver: Arc<dyn FsDriver>,
    path: &str,
    range: Option<ByteRange>,
    size: u64,
    chunk_bytes: usize,
) -> S3Result<Vec<u8>> {
    let handle = driver.open(path, "r", 0).await.map_err(S3Failure::Fs)?;
    let start = range.map_or(0, |range| range.start);
    let end = range.map_or(size.saturating_sub(1), |range| range.end);
    let expected = if size == 0 || end < start {
        0
    } else {
        end - start + 1
    };
    let mut output = Vec::with_capacity(expected.min(usize::MAX as u64) as usize);
    let mut position = start;
    while position <= end && expected > 0 {
        let remaining = (end - position + 1).min(chunk_bytes.max(1) as u64);
        let mut buffer = vec![0_u8; remaining as usize];
        let count = handle
            .read(&mut buffer, Some(position))
            .await
            .map_err(S3Failure::Fs)?;
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
        position += count as u64;
    }
    handle.close().await.map_err(S3Failure::Fs)?;
    Ok(output)
}

async fn apply_mtime(driver: &Arc<dyn FsDriver>, path: &str, mtime_ms: i64) -> S3Result<()> {
    if driver.capabilities().times {
        driver
            .utimes(path, mtime_ms, mtime_ms)
            .await
            .map_err(S3Failure::Fs)?;
    }
    Ok(())
}

fn remove_tree<'a>(
    driver: &'a Arc<dyn FsDriver>,
    path: &'a str,
) -> Pin<Box<dyn Future<Output = S3Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let entries = match driver.readdir(path).await {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(S3Failure::Fs(error)),
        };
        for entry in entries {
            let child = format!(
                "{}/{}",
                normalize_path(path).trim_end_matches('/'),
                entry.name
            );
            if entry.is_directory() {
                remove_tree(driver, &child).await?;
            } else {
                match driver.unlink(&child).await {
                    Ok(()) => {}
                    Err(error) if matches!(error.code, mount_rs_core::ErrorCode::Enoent) => {}
                    Err(error) => return Err(S3Failure::Fs(error)),
                }
            }
        }
        match driver.rmdir(path).await {
            Ok(()) => Ok(()),
            Err(error) if matches!(error.code, mount_rs_core::ErrorCode::Enoent) => Ok(()),
            Err(error) => Err(S3Failure::Fs(error)),
        }
    })
}

fn collect_listing(
    driver: Arc<dyn FsDriver>,
) -> Pin<Box<dyn Future<Output = S3Result<Vec<ListingEntry>>> + Send>> {
    Box::pin(async move {
        let mut output = Vec::new();
        walk_directory(driver, "/".to_owned(), String::new(), &mut output).await?;
        output.sort_by(|left, right| compare_utf8(&left.key, &right.key));
        Ok(output)
    })
}

fn walk_directory<'a>(
    driver: Arc<dyn FsDriver>,
    path: String,
    key: String,
    output: &'a mut Vec<ListingEntry>,
) -> Pin<Box<dyn Future<Output = S3Result<bool>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = driver.readdir(&path).await.map_err(S3Failure::Fs)?;
        entries.retain(|entry| {
            entry.file_type == FileType::File || entry.file_type == FileType::Directory
        });
        if path == "/" {
            entries.retain(|entry| entry.name != MULTIPART_PREFIX);
        }
        entries.sort_by(|left, right| {
            let left_key = if left.is_directory() {
                format!("{}/", left.name)
            } else {
                left.name.clone()
            };
            let right_key = if right.is_directory() {
                format!("{}/", right.name)
            } else {
                right.name.clone()
            };
            compare_utf8(&left_key, &right_key)
        });
        if entries.is_empty() {
            if !key.is_empty() {
                output.push(ListingEntry { key, path });
                return Ok(false);
            }
            return Ok(false);
        }
        let mut visible = false;
        for entry in entries {
            let child_path = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{path}/{}", entry.name)
            };
            let child_key = if key.is_empty() {
                if entry.is_directory() {
                    format!("{}/", entry.name)
                } else {
                    entry.name.clone()
                }
            } else if entry.is_directory() {
                format!("{key}{}/", entry.name)
            } else {
                format!("{key}{}", entry.name)
            };
            if entry.is_directory() {
                let before = output.len();
                let child_visible = walk_directory(
                    driver.clone(),
                    child_path.clone(),
                    child_key.clone(),
                    output,
                )
                .await?;
                visible = visible || child_visible || output.len() > before;
            } else {
                output.push(ListingEntry {
                    key: child_key,
                    path: child_path,
                });
                visible = true;
            }
        }
        Ok(visible)
    })
}

fn listable_prefix(prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if prefix.starts_with('/')
        || prefix.contains("//")
        || prefix.contains('\0')
        || prefix.starts_with(&format!("{MULTIPART_PREFIX}/"))
    {
        return false;
    }
    let components = prefix.split('/').collect::<Vec<_>>();
    !components[..components.len().saturating_sub(1)]
        .iter()
        .any(|part| *part == "." || *part == "..")
}

fn evaluate_get_conditionals(
    stats: &Stats,
    etag: &str,
    headers: &[HeaderEntry],
    method: &str,
) -> Option<u16> {
    if let Some(value) = header_list(headers, "if-match")
        && !conditional_match(&value, etag)
    {
        return Some(412);
    }
    if let Some(value) = header_value(headers, "if-unmodified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms > value + 999
    {
        return Some(412);
    }
    if let Some(value) = header_list(headers, "if-none-match")
        && conditional_match(&value, etag)
    {
        return Some(
            if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD") {
                304
            } else {
                412
            },
        );
    } else if let Some(value) = header_value(headers, "if-modified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms <= value + 999
    {
        return Some(304);
    }
    None
}

fn if_range_matches(value: &str, etag: &str, mtime_ms: i64) -> bool {
    if value.trim().starts_with('"') {
        return value.trim() == protocol::etag_header(etag);
    }
    protocol::parse_http_date(value).is_some_and(|date| mtime_ms <= date + 999)
}

fn check_put_conditionals(existing: Option<&Stats>, headers: &[HeaderEntry]) -> S3Result<()> {
    if let Some(value) = header_list(headers, "if-match") {
        let Some(existing) = existing else {
            return Err(S3Failure::s3("NoSuchKey"));
        };
        if !conditional_match(&value, &object_etag(existing)) {
            return Err(S3Failure::s3("PreconditionFailed"));
        }
    }
    if let Some(value) = header_list(headers, "if-none-match")
        && let Some(existing) = existing
        && conditional_match(&value, &object_etag(existing))
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_value(headers, "if-unmodified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && existing.is_some_and(|existing| existing.mtime_ms > value + 999)
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    Ok(())
}

fn check_create_only(headers: &[HeaderEntry]) -> bool {
    header_list(headers, "if-none-match").is_some_and(|value| value.trim() == "*")
}

fn check_copy_conditionals(stats: &Stats, etag: &str, headers: &[HeaderEntry]) -> S3Result<()> {
    if let Some(value) = header_list(headers, "x-amz-copy-source-if-match")
        && !conditional_match(&value, etag)
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_list(headers, "x-amz-copy-source-if-none-match")
        && conditional_match(&value, etag)
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_value(headers, "x-amz-copy-source-if-unmodified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms > value + 999
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    if let Some(value) = header_value(headers, "x-amz-copy-source-if-modified-since")
        .and_then(|value| protocol::parse_http_date(&value))
        && stats.mtime_ms <= value + 999
    {
        return Err(S3Failure::s3("PreconditionFailed"));
    }
    Ok(())
}

fn validate_declared_length(headers: &[HeaderEntry], actual: usize) -> S3Result<()> {
    let Some(length) = header_content_length(headers) else {
        return Err(S3Failure::s3("MissingContentLength"));
    };
    if length != actual as u64 {
        return Err(S3Failure::s3("IncompleteBody"));
    }
    Ok(())
}

fn decode_request_body(
    headers: &[HeaderEntry],
    body: &[u8],
    max_body_bytes: usize,
    verified: Option<&sigv4::VerifiedRequest>,
    credentials: Option<&Credentials>,
) -> S3Result<Vec<u8>> {
    let chunked = content_encoding_chunked(headers);
    let streaming = header_value(headers, "x-amz-content-sha256").unwrap_or_default();
    let decoded_length = header_value(headers, "x-amz-decoded-content-length")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| S3Failure::s3("InvalidArgument"))
        })
        .transpose()?;
    if !chunked {
        if streaming == sigv4::STREAMING_PAYLOAD || streaming.contains("TRAILER") {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        return Ok(body.to_vec());
    }
    if streaming.contains("TRAILER") {
        return Err(S3Failure::s3("NotImplemented"));
    }
    let signing = if streaming == sigv4::STREAMING_PAYLOAD {
        match (credentials, verified) {
            (Some(credentials), Some(verified)) if !verified.presigned => Some(ChunkSigning {
                credentials,
                verified,
            }),
            (None, _) => None,
            _ => return Err(S3Failure::s3("SignatureDoesNotMatch")),
        }
    } else {
        None
    };
    decode_aws_chunked(body, max_body_bytes, decoded_length, signing)
}

struct ChunkSigning<'a> {
    credentials: &'a Credentials,
    verified: &'a sigv4::VerifiedRequest,
}

const MAX_SIGNED_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

fn decode_aws_chunked(
    body: &[u8],
    max_body_bytes: usize,
    decoded_length: Option<u64>,
    signing: Option<ChunkSigning<'_>>,
) -> S3Result<Vec<u8>> {
    let mut cursor = 0;
    let mut output = Vec::new();
    let mut previous_signature = signing
        .as_ref()
        .map(|chunk| chunk.verified.signature.clone());
    loop {
        let Some(line_offset) = body
            .get(cursor..)
            .and_then(|remaining| remaining.windows(2).position(|pair| pair == b"\r\n"))
        else {
            return Err(S3Failure::s3("InvalidRequest"));
        };
        let line_end = cursor + line_offset;
        let size_text = std::str::from_utf8(&body[cursor..line_end])
            .map_err(|_| S3Failure::s3("InvalidRequest"))?;
        let mut extensions = size_text.split(';');
        let size_text = extensions.next().unwrap_or_default();
        if size_text.is_empty() || size_text.len() > 16 {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        let size =
            u64::from_str_radix(size_text, 16).map_err(|_| S3Failure::s3("InvalidRequest"))?;
        let provided_signature = extensions.find_map(|extension| {
            extension
                .strip_prefix("chunk-signature=")
                .map(ToOwned::to_owned)
        });
        if signing.is_some()
            && provided_signature.as_deref().is_none_or(|signature| {
                signature.len() != 64 || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(S3Failure::s3("SignatureDoesNotMatch"));
        }
        cursor = line_end + 2;
        if signing.is_some() && size > MAX_SIGNED_CHUNK_BYTES {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let size_usize = usize::try_from(size).map_err(|_| S3Failure::s3("EntityTooLarge"))?;
        if output.len().saturating_add(size_usize) > max_body_bytes {
            return Err(S3Failure::s3("EntityTooLarge"));
        }
        let end = cursor
            .checked_add(size_usize)
            .ok_or_else(|| S3Failure::s3("EntityTooLarge"))?;
        if end.checked_add(2).is_none_or(|end| end > body.len()) {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        let payload = &body[cursor..end];
        if decoded_length
            .is_some_and(|length| output.len().saturating_add(size_usize) as u64 > length)
        {
            return Err(S3Failure::s3("IncompleteBody"));
        }
        if let Some(signing) = signing.as_ref() {
            let previous = previous_signature
                .as_deref()
                .ok_or_else(|| S3Failure::s3("SignatureDoesNotMatch"))?;
            let expected = chunk_signature(signing, previous, &sigv4::sha256_hex(payload));
            let supplied = provided_signature
                .as_deref()
                .expect("signed chunk signature checked above")
                .to_ascii_lowercase();
            if expected.as_bytes().ct_eq(supplied.as_bytes()).unwrap_u8() != 1 {
                return Err(S3Failure::s3("SignatureDoesNotMatch"));
            }
            previous_signature = Some(supplied);
        }
        output.extend_from_slice(payload);
        cursor = end;
        if &body[cursor..cursor + 2] != b"\r\n" {
            return Err(S3Failure::s3("InvalidRequest"));
        }
        cursor += 2;
        if size == 0 {
            if decoded_length.is_some_and(|length| output.len() as u64 != length) {
                return Err(S3Failure::s3("IncompleteBody"));
            }
            if cursor != body.len() {
                return Err(S3Failure::s3("NotImplemented"));
            }
            return Ok(output);
        }
    }
}

fn chunk_signature(signing: &ChunkSigning<'_>, previous: &str, payload_hash: &str) -> String {
    sigv4::sign_chunk(
        &signing.credentials.secret_access_key,
        &signing.verified.scope,
        &sigv4::format_amz_date(signing.verified.timestamp_ms),
        previous,
        payload_hash,
    )
}

async fn read_manifest(
    driver: &Arc<dyn FsDriver>,
    upload_id: &str,
    key: &str,
) -> S3Result<UploadManifest> {
    if !is_upload_id(upload_id) {
        return Err(S3Failure::s3("NoSuchUpload"));
    }
    let path = format!("{}/{MANIFEST_NAME}", upload_directory(upload_id));
    let size = driver
        .stat(&path)
        .await
        .map_err(|error| match error {
            error
                if matches!(
                    error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                S3Failure::s3("NoSuchUpload")
            }
            error => S3Failure::Fs(error),
        })?
        .size;
    let bytes = read_bytes_range(driver.clone(), &path, None, size, DEFAULT_READ_CHUNK)
        .await
        .map_err(|error| match error {
            S3Failure::Fs(ref fs_error)
                if matches!(
                    fs_error.code,
                    mount_rs_core::ErrorCode::Enoent | mount_rs_core::ErrorCode::Enotdir
                ) =>
            {
                S3Failure::s3("NoSuchUpload")
            }
            other => other,
        })?;
    let manifest: UploadManifest =
        serde_json::from_slice(&bytes).map_err(|_| S3Failure::s3("NoSuchUpload"))?;
    if manifest.key != key {
        return Err(S3Failure::s3("NoSuchUpload"));
    }
    Ok(manifest)
}

fn sigv4_error(failure: SigV4Failure, presigned: bool) -> protocol::S3Error {
    match failure {
        SigV4Failure::Missing => s3_error("AccessDenied"),
        SigV4Failure::Malformed(_) => s3_error(if presigned {
            "AuthorizationQueryParametersError"
        } else {
            "AuthorizationHeaderMalformed"
        }),
        SigV4Failure::UnsupportedAlgorithm => {
            protocol::error_with_message("InvalidRequest", "Please use AWS4-HMAC-SHA256.")
        }
        SigV4Failure::UnknownAccessKey => s3_error("InvalidAccessKeyId"),
        SigV4Failure::ScopeMismatch(_) => s3_error(if presigned {
            "AuthorizationQueryParametersError"
        } else {
            "AuthorizationHeaderMalformed"
        }),
        SigV4Failure::ClockSkew => s3_error("RequestTimeTooSkewed"),
        SigV4Failure::Expired => {
            protocol::error_with_message("AccessDenied", "Request has expired")
        }
        SigV4Failure::SignatureMismatch => s3_error("SignatureDoesNotMatch"),
    }
}
