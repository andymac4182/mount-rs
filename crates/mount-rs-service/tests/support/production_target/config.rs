use serde::{Deserialize, Serialize};

pub const SERVERS: usize = 10;
pub const PHASE_SECONDS: u64 = 600;
pub const WORK_SECONDS: u64 = 1800;
pub const REQUEST_SECONDS: u64 = 30;
pub const DISK_FLOOR: u64 = 64 * 1024 * 1024 * 1024;
pub const RSS_CAP: u64 = 24 * 1024 * 1024 * 1024;
pub const PATTERNS: [&str; 8] = [
    "sequential_read",
    "random_read",
    "sequential_overwrite",
    "random_overwrite",
    "mixed",
    "hot_file",
    "append_truncate",
    "churn",
];
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub full_target: bool,
    pub drives: usize,
    pub files: usize,
    pub seconds: u64,
    pub provider: String,
}
impl Config {
    pub fn environment() -> Result<Self, String> {
        fn number(name: &str, default: usize) -> Result<usize, String> {
            match std::env::var(name) {
                Ok(s) => s.parse().map_err(|_| format!("invalid {name}")),
                Err(std::env::VarError::NotPresent) => Ok(default),
                Err(_) => Err(format!("invalid {name}")),
            }
        }
        let full_target = match std::env::var("MOUNT_RS_TARGET_MODE").as_deref() {
            Ok("full") => true,
            Ok("control") => false,
            _ => return Err("explicit MOUNT_RS_TARGET_MODE=full|control required".into()),
        };
        let config = Self {
            full_target,
            drives: number(
                "MOUNT_RS_TARGET_DRIVES",
                if full_target { 10000 } else { 10 },
            )?,
            files: number("MOUNT_RS_TARGET_FILES", 1000)?,
            seconds: number("MOUNT_RS_TARGET_SECONDS", if full_target { 30 } else { 1 })? as u64,
            provider: std::env::var("MOUNT_RS_TARGET_PROVIDER").unwrap_or_else(|_| "sqlite".into()),
        };
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.drives < 10
            || self.drives > 10000
            || !self.drives.is_multiple_of(2)
            || self.files == 0
            || self.files > 1000
            || self.seconds == 0
            || self.seconds > 30
        {
            return Err("invalid target dimensions or stage duration".into());
        }
        if self.full_target && (self.drives != 10000 || self.files != 1000 || self.seconds != 30) {
            return Err("full target refuses dimensional or duration shrinkage".into());
        }
        if self.provider != "sqlite" && self.provider != "tidb" {
            return Err("target provider must be sqlite or tidb".into());
        }
        Ok(())
    }
    pub fn active(&self, mostly_idle: bool) -> usize {
        if mostly_idle {
            (self.drives / 100).clamp(1, 100)
        } else {
            self.drives
        }
    }
}
#[test]
fn full_dimensions_cannot_be_shrunk_and_both_modes_remain_required() {
    let mut c = Config {
        full_target: true,
        drives: 10000,
        files: 1000,
        seconds: 30,
        provider: "tidb".into(),
    };
    c.validate().unwrap();
    assert_eq!(c.active(true), 100);
    assert_eq!(c.active(false), 10000);
    c.drives = 10;
    assert!(c.validate().is_err());
    c.drives = 10000;
    c.files = 2;
    assert!(c.validate().is_err());
    c.files = 1000;
    c.seconds = 1;
    assert!(c.validate().is_err());
}
