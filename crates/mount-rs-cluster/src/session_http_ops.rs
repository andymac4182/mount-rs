//! Bounded handle RPC. Control requests are JSON; writes and read responses
//! carry raw bytes. No request is automatically retried, including cursor reads.
use super::*;
use mount_rs_core::OpenFlags;
use serde::Deserialize;
const MAX_IO: usize = 65536;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Flags {
    read: bool,
    write: bool,
    create: bool,
    truncate: bool,
    append: bool,
    exclusive: bool,
}
impl From<Flags> for OpenFlags {
    fn from(f: Flags) -> Self {
        Self {
            read: f.read,
            write: f.write,
            create: f.create,
            truncate: f.truncate,
            append: f.append,
            exclusive: f.exclusive,
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Capabilities,
    Path {
        request: PathOperation,
    },
    Open {
        path: String,
        flags: Flags,
        mode: u32,
    },
    Read {
        handle: u64,
        length: usize,
        position: Option<u64>,
    },
    Stat {
        handle: u64,
    },
    Truncate {
        handle: u64,
        length: u64,
    },
    Sync {
        handle: u64,
        data_only: bool,
    },
    Close {
        handle: u64,
    },
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum PathOperation {
    Stat {
        path: String,
    },
    Lstat {
        path: String,
    },
    Statfs {
        path: String,
    },
    Readdir {
        path: String,
    },
    Readlink {
        path: String,
    },
    Mkdir {
        path: String,
        recursive: bool,
        mode: Option<u32>,
    },
    Rmdir {
        path: String,
    },
    Unlink {
        path: String,
    },
    Rename {
        source: String,
        destination: String,
    },
    Link {
        source: String,
        destination: String,
    },
    Symlink {
        target: String,
        path: String,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    Chown {
        path: String,
        uid: u32,
        gid: u32,
    },
    Lchown {
        path: String,
        uid: u32,
        gid: u32,
    },
    Truncate {
        path: String,
        length: u64,
    },
    Utimes {
        path: String,
        atime_ms: i64,
        mtime_ms: i64,
    },
    Lutimes {
        path: String,
        atime_ms: i64,
        mtime_ms: i64,
    },
    Utimens {
        path: String,
        atime_ns: String,
        mtime_ns: String,
        follow_symlinks: bool,
    },
    Mknod {
        path: String,
        mode: u32,
        dev: u64,
    },
    Syncfs,
}
fn valid_path(path: &str) -> Result<()> {
    if path.len() > 4096 || !path.starts_with('/') || path.contains('\0') {
        return Err(invalid());
    }
    Ok(())
}
impl PathOperation {
    async fn execute(self, driver: Arc<dyn mount_rs_core::FsDriver>) -> Result<Response<HttpBody>> {
        use PathOperation::*;
        match &self {
            Syncfs => {}
            Rename {
                source,
                destination,
            }
            | Link {
                source,
                destination,
            } => {
                valid_path(source)?;
                valid_path(destination)?;
            }
            Stat { path }
            | Lstat { path }
            | Statfs { path }
            | Readdir { path }
            | Readlink { path }
            | Mkdir { path, .. }
            | Rmdir { path }
            | Unlink { path }
            | Symlink { path, .. }
            | Chmod { path, .. }
            | Chown { path, .. }
            | Lchown { path, .. }
            | Truncate { path, .. }
            | Utimes { path, .. }
            | Lutimes { path, .. }
            | Utimens { path, .. }
            | Mknod { path, .. } => valid_path(path)?,
        }
        match self {
            Stat { path } => return json(driver.stat(&path).await?),
            Lstat { path } => return json(driver.lstat(&path).await?),
            Statfs { path } => return json(driver.statfs(&path).await?),
            Readdir { path } => return json(driver.readdir(&path).await?),
            Readlink { path } => return json(driver.readlink(&path).await?),
            Mkdir {
                path,
                recursive,
                mode,
            } => {
                return json(
                    driver
                        .mkdir(&path, mount_rs_core::MkdirOptions { recursive, mode })
                        .await?,
                );
            }
            Rmdir { path } => driver.rmdir(&path).await?,
            Unlink { path } => driver.unlink(&path).await?,
            Rename {
                source,
                destination,
            } => driver.rename(&source, &destination).await?,
            Link {
                source,
                destination,
            } => driver.link(&source, &destination).await?,
            Symlink { target, path } => {
                if target.len() > 4096 || target.contains('\0') {
                    return Err(invalid());
                }
                driver.symlink(&target, &path).await?;
            }
            Chmod { path, mode } => driver.chmod(&path, mode).await?,
            Chown { path, uid, gid } => driver.chown(&path, uid, gid).await?,
            Lchown { path, uid, gid } => driver.lchown(&path, uid, gid).await?,
            Truncate { path, length } => driver.truncate(&path, length).await?,
            Utimes {
                path,
                atime_ms,
                mtime_ms,
            } => driver.utimes(&path, atime_ms, mtime_ms).await?,
            Lutimes {
                path,
                atime_ms,
                mtime_ms,
            } => driver.lutimes(&path, atime_ms, mtime_ms).await?,
            Utimens {
                path,
                atime_ns,
                mtime_ns,
                follow_symlinks,
            } => {
                driver
                    .utimens(
                        &path,
                        atime_ns.parse().map_err(|_| invalid())?,
                        mtime_ns.parse().map_err(|_| invalid())?,
                        follow_symlinks,
                    )
                    .await?
            }
            Mknod { path, mode, dev } => driver.mknod(&path, mode, dev).await?,
            Syncfs => driver.syncfs().await?,
        }
        Ok(response(StatusCode::NO_CONTENT, Bytes::new()))
    }
}

fn invalid() -> FsError {
    FsError::new(ErrorCode::Einval).with_syscall("session")
}
fn token(headers: &http::HeaderMap) -> Result<SessionToken> {
    let mut values = headers.get_all(SESSION_HEADER).iter();
    let value = values.next().ok_or_else(invalid)?;
    if values.next().is_some() {
        return Err(invalid());
    }
    SessionToken::from_wire(value.to_str().map_err(|_| invalid())?)
}
fn number(headers: &http::HeaderMap, name: &str) -> Result<Option<u64>> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid());
    }
    let value = value.to_str().map_err(|_| invalid())?;
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    value.parse().map(Some).map_err(|_| invalid())
}
async fn body(mut body: HttpBody, limit: usize) -> Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(5), async move {
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| invalid())?;
            if let Some(data) = frame.data_ref() {
                if data.len() > limit - bytes.len() {
                    return Err(FsError::new(ErrorCode::Efbig).with_syscall("session"));
                }
                bytes.extend_from_slice(data);
            }
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| FsError::new(ErrorCode::Eintr).with_syscall("session"))?
}
const MAX_JSON_RESPONSE: usize = 1024 * 1024;
struct JsonBuffer {
    bytes: Vec<u8>,
    exceeded: bool,
}
impl std::io::Write for JsonBuffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if data.len() > MAX_JSON_RESPONSE - self.bytes.len() {
            self.exceeded = true;
            return Err(std::io::Error::other("session response limit"));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn json(value: impl serde::Serialize) -> Result<Response<HttpBody>> {
    let mut buffer = JsonBuffer {
        bytes: Vec::new(),
        exceeded: false,
    };
    serde_json::to_writer(&mut buffer, &value).map_err(|_| {
        FsError::new(if buffer.exceeded {
            ErrorCode::Efbig
        } else {
            ErrorCode::Eio
        })
        .with_syscall("session")
    })?;
    Ok(response(StatusCode::OK, buffer.bytes.into()))
}
pub(super) async fn serve(
    request: Request<HttpBody>,
    registry: &VolumeSessions,
    principal: &str,
    max_request_bytes: usize,
    timings: Option<&crate::owner_http::SessionOperationCounters>,
) -> Response<HttpBody> {
    match execute(request, registry, principal, max_request_bytes, timings).await {
        Ok(response) => response,
        Err(error) => filesystem_error(error),
    }
}
async fn execute(
    request: Request<HttpBody>,
    registry: &VolumeSessions,
    principal: &str,
    max_request_bytes: usize,
    timings: Option<&crate::owner_http::SessionOperationCounters>,
) -> Result<Response<HttpBody>> {
    if !matches!(*request.method(), Method::GET | Method::POST | Method::PUT) {
        let mut response = rejected(StatusCode::METHOD_NOT_ALLOWED, ErrorCode::Enosys);
        response
            .headers_mut()
            .insert(http::header::ALLOW, "GET, POST, PUT".parse().unwrap());
        return Ok(response);
    }
    let token = token(request.headers())?;
    let session = registry.lookup(principal, &token)?;
    if request.method() == Method::PUT {
        let handle = number(request.headers(), "x-mount-handle")?.ok_or_else(invalid)?;
        let position = number(request.headers(), "x-mount-position")?;
        let bytes = body(request.into_body(), MAX_IO.min(max_request_bytes)).await?;
        let started = timings.map(|_| tokio::time::Instant::now());
        let count = session.write(handle, &bytes, position).await?;
        if count > bytes.len() {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("write"));
        }
        let response = json(serde_json::json!({"written":count}))?;
        if let Some((counters, start)) = timings.zip(started) {
            counters.write.record(start);
        }
        return Ok(response);
    }
    let operation = if request.method() == Method::GET {
        let handle = number(request.headers(), "x-mount-handle")?.ok_or_else(invalid)?;
        let position = number(request.headers(), "x-mount-position")?.ok_or_else(invalid)?;
        let length = number(request.headers(), "x-mount-length")?.ok_or_else(invalid)?;
        if length > MAX_IO as u64 {
            return Err(FsError::new(ErrorCode::Efbig).with_syscall("read"));
        }
        body(request.into_body(), 0).await?;
        Operation::Read {
            handle,
            length: length as usize,
            position: Some(position),
        }
    } else {
        if request.headers().contains_key("x-mount-handle")
            || request.headers().contains_key("x-mount-position")
            || request.headers().contains_key("x-mount-length")
        {
            return Err(invalid());
        }
        let bytes = body(request.into_body(), 8192.min(max_request_bytes)).await?;
        serde_json::from_slice(&bytes).map_err(|_| invalid())?
    };
    let counter = timings.and_then(|c| match &operation {
        Operation::Path {
            request: PathOperation::Stat { .. },
        }
        | Operation::Stat { .. } => Some(&c.stat),
        Operation::Read { .. } => Some(&c.read),
        Operation::Path {
            request: PathOperation::Chmod { .. },
        } => Some(&c.chmod),
        _ => None,
    });
    let measurement = counter.map(|counter| (counter, tokio::time::Instant::now()));
    let result = match operation {
        Operation::Capabilities => session.with_driver(|driver| async move {
            json(serde_json::json!({"capabilities":driver.capabilities(), "utimens":driver.has_utimens(), "max_io_bytes":MAX_IO.min(max_request_bytes),"positional_read_headers":true}))
        }).await,
        Operation::Path { request } => session.with_driver(|driver| request.execute(driver)).await,
        Operation::Open { path, flags, mode } => {
            if path.len() > 4096 || !path.starts_with('/') || path.contains('\0') {
                return Err(invalid());
            }
            let handle = session.open_flags(&path, flags.into(), mode).await?;
            json(serde_json::json!({"handle":handle}))
        }
        Operation::Read {
            handle,
            length,
            position,
        } => {
            if length > MAX_IO {
                return Err(FsError::new(ErrorCode::Efbig).with_syscall("read"));
            }
            let mut buffer = vec![0; length];
            let count = session.read(handle, &mut buffer, position).await?;
            if count > length {
                return Err(FsError::new(ErrorCode::Eio).with_syscall("read"));
            }
            buffer.truncate(count);
            let mut response = response(StatusCode::OK, buffer.into());
            response.headers_mut().insert(
                http::header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            Ok(response)
        }
        Operation::Stat { handle } => json(session.stat(handle).await?),
        Operation::Truncate { handle, length } => {
            session.truncate(handle, length).await?;
            Ok(response(StatusCode::NO_CONTENT, Bytes::new()))
        }
        Operation::Sync { handle, data_only } => {
            session.sync(handle, data_only).await?;
            Ok(response(StatusCode::NO_CONTENT, Bytes::new()))
        }
        Operation::Close { handle } => {
            session.close_handle(handle).await?;
            Ok(response(StatusCode::NO_CONTENT, Bytes::new()))
        }
    };
    if result.is_ok()
        && let Some((counter, start)) = measurement
    {
        counter.record(start);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_response_limit_rejects_without_partial_success() {
        assert!(json("small").is_ok());
        assert_eq!(
            json("x".repeat(MAX_JSON_RESPONSE)).err().unwrap().code,
            ErrorCode::Efbig
        );
    }
}
