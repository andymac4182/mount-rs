use super::{backend::Backend, state::Expected};
use std::{collections::BTreeSet, time::Duration};
#[derive(Default)]
pub struct Owner {
    context: Option<mount_rs_sdk::StorageContext>,
    filesystem: Option<mount_rs_sdk::Filesystem>,
    handle: Option<std::sync::Arc<dyn mount_rs_core::FileHandle>>,
}
impl Owner {
    /// Establish only empty persistent root/backing state; no namespace/payload preseed.
    pub async fn initialize_empty(
        &mut self,
        backend: &Backend,
        drive: usize,
    ) -> Result<serde_json::Value, String> {
        if self.context.is_none() {
            self.context = Some(
                mount_rs_sdk::StorageContext::new(16)
                    .map_err(|_| "empty initializer context failed")?,
            );
        }
        self.filesystem = Some(backend.open(drive, self.context.as_ref().unwrap()).await?);
        let fs = self.filesystem.as_ref().unwrap();
        let entries = fs
            .driver()
            .readdir("/")
            .await
            .map_err(|_| "empty initializer membership failed")?;
        if entries.iter().any(|e| e.name != "." && e.name != "..") {
            return Err("empty initializer found unexpected namespace".into());
        }
        let mut receipt = backend.receipt(drive).await?;
        super::process::validate_backing_receipt(&receipt, drive)?;
        fs.shutdown()
            .await
            .map_err(|_| "empty initializer filesystem close failed")?;
        self.filesystem = None;
        receipt["empty_root_verified"] = serde_json::json!(true);
        receipt["namespace_entries"] = serde_json::json!(0);
        receipt["filesystem_closed"] = serde_json::json!(true);
        Ok(receipt)
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let closed = tokio::time::timeout(Duration::from_secs(30), async {
            let mut failed = false;
            if let Some(handle) = &self.handle {
                failed |= handle.close().await.is_err();
            }
            if let Some(fs) = &self.filesystem {
                failed |= fs.shutdown().await.is_err();
            }
            if let Some(context) = &self.context {
                failed |= context.close().await.is_err();
            }
            if failed {
                Err("fresh oracle close failed".to_string())
            } else {
                Ok(())
            }
        })
        .await
        .map_err(|_| "fresh oracle cleanup unproven".to_string())
        .and_then(|r| r);
        if closed.is_ok() {
            self.handle = None;
            self.filesystem = None;
            self.context = None;
        }
        closed
    }
}
pub async fn verify(
    owner: &mut Owner,
    backend: &Backend,
    expected: &Expected,
) -> Result<(u64, u64), String> {
    owner.context =
        Some(mount_rs_sdk::StorageContext::new(16).map_err(|_| "oracle context failed")?);
    let result = tokio::time::timeout(Duration::from_secs(600), async {
        owner.filesystem = Some(
            backend
                .open(expected.drive, owner.context.as_ref().unwrap())
                .await?,
        );
        let driver = owner.filesystem.as_ref().unwrap().driver();
        backend.receipt(expected.drive).await?;
        let actual: BTreeSet<_> = driver
            .readdir("/")
            .await
            .map_err(|_| "fresh membership read failed")?
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| e.name)
            .collect();
        let names = expected.files.keys().cloned().collect();
        if actual != names {
            return Err("fresh namespace membership mismatch".into());
        }
        let mut verified_bytes = 0;
        let mut verified_files = 0;
        for (name, file) in &expected.files {
            owner.handle = Some(
                driver
                    .open(&format!("/{name}"), "r", 0)
                    .await
                    .map_err(|_| "fresh oracle open failed")?,
            );
            let handle = owner.handle.as_ref().unwrap();
            let checked: Result<(), String> = async {
                if handle
                    .stat()
                    .await
                    .map_err(|_| "fresh oracle stat failed")?
                    .size
                    != file.length as u64
                {
                    return Err("fresh length mismatch".into());
                }
                let mut data = [0; 4096];
                for block in 0..file.length / 4096 {
                    if handle
                        .read(&mut data, Some((block * 4096) as u64))
                        .await
                        .map_err(|_| "fresh read failed")?
                        != 4096
                        || data != expected.bytes(name, block)
                    {
                        return Err("fresh every-byte oracle mismatch".into());
                    }
                    verified_bytes += 4096;
                }
                if handle
                    .read(&mut data, Some(file.length as u64))
                    .await
                    .map_err(|_| "fresh EOF read failed")?
                    != 0
                {
                    return Err("fresh EOF mismatch".into());
                }
                Ok(())
            }
            .await;
            let close = handle
                .close()
                .await
                .map_err(|_| "fresh handle close failed");
            checked?;
            close?;
            owner.handle = None;
            verified_files += 1;
        }
        Ok((verified_files, verified_bytes))
    })
    .await
    .map_err(|_| "fresh oracle deadline".to_string())
    .and_then(|r| r);
    owner.close().await?;
    result
}
