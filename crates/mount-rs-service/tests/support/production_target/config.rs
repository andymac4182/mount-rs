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
    #[serde(default = "default_population_seconds")]
    pub population_seconds: u64,
    pub provider: String,
}
fn default_population_seconds() -> u64 {
    PHASE_SECONDS
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
            population_seconds: number(
                "MOUNT_RS_TARGET_POPULATION_SECONDS",
                PHASE_SECONDS as usize,
            )? as u64,
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
        if self.population_seconds == 0 || self.population_seconds > WORK_SECONDS {
            return Err("population allowance must be between 1 and enclosing work seconds".into());
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
        population_seconds: PHASE_SECONDS,
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

#[test]
fn production_population_budget_defaults_when_omitted_and_serializes_explicitly() {
    let old = serde_json::json!({
        "full_target":false,"drives":10,"files":2,"seconds":1,"provider":"sqlite"
    });
    let mut config: Config = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(config.population_seconds, PHASE_SECONDS);
    config.validate().unwrap();
    config.population_seconds = 900;
    let encoded = serde_json::to_value(&config).unwrap();
    assert_eq!(encoded["population_seconds"], 900);
    let decoded: Config = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.population_seconds, 900);
    decoded.validate().unwrap();
    let mut unknown = old;
    unknown["unrecognized_budget"] = serde_json::json!(900);
    assert!(serde_json::from_value::<Config>(unknown).is_err());
}

#[test]
fn production_population_budget_rejects_zero_excessive_and_overflow_values() {
    let mut config: Config = serde_json::from_value(serde_json::json!({
        "full_target":false,"drives":10,"files":2,"seconds":1,"provider":"tidb"
    }))
    .unwrap();
    for seconds in [0, WORK_SECONDS + 1, u64::MAX] {
        config.population_seconds = seconds;
        assert!(
            config.validate().is_err(),
            "accepted population allowance {seconds}"
        );
    }
    for seconds in [1, 60, PHASE_SECONDS, 900, WORK_SECONDS] {
        config.population_seconds = seconds;
        config.validate().unwrap();
    }
    assert!(serde_json::from_str::<Config>(
        r#"{"full_target":false,"drives":10,"files":2,"seconds":1,"provider":"tidb","population_seconds":18446744073709551616}"#
    ).is_err());
}

#[test]
fn production_population_budget_does_not_relax_full_geometry_or_stage_duration() {
    let mut config: Config = serde_json::from_value(serde_json::json!({
        "full_target":true,"drives":10000,"files":1000,"seconds":30,
        "provider":"tidb","population_seconds":900
    }))
    .unwrap();
    config.validate().unwrap();
    for population in [1, PHASE_SECONDS, WORK_SECONDS] {
        config.population_seconds = population;
        config.drives = 10;
        assert!(config.validate().is_err());
        config.drives = 10000;
        config.files = 2;
        assert!(config.validate().is_err());
        config.files = 1000;
        config.seconds = 1;
        assert!(config.validate().is_err());
        config.seconds = 30;
        config.validate().unwrap();
    }
}
