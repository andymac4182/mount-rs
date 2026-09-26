use super::{backend::Backend, state::Expected};
use std::{collections::BTreeSet, time::Duration};
pub struct Owner {
    pub accounting: super::metrics::Accounting,
    context: Option<mount_rs_sdk::StorageContext>,
    filesystem: Option<mount_rs_sdk::Filesystem>,
    handle: Option<std::sync::Arc<dyn mount_rs_core::FileHandle>>,
}
impl Default for Owner {
    fn default() -> Self {
        Self {
            context: None,
            filesystem: None,
            handle: None,
            accounting: super::metrics::Accounting::new(
                mount_rs_core::diagnostics::profile::enabled(),
            ),
        }
    }
}
async fn observed<T, E>(
    accounting: &super::metrics::Accounting,
    category: &'static str,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let span = accounting.begin(category);
    let result = future.await;
    span.finish(result.is_ok(), 0);
    result
}
impl Owner {
    /// Establish only empty persistent root/backing state; no namespace/payload preseed.
    pub async fn initialize_empty(
        &mut self,
        backend: &Backend,
        drive: usize,
    ) -> Result<serde_json::Value, String> {
        if self.context.is_none() {
            let span = self.accounting.begin("context_open");
            let context = mount_rs_sdk::StorageContext::new(16);
            span.finish(context.is_ok(), 0);
            self.context = Some(context.map_err(|_| "empty initializer context failed")?);
        }
        self.filesystem = Some(
            observed(
                &self.accounting,
                "filesystem_open",
                backend.open(drive, self.context.as_ref().unwrap()),
            )
            .await?,
        );
        let fs = self.filesystem.as_ref().unwrap();
        let entries = observed(&self.accounting, "membership", fs.driver().readdir("/"))
            .await
            .map_err(|_| "empty initializer membership failed")?;
        if entries.iter().any(|e| e.name != "." && e.name != "..") {
            return Err("empty initializer found unexpected namespace".into());
        }
        let mut receipt =
            observed(&self.accounting, "backing_receipt", backend.receipt(drive)).await?;
        super::process::validate_backing_receipt(&receipt, drive)?;
        observed(&self.accounting, "filesystem_context_close", fs.shutdown())
            .await
            .map_err(|_| "empty initializer filesystem close failed")?;
        self.filesystem = None;
        receipt["empty_root_verified"] = serde_json::json!(true);
        receipt["namespace_entries"] = serde_json::json!(0);
        receipt["filesystem_closed"] = serde_json::json!(true);
        Ok(receipt)
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let close_span = self.accounting.begin("filesystem_context_close");
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
        close_span.finish(closed.is_ok(), 0);
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
    let metrics = owner.accounting.clone();
    let span = metrics.begin("context_open");
    let context = mount_rs_sdk::StorageContext::new(16);
    span.finish(context.is_ok(), 0);
    owner.context = Some(context.map_err(|_| "oracle context failed")?);
    let result = tokio::time::timeout(Duration::from_secs(600), async {
        owner.filesystem = Some(
            observed(
                &metrics,
                "filesystem_open",
                backend.open(expected.drive, owner.context.as_ref().unwrap()),
            )
            .await?,
        );
        let driver = owner.filesystem.as_ref().unwrap().driver();
        observed(&metrics, "backing_receipt", backend.receipt(expected.drive)).await?;
        observed(&metrics, "membership", async {
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
                Err("fresh namespace membership mismatch")
            } else {
                Ok(())
            }
        })
        .await?;
        let mut verified_bytes = 0;
        let mut verified_files = 0;
        for (name, file) in &expected.files {
            owner.handle = Some(
                observed(
                    &metrics,
                    "file_open",
                    driver.open(&format!("/{name}"), "r", 0),
                )
                .await
                .map_err(|_| "fresh oracle open failed")?,
            );
            let handle = owner.handle.as_ref().unwrap();
            let checked: Result<(), String> = async {
                observed(&metrics, "stat", async {
                    if handle
                        .stat()
                        .await
                        .map_err(|_| "fresh oracle stat failed")?
                        .size
                        != file.length as u64
                    {
                        Err("fresh length mismatch")
                    } else {
                        Ok(())
                    }
                })
                .await?;
                let mut data = [0; 4096];
                for block in 0..file.length / 4096 {
                    let span = metrics.begin("data_read");
                    let read = handle.read(&mut data, Some((block * 4096) as u64)).await;
                    span.finish(read.is_ok(), read.as_ref().copied().unwrap_or(0) as u64);
                    if read.map_err(|_| "fresh read failed")? != 4096 {
                        return Err("fresh every-byte oracle mismatch".into());
                    }
                    let span = metrics.begin("expected_compare");
                    let matches = data == expected.bytes(name, block);
                    span.finish(matches, 4096);
                    if !matches {
                        return Err("fresh every-byte oracle mismatch".into());
                    }
                    verified_bytes += 4096;
                }
                observed(&metrics, "eof", async {
                    if handle
                        .read(&mut data, Some(file.length as u64))
                        .await
                        .map_err(|_| "fresh EOF read failed")?
                        != 0
                    {
                        Err("fresh EOF mismatch")
                    } else {
                        Ok(())
                    }
                })
                .await?;
                Ok(())
            }
            .await;
            let close = observed(&metrics, "handle_close", handle.close())
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
