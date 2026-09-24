//! One-Partition session routing with a fresh catalog authorization check.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{FsDriver, MkdirOptions};
use mount_rs_remote_protocol::{Operation, OperationName, WireError};
use serde_json::{Value, json};

use crate::auth::authorize_drive;
use crate::catalog::{CatalogStore, DriveKey, IssuerPolicyDefinition, Permission};

#[derive(Debug, Clone)]
pub struct SessionIdentity {
    pub partition_id: String,
    pub policy_id: String,
    pub issuer: String,
    pub subject: String,
    pub claims: Value,
    pub expires_at: i64,
}

pub struct DriveDispatcher {
    catalog: Arc<dyn CatalogStore>,
    drives: BTreeMap<DriveKey, Arc<dyn FsDriver>>,
}

impl DriveDispatcher {
    #[must_use]
    pub fn new(catalog: Arc<dyn CatalogStore>) -> Self {
        Self {
            catalog,
            drives: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        partition_id: &str,
        drive_id: &str,
        driver: Arc<dyn FsDriver>,
    ) -> Result<(), &'static str> {
        let key = DriveKey {
            partition_id: partition_id.to_owned(),
            drive_id: drive_id.to_owned(),
        };
        if self.drives.contains_key(&key) {
            return Err("duplicate Drive registration");
        }
        self.drives.insert(key, driver);
        Ok(())
    }

    pub async fn dispatch(
        &self,
        identity: &SessionIdentity,
        drive_id: &str,
        operation: &Operation,
    ) -> Result<Value, WireError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| error("EIO"))?
            .as_secs() as i64;
        if now > identity.expires_at {
            return Err(error("EACCES"));
        }
        let catalog = self
            .catalog
            .load_current()
            .await
            .map_err(|_| error("EACCES"))?;
        let policy: IssuerPolicyDefinition = catalog
            .issuer_policies
            .get(&identity.policy_id)
            .cloned()
            .ok_or_else(|| error("EACCES"))
            .and_then(|value| serde_json::from_value(value).map_err(|_| error("EACCES")))?;
        if policy.issuer != identity.issuer {
            return Err(error("EACCES"));
        }
        let permission = authorize_drive(
            &catalog,
            &identity.policy_id,
            &identity.claims,
            &identity.partition_id,
            drive_id,
        )
        .ok_or_else(|| error("EACCES"))?;
        if operation.required_permission() == mount_rs_remote_protocol::Permission::Write
            && permission != Permission::Write
        {
            return Err(error("EACCES"));
        }
        let key = DriveKey {
            partition_id: identity.partition_id.clone(),
            drive_id: drive_id.to_owned(),
        };
        let driver = self.drives.get(&key).ok_or_else(|| error("EACCES"))?;
        match operation.name {
            OperationName::Capabilities => {
                serde_json::to_value(driver.capabilities()).map_err(|_| error("EIO"))
            }
            OperationName::Stat => {
                let path = path(&operation.body)?;
                serde_json::to_value(driver.stat(path).await.map_err(fs_error)?)
                    .map_err(|_| error("EIO"))
            }
            OperationName::Lstat => {
                let path = path(&operation.body)?;
                serde_json::to_value(driver.lstat(path).await.map_err(fs_error)?)
                    .map_err(|_| error("EIO"))
            }
            OperationName::Mkdir => {
                let path = path(&operation.body)?;
                let mode = operation
                    .body
                    .get("mode")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| error("EINVAL"))?;
                let created = driver
                    .mkdir(
                        path,
                        MkdirOptions {
                            recursive: false,
                            mode: Some(mode),
                        },
                    )
                    .await
                    .map_err(fs_error)?;
                Ok(json!({"created":created}))
            }
            _ => Err(error("ENOSYS")),
        }
    }
}

fn path(body: &Value) -> Result<&str, WireError> {
    let path = body
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| error("EINVAL"))?;
    if !path.starts_with('/')
        || path.contains('\0')
        || mount_rs_core::path::normalize_path(path) != path
    {
        return Err(error("EINVAL"));
    }
    Ok(path)
}

fn error(code: &str) -> WireError {
    WireError {
        code: code.to_owned(),
    }
}

fn fs_error(error_value: mount_rs_core::FsError) -> WireError {
    error(error_value.code.as_str())
}
