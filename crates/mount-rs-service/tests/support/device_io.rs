//! Opt-in boundary observations; OS accounting is not physical flash I/O.
use serde::{Serialize, Serializer};

// Checked against installed Darwin SDK V2, including its two final fields.
#[cfg(target_os = "macos")]
const _: () = {
    assert!(std::mem::size_of::<libc::rusage_info_v2>() == 160);
    assert!(std::mem::offset_of!(libc::rusage_info_v2, ri_diskio_bytesread) == 144);
    assert!(std::mem::offset_of!(libc::rusage_info_v2, ri_diskio_byteswritten) == 152);
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct LinuxProcessCounters {
    #[serde(serialize_with = "counter_string")]
    rchar: u64,
    #[serde(serialize_with = "counter_string")]
    wchar: u64,
    #[serde(serialize_with = "counter_string")]
    syscr: u64,
    #[serde(serialize_with = "counter_string")]
    syscw: u64,
    #[serde(serialize_with = "counter_string")]
    read_bytes: u64,
    #[serde(serialize_with = "counter_string")]
    write_bytes: u64,
    #[serde(serialize_with = "counter_string")]
    cancelled_write_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ProcessIdentity {
    pid: u32,
    #[serde(serialize_with = "counter_string")]
    start_token: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct BlockCounters {
    #[serde(serialize_with = "counter_string")]
    read_ops_completed: u64,
    #[serde(serialize_with = "counter_string")]
    write_ops_completed: u64,
    #[serde(serialize_with = "counter_string")]
    sectors_read: u64,
    #[serde(serialize_with = "counter_string")]
    sectors_written: u64,
    #[serde(serialize_with = "counter_string")]
    read_bytes: u64,
    #[serde(serialize_with = "counter_string")]
    write_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct DeviceIdentity {
    name: String,
    major: u32,
    minor: u32,
    #[serde(serialize_with = "counter_string")]
    disk_sequence: u64,
    boot_id: String,
}

fn counter_string<S: Serializer>(counter: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&counter.to_string())
}

#[derive(Clone, Copy)]
enum MacNumberRead {
    Missing,
    NotNumber,
    NotInteger,
    Read { exact: bool, signed_value: i64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct MacDriverCounters {
    #[serde(serialize_with = "counter_string")]
    read_operations_processed: u64,
    #[serde(serialize_with = "counter_string")]
    write_operations_processed: u64,
    #[serde(serialize_with = "counter_string")]
    read_bytes: u64,
    #[serde(serialize_with = "counter_string")]
    write_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct MacDriverIdentity {
    #[serde(serialize_with = "counter_string")]
    registry_entry_id: u64,
    observer_process: ProcessIdentity,
}
struct MacDriverRead {
    registry_entry_id: Option<u64>,
    is_block_storage_driver: bool,
    numbers: Option<[MacNumberRead; 4]>,
}
fn parse_registry_entry_id(text: &str) -> Result<u64, &'static str> {
    let id = decimal_u64(text)?;
    if id == 0 || text.starts_with('0') {
        return Err("selected IORegistry ID must be canonical nonzero decimal");
    }
    Ok(id)
}
fn decode_mac_driver(
    selected_id: u64,
    read: MacDriverRead,
) -> Result<MacDriverCounters, &'static str> {
    if selected_id == 0 || read.registry_entry_id != Some(selected_id) {
        return Err("selected macOS driver identity unavailable or changed");
    }
    if !read.is_block_storage_driver {
        return Err("selected macOS registry entry is not a block storage driver");
    }
    let numbers = read
        .numbers
        .ok_or("selected macOS driver statistics unavailable")?;
    let number = |value| match value {
        MacNumberRead::Read {
            exact: true,
            signed_value,
        } if signed_value >= 0 => Ok(signed_value as u64),
        _ => Err("selected macOS driver counter missing, wrong type or lossy"),
    };
    let [read_ops, write_ops, read_bytes, write_bytes] = numbers.map(number);
    Ok(MacDriverCounters {
        read_operations_processed: read_ops?,
        write_operations_processed: write_ops?,
        read_bytes: read_bytes?,
        write_bytes: write_bytes?,
    })
}
fn checked_mac_driver_delta(
    before_identity: &MacDriverIdentity,
    before: &MacDriverCounters,
    after_identity: &MacDriverIdentity,
    after: &MacDriverCounters,
) -> Result<MacDriverCounters, &'static str> {
    if before_identity != after_identity {
        return Err("selected macOS driver or observer identity changed");
    }
    macro_rules! delta {
        ($field:ident) => {
            after
                .$field
                .checked_sub(before.$field)
                .ok_or("OS block I/O counter reset")?
        };
    }
    Ok(MacDriverCounters {
        read_operations_processed: delta!(read_operations_processed),
        write_operations_processed: delta!(write_operations_processed),
        read_bytes: delta!(read_bytes),
        write_bytes: delta!(write_bytes),
    })
}

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Observation<T> {
    Disabled,
    Unselected,
    Unsupported { reason: &'static str },
    Unavailable { reason: &'static str },
    Available { sample: T },
}
#[derive(Clone, Serialize)]
struct ProcessRecord {
    source: &'static str,
    identity: ProcessIdentity,
    #[serde(serialize_with = "counter_string")]
    read_bytes: u64,
    #[serde(serialize_with = "counter_string")]
    write_bytes: u64,
    linux_counters: Option<LinuxProcessCounters>,
}
#[derive(Clone, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
enum DeviceRecord {
    LinuxBlockStat {
        identity: DeviceIdentity,
        counters: BlockCounters,
    },
    DarwinIoKitStatistics {
        identity: MacDriverIdentity,
        counters: MacDriverCounters,
    },
}
#[derive(Serialize)]
pub struct Snapshot {
    enabled: bool,
    process_disk: Observation<ProcessRecord>,
    host_block_device: Observation<DeviceRecord>,
    #[serde(serialize_with = "optional_counter_string")]
    observer_elapsed_ns: Option<u64>,
    #[serde(skip)]
    observed_at: Option<std::time::Instant>,
}
impl Snapshot {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            process_disk: Observation::Disabled,
            host_block_device: Observation::Disabled,
            observer_elapsed_ns: None,
            observed_at: None,
        }
    }
    pub fn capture_from_env() -> Self {
        let enabled = std::env::var_os("MOUNT_RS_PROFILE_IO").is_some_and(|value| value == "1");
        collect_when_enabled(enabled, || {
            let started = std::time::Instant::now();
            let process_disk = collect_process_disk();
            let host_block_device = collect_selected_device(&process_disk);
            Self {
                enabled: true,
                process_disk,
                host_block_device,
                observer_elapsed_ns: u64::try_from(started.elapsed().as_nanos()).ok(),
                observed_at: Some(started),
            }
        })
        .unwrap_or_else(Self::disabled)
    }

    pub fn delta(&self, before: &Self) -> serde_json::Value {
        let interval = self
            .observed_at
            .zip(before.observed_at)
            .and_then(|(after, before)| after.checked_duration_since(before))
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
        self.delta_for_interval(before, interval)
    }

    fn delta_for_interval(&self, before: &Self, interval_ns: Option<u64>) -> serde_json::Value {
        let interval_ns = interval_ns.filter(|interval| *interval != 0);
        let mut process = interval_observation(
            &before.process_disk,
            &self.process_disk,
            interval_ns,
            checked_process_record_delta,
        );
        process["scope"] = serde_json::json!(
            "OS accounting for one observer PID; character/syscall counters include cached and non-disk I/O; Linux write bytes are charged when dirtied and cancelled bytes remain separate; no child/container/remote-datastore or physical-flash attribution implied"
        );
        let mut device = interval_observation(
            &before.host_block_device,
            &self.host_block_device,
            interval_ns,
            checked_device_record_delta,
        );
        device["scope"] = serde_json::json!(
            "one explicitly selected block device or driver, shared by all processes and kernel writeback; identity does not establish datastore/file/VM mapping; no summation or physical-flash attribution"
        );
        serde_json::json!({
            "schema":"mount-rs-os-io-v1", "enabled_start":before.enabled,"enabled_end":self.enabled,
            "interval_ns":interval_ns.map(|value| value.to_string()),
            "observer_elapsed_start_ns":before.observer_elapsed_ns.map(|value| value.to_string()),
            "observer_elapsed_end_ns":self.observer_elapsed_ns.map(|value| value.to_string()),
            "sampling_window":"boundary capture start to start; sequential observations, observer work included; no atomic multi-counter or API wall-time bound",
            "process_disk":process, "host_block_device":device,
        })
    }
}

fn optional_counter_string<S: Serializer>(
    counter: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match counter {
        Some(value) => serializer.serialize_some(&value.to_string()),
        None => serializer.serialize_none(),
    }
}

fn interval_observation<T: Serialize, D: Serialize>(
    before: &Observation<T>,
    after: &Observation<T>,
    interval_ns: Option<u64>,
    checked_delta: impl FnOnce(&T, &T) -> Result<D, &'static str>,
) -> serde_json::Value {
    let mut result =
        serde_json::json!({"before":before,"after":after,"complete":false,"counters":null});
    let (status, reason) = match (before, after) {
        (Observation::Available { sample: before }, Observation::Available { sample: after }) => {
            if interval_ns.is_none() {
                ("unavailable", Some("OS I/O interval unavailable"))
            } else {
                match checked_delta(before, after) {
                    Ok(delta) => {
                        result["complete"] = serde_json::json!(true);
                        result["counters"] =
                            serde_json::to_value(delta).expect("fixed OS counters serialize");
                        ("available", None)
                    }
                    Err(reason) => ("unavailable", Some(reason)),
                }
            }
        }
        (Observation::Disabled, Observation::Disabled) => {
            ("disabled", Some("OS I/O profiling disabled"))
        }
        (Observation::Unselected, Observation::Unselected) => (
            "unselected",
            Some("no explicit block device or driver selected"),
        ),
        (_, Observation::Unsupported { reason }) => ("unsupported", Some(*reason)),
        (_, Observation::Unavailable { reason }) => ("unavailable", Some(*reason)),
        (Observation::Unsupported { reason }, _) => ("unsupported", Some(*reason)),
        (Observation::Unavailable { reason }, _) => ("unavailable", Some(*reason)),
        _ => ("unavailable", Some("OS I/O observation coverage changed")),
    };
    result["status"] = serde_json::json!(status);
    result["reason"] = serde_json::json!(reason);
    result
}

fn checked_process_record_delta(
    before: &ProcessRecord,
    after: &ProcessRecord,
) -> Result<serde_json::Value, &'static str> {
    if before.identity != after.identity || before.source != after.source {
        return Err("OS process identity or source changed");
    }
    let linux_counters = match (&before.linux_counters, &after.linux_counters) {
        (Some(before_counters), Some(after_counters)) => Some(checked_process_delta(
            &before.identity,
            before_counters,
            &after.identity,
            after_counters,
        )?),
        (None, None) => None,
        _ => return Err("OS process I/O counter coverage changed"),
    };
    let read_bytes = after
        .read_bytes
        .checked_sub(before.read_bytes)
        .ok_or("OS process I/O counter reset")?;
    let write_bytes = after
        .write_bytes
        .checked_sub(before.write_bytes)
        .ok_or("OS process I/O counter reset")?;
    Ok(
        serde_json::json!({"read_bytes":read_bytes.to_string(),"write_bytes":write_bytes.to_string(),"linux_counters":linux_counters}),
    )
}

fn checked_device_record_delta(
    before: &DeviceRecord,
    after: &DeviceRecord,
) -> Result<serde_json::Value, &'static str> {
    match (before, after) {
        (
            DeviceRecord::LinuxBlockStat {
                identity: before_id,
                counters: before,
            },
            DeviceRecord::LinuxBlockStat {
                identity: after_id,
                counters: after,
            },
        ) => Ok(serde_json::json!(checked_block_delta(
            before_id, before, after_id, after
        )?)),
        (
            DeviceRecord::DarwinIoKitStatistics {
                identity: before_id,
                counters: before,
            },
            DeviceRecord::DarwinIoKitStatistics {
                identity: after_id,
                counters: after,
            },
        ) => Ok(serde_json::json!(checked_mac_driver_delta(
            before_id, before, after_id, after
        )?)),
        _ => Err("OS block counter source changed"),
    }
}

fn decimal_u64(text: &str) -> Result<u64, &'static str> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid unsigned OS counter");
    }
    text.parse().map_err(|_| "OS counter overflow")
}
fn parse_linux_process_io(text: &str) -> Result<LinuxProcessCounters, &'static str> {
    const NAMES: [&str; 7] = [
        "rchar",
        "wchar",
        "syscr",
        "syscw",
        "read_bytes",
        "write_bytes",
        "cancelled_write_bytes",
    ];
    if text.len() > 16384 {
        return Err("OS process I/O record too large");
    }
    let mut values = [None; 7];
    for line in text.lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or("invalid OS process I/O record")?;
        if let Some(index) = NAMES.iter().position(|candidate| *candidate == name) {
            if values[index].is_some() {
                return Err("duplicate OS process I/O counter");
            }
            values[index] = Some(decimal_u64(value.trim())?);
        }
    }
    let [
        rchar,
        wchar,
        syscr,
        syscw,
        read_bytes,
        write_bytes,
        cancelled_write_bytes,
    ] = values.map(|value| value.ok_or("missing OS process I/O counter"));
    Ok(LinuxProcessCounters {
        rchar: rchar?,
        wchar: wchar?,
        syscr: syscr?,
        syscw: syscw?,
        read_bytes: read_bytes?,
        write_bytes: write_bytes?,
        cancelled_write_bytes: cancelled_write_bytes?,
    })
}
fn parse_linux_process_identity(text: &str, pid: u32) -> Result<ProcessIdentity, &'static str> {
    let comm_start = text.find('(').ok_or("OS process identity missing comm")?;
    let comm_end = text.rfind(')').ok_or("OS process identity missing comm")?;
    if comm_end < comm_start || decimal_u64(text[..comm_start].trim())? != u64::from(pid) {
        return Err("OS process PID identity mismatch");
    }
    // Field3 is state, field22 is starttime; comm may itself contain ')' or spaces.
    let start = text[comm_end + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or("OS process start identity missing")?;
    Ok(ProcessIdentity {
        pid,
        start_token: decimal_u64(start)?,
    })
}
fn parse_linux_block_stat(text: &str) -> Result<BlockCounters, &'static str> {
    if text.len() > 4096 {
        return Err("OS block statistics record too large");
    }
    let mut fields = [0; 32];
    let mut count = 0;
    for token in text.split_whitespace() {
        if count == fields.len() {
            return Err("OS block statistics field count unsupported");
        }
        fields[count] = decimal_u64(token)?;
        count += 1;
    }
    if count < 11 {
        return Err("OS block statistics incomplete");
    }
    Ok(BlockCounters {
        read_ops_completed: fields[0],
        write_ops_completed: fields[4],
        sectors_read: fields[2],
        sectors_written: fields[6],
        read_bytes: fields[2]
            .checked_mul(512)
            .ok_or("OS block byte counter overflow")?,
        write_bytes: fields[6]
            .checked_mul(512)
            .ok_or("OS block byte counter overflow")?,
    })
}
fn checked_process_delta(
    before_identity: &ProcessIdentity,
    before: &LinuxProcessCounters,
    after_identity: &ProcessIdentity,
    after: &LinuxProcessCounters,
) -> Result<LinuxProcessCounters, &'static str> {
    if before_identity != after_identity {
        return Err("OS process identity changed");
    }
    macro_rules! delta {
        ($field:ident) => {
            after
                .$field
                .checked_sub(before.$field)
                .ok_or("OS process I/O counter reset")?
        };
    }
    Ok(LinuxProcessCounters {
        rchar: delta!(rchar),
        wchar: delta!(wchar),
        syscr: delta!(syscr),
        syscw: delta!(syscw),
        read_bytes: delta!(read_bytes),
        write_bytes: delta!(write_bytes),
        cancelled_write_bytes: delta!(cancelled_write_bytes),
    })
}
fn checked_block_delta(
    before_identity: &DeviceIdentity,
    before: &BlockCounters,
    after_identity: &DeviceIdentity,
    after: &BlockCounters,
) -> Result<BlockCounters, &'static str> {
    if before_identity != after_identity {
        return Err("OS block device identity changed");
    }
    delta_block_counters(before, after)
}
fn delta_block_counters(
    before: &BlockCounters,
    after: &BlockCounters,
) -> Result<BlockCounters, &'static str> {
    macro_rules! delta {
        ($field:ident) => {
            after
                .$field
                .checked_sub(before.$field)
                .ok_or("OS block I/O counter reset")?
        };
    }
    Ok(BlockCounters {
        read_ops_completed: delta!(read_ops_completed),
        write_ops_completed: delta!(write_ops_completed),
        sectors_read: delta!(sectors_read),
        sectors_written: delta!(sectors_written),
        read_bytes: delta!(read_bytes),
        write_bytes: delta!(write_bytes),
    })
}
fn valid_device_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
fn collect_when_enabled<T>(enabled: bool, observer: impl FnOnce() -> T) -> Option<T> {
    enabled.then(observer)
}
fn collect_consistent_device(
    mut identity: impl FnMut() -> Result<DeviceIdentity, &'static str>,
    counters: impl FnOnce() -> Result<BlockCounters, &'static str>,
) -> Result<(DeviceIdentity, BlockCounters), &'static str> {
    let before = identity()?;
    let sample = counters()?;
    if before != identity()? {
        return Err("OS block device identity changed during observation");
    }
    Ok((before, sample))
}

#[cfg(target_os = "linux")]
fn read_bounded(path: impl AsRef<std::path::Path>, limit: usize) -> Result<String, &'static str> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "OS I/O observation read unavailable")?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "OS I/O observation read unavailable")?;
    if bytes.len() > limit {
        return Err("OS I/O observation exceeds bounded record size");
    }
    String::from_utf8(bytes).map_err(|_| "OS I/O observation encoding invalid")
}

#[cfg(target_os = "linux")]
fn collect_process_disk() -> Observation<ProcessRecord> {
    let sample = (|| {
        let pid = std::process::id();
        let identity = parse_linux_process_identity(&read_bounded("/proc/self/stat", 16384)?, pid)?;
        let counters = parse_linux_process_io(&read_bounded("/proc/self/io", 16384)?)?;
        let after = parse_linux_process_identity(&read_bounded("/proc/self/stat", 16384)?, pid)?;
        if identity != after {
            return Err("OS process identity changed during observation");
        }
        Ok(ProcessRecord {
            source: "linux_proc_self_io",
            identity,
            read_bytes: counters.read_bytes,
            write_bytes: counters.write_bytes,
            linux_counters: Some(counters),
        })
    })();
    observation(sample)
}

#[cfg(target_os = "macos")]
fn collect_process_disk() -> Observation<ProcessRecord> {
    let sample = (|| {
        let pid = std::process::id();
        let native_pid = i32::try_from(pid).map_err(|_| "OS observer PID out of range")?;
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
        // Installed libproc.h takes a contiguous V2 struct address through its
        // rusage_info_t* ABI. libc's repr(C) layout matches the installed SDK;
        // the API fills this whole flavor on return0. Failure never initializes.
        let status = unsafe {
            libc::proc_pid_rusage(native_pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast())
        };
        if status != 0 {
            return Err("proc_pid_rusage V2 unavailable");
        }
        let info = unsafe { info.assume_init() };
        if info.ri_proc_start_abstime == 0 {
            return Err("OS process start identity unavailable");
        }
        Ok(ProcessRecord {
            source: "darwin_proc_pid_rusage_v2",
            identity: ProcessIdentity {
                pid,
                start_token: info.ri_proc_start_abstime,
            },
            read_bytes: info.ri_diskio_bytesread,
            write_bytes: info.ri_diskio_byteswritten,
            linux_counters: None,
        })
    })();
    observation(sample)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn collect_process_disk() -> Observation<ProcessRecord> {
    Observation::Unsupported {
        reason: "OS process disk accounting not implemented on this platform",
    }
}

fn observation<T>(result: Result<T, &'static str>) -> Observation<T> {
    match result {
        Ok(sample) => Observation::Available { sample },
        Err(reason) => Observation::Unavailable { reason },
    }
}

#[cfg(target_os = "linux")]
fn collect_selected_device(_process: &Observation<ProcessRecord>) -> Observation<DeviceRecord> {
    let Some(name) = std::env::var_os("MOUNT_RS_PROFILE_BLOCK_DEVICE") else {
        return if std::env::var_os("MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID").is_some() {
            Observation::Unsupported {
                reason: "IORegistry driver selection requires macOS",
            }
        } else {
            Observation::Unselected
        };
    };
    let sample = (|| {
        let name = name
            .into_string()
            .map_err(|_| "selected Linux block device name invalid")?;
        if !valid_device_name(&name) {
            return Err("selected Linux block device name invalid");
        }
        let root = std::path::Path::new("/sys/block").join(&name);
        let (identity, counters) = collect_consistent_device(
            || read_linux_device_identity(&root, &name),
            || parse_linux_block_stat(&read_bounded(root.join("stat"), 4096)?),
        )?;
        Ok(DeviceRecord::LinuxBlockStat { identity, counters })
    })();
    observation(sample)
}

#[cfg(target_os = "linux")]
fn read_linux_device_identity(
    root: &std::path::Path,
    name: &str,
) -> Result<DeviceIdentity, &'static str> {
    let dev = read_bounded(root.join("dev"), 128)?;
    let (major, minor) = dev
        .trim()
        .split_once(':')
        .ok_or("OS block major/minor unavailable")?;
    let major = u32::try_from(decimal_u64(major)?).map_err(|_| "OS block major out of range")?;
    let minor = u32::try_from(decimal_u64(minor)?).map_err(|_| "OS block minor out of range")?;
    let disk_sequence = decimal_u64(read_bounded(root.join("diskseq"), 128)?.trim())?;
    let boot_id = read_bounded("/proc/sys/kernel/random/boot_id", 128)?
        .trim()
        .to_owned();
    if boot_id.len() != 36
        || !boot_id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err("OS boot identity unavailable");
    }
    Ok(DeviceIdentity {
        name: name.to_owned(),
        major,
        minor,
        disk_sequence,
        boot_id,
    })
}

#[cfg(target_os = "macos")]
fn collect_selected_device(process: &Observation<ProcessRecord>) -> Observation<DeviceRecord> {
    let Some(selected) = std::env::var_os("MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID") else {
        return if std::env::var_os("MOUNT_RS_PROFILE_BLOCK_DEVICE").is_some() {
            Observation::Unsupported {
                reason: "Linux block device selection requires Linux",
            }
        } else {
            Observation::Unselected
        };
    };
    let sample = (|| {
        let id = parse_registry_entry_id(
            &selected
                .into_string()
                .map_err(|_| "selected IORegistry ID invalid")?,
        )?;
        let Observation::Available { sample: process } = process else {
            return Err("observer process identity unavailable for selected macOS driver");
        };
        let counters = macos_driver::collect(id)?;
        Ok(DeviceRecord::DarwinIoKitStatistics {
            identity: MacDriverIdentity {
                registry_entry_id: id,
                observer_process: process.identity.clone(),
            },
            counters,
        })
    })();
    observation(sample)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn collect_selected_device(_process: &Observation<ProcessRecord>) -> Observation<DeviceRecord> {
    Observation::Unsupported {
        reason: "selected OS block device accounting not implemented on this platform",
    }
}

#[cfg(target_os = "macos")]
mod macos_driver {
    use super::{MacDriverCounters, MacDriverRead, MacNumberRead, decode_mac_driver};
    use std::{
        ffi::{CStr, c_void},
        ptr,
    };

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegistryEntryIDMatching(id: u64) -> *mut c_void;
        fn IOServiceGetMatchingService(port: libc::mach_port_t, matching: *const c_void) -> u32;
        fn IOObjectConformsTo(object: u32, class: *const libc::c_char) -> libc::boolean_t;
        fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> libc::kern_return_t;
        fn IORegistryEntryCreateCFProperty(
            entry: u32,
            key: *const c_void,
            allocator: *const c_void,
            options: u32,
        ) -> *const c_void;
        fn IOObjectRelease(object: u32) -> libc::kern_return_t;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const libc::c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
        fn CFGetTypeID(object: *const c_void) -> libc::c_ulong;
        fn CFDictionaryGetTypeID() -> libc::c_ulong;
        fn CFNumberGetTypeID() -> libc::c_ulong;
        fn CFDictionaryGetValueIfPresent(
            dictionary: *const c_void,
            key: *const c_void,
            value: *mut *const c_void,
        ) -> u8;
        fn CFNumberIsFloatType(number: *const c_void) -> u8;
        fn CFNumberGetValue(number: *const c_void, kind: libc::c_long, value: *mut c_void) -> u8;
    }

    // Own only nonnull Create-rule objects. Values borrowed from Statistics
    // remain unowned and alive until this dictionary guard drops.
    struct CfOwned(*const c_void);
    impl CfOwned {
        fn from_create(pointer: *const c_void) -> Result<Self, &'static str> {
            if pointer.is_null() {
                Err("selected macOS driver CF object unavailable")
            } else {
                Ok(Self(pointer))
            }
        }
        fn string(text: &CStr) -> Result<Self, &'static str> {
            // Fixed null-terminated keys, null default allocator, UTF8 encoding.
            Self::from_create(unsafe {
                CFStringCreateWithCString(ptr::null(), text.as_ptr(), 0x08000100)
            })
        }
        fn into_consumed(self) -> *const c_void {
            let pointer = self.0;
            std::mem::forget(self);
            pointer
        }
    }
    impl Drop for CfOwned {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }
    struct ServiceOwned(u32);
    impl Drop for ServiceOwned {
        fn drop(&mut self) {
            unsafe { IOObjectRelease(self.0) };
        }
    }
    fn actual_id(service: &ServiceOwned) -> Result<u64, &'static str> {
        let mut id = 0;
        if unsafe { IORegistryEntryGetRegistryEntryID(service.0, &mut id) } != 0 {
            return Err("selected macOS driver registry identity unavailable");
        }
        Ok(id)
    }
    fn number(dictionary: &CfOwned, key: &CStr) -> Result<MacNumberRead, &'static str> {
        let key = CfOwned::string(key)?;
        let mut value = ptr::null();
        if unsafe { CFDictionaryGetValueIfPresent(dictionary.0, key.0, &mut value) } == 0
            || value.is_null()
        {
            return Ok(MacNumberRead::Missing);
        }
        if unsafe { CFGetTypeID(value) != CFNumberGetTypeID() } {
            return Ok(MacNumberRead::NotNumber);
        }
        if unsafe { CFNumberIsFloatType(value) } != 0 {
            return Ok(MacNumberRead::NotInteger);
        }
        let mut signed_value = 0i64;
        // SInt64=4. Failed/lossy conversion may write a best-effort value; the
        // decoder rejects exact=false before using that value as a counter.
        let exact =
            unsafe { CFNumberGetValue(value, 4, (&mut signed_value as *mut i64).cast()) } != 0;
        Ok(MacNumberRead::Read {
            exact,
            signed_value,
        })
    }
    pub(super) fn collect(selected_id: u64) -> Result<MacDriverCounters, &'static str> {
        let matching = CfOwned::from_create(unsafe { IORegistryEntryIDMatching(selected_id) })?;
        // SDK explicitly defines0 as default main port. This call ALWAYS
        // consumes the matching dictionary reference, including lookup failure.
        let service = unsafe { IOServiceGetMatchingService(0, matching.into_consumed()) };
        if service == 0 {
            return Err("selected macOS driver not found or unavailable");
        }
        let service = ServiceOwned(service);
        let id = actual_id(&service)?;
        let is_driver =
            unsafe { IOObjectConformsTo(service.0, c"IOBlockStorageDriver".as_ptr()) } != 0;
        if id != selected_id || !is_driver {
            return Err("selected macOS driver identity or class mismatch");
        }
        let key = CfOwned::string(c"Statistics")?;
        let statistics = CfOwned::from_create(unsafe {
            IORegistryEntryCreateCFProperty(service.0, key.0, ptr::null(), 0)
        })?;
        if unsafe { CFGetTypeID(statistics.0) != CFDictionaryGetTypeID() } {
            return Err("selected macOS driver statistics dictionary unavailable");
        }
        let [read_ops, write_ops, read_bytes, write_bytes] = [
            c"Operations (Read)",
            c"Operations (Write)",
            c"Bytes (Read)",
            c"Bytes (Write)",
        ]
        .map(|key| number(&statistics, key));
        // Recheck identity around the copy/conversions; only this one retained
        // service is queried, with no parent/child lookup, inventory or summation.
        decode_mac_driver(
            selected_id,
            MacDriverRead {
                registry_entry_id: Some(actual_id(&service)?),
                is_block_storage_driver: is_driver,
                numbers: Some([read_ops?, write_ops?, read_bytes?, write_bytes?]),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROCESS_IO: &str = "rchar: 9\nwchar: 11\nsyscr: 3\nsyscw: 4\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 10240\n";
    const BLOCK_STAT: &str = "5 2 8 4 9 1 16 3 0 6 7\n";

    fn process_identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: 71,
            start_token: 900,
        }
    }
    fn device_identity() -> DeviceIdentity {
        DeviceIdentity {
            name: "nvme0n1".into(),
            major: 259,
            minor: 0,
            disk_sequence: 14,
            boot_id: "00112233-4455-6677-8899-aabbccddeeff".into(),
        }
    }

    #[test]
    fn linux_process_io_retains_cached_syscall_and_cancelled_bytes_separately() {
        let parsed = parse_linux_process_io(PROCESS_IO).unwrap();
        assert_eq!(
            parsed,
            LinuxProcessCounters {
                rchar: 9,
                wchar: 11,
                syscr: 3,
                syscw: 4,
                read_bytes: 4096,
                write_bytes: 8192,
                cancelled_write_bytes: 10240,
            }
        );
        assert!(parsed.cancelled_write_bytes > parsed.write_bytes);
    }

    #[test]
    fn linux_process_parser_rejects_missing_duplicate_malformed_or_overflowed_fields() {
        for invalid in [
            PROCESS_IO.replace("syscr: 3\n", ""),
            format!("{PROCESS_IO}syscr: 3\n"),
            PROCESS_IO.replace("syscr: 3", "syscr: -3"),
            PROCESS_IO.replace("syscr: 3", "syscr: +3"),
            PROCESS_IO.replace("syscr: 3", "syscr: 18446744073709551616"),
            PROCESS_IO.replace("syscr: 3", "syscr: 3 4"),
        ] {
            assert!(
                parse_linux_process_io(&invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn linux_process_start_identity_handles_spaces_and_parentheses_in_comm() {
        let text = "71 (worker (io)) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 900 20";
        assert_eq!(
            parse_linux_process_identity(text, 71).unwrap(),
            process_identity()
        );
        assert!(parse_linux_process_identity(text, 72).is_err());
        assert!(parse_linux_process_identity("71 (worker) R 1", 71).is_err());
    }

    #[test]
    fn selected_linux_block_stats_use_completed_ops_and512_byte_sectors() {
        let parsed = parse_linux_block_stat(BLOCK_STAT).unwrap();
        assert_eq!(
            parsed,
            BlockCounters {
                read_ops_completed: 5,
                write_ops_completed: 9,
                sectors_read: 8,
                sectors_written: 16,
                read_bytes: 4096,
                write_bytes: 8192,
            }
        );
        let raw = serde_json::to_value(DeviceRecord::LinuxBlockStat {
            identity: device_identity(),
            counters: parsed.clone(),
        })
        .unwrap();
        assert_eq!(raw["source"], "linux_block_stat");
        assert_eq!(raw["counters"]["sectors_read"], "8");
        assert_eq!(
            parse_linux_block_stat(&format!("{} 3 4 5 6 7 8", BLOCK_STAT.trim())).unwrap(),
            parsed
        );
        assert!(parse_linux_block_stat("5 2 8").is_err());
        assert!(parse_linux_block_stat("5 2 36028797018963968 4 9 1 16 3 0 6 7").is_err());
    }

    #[test]
    fn selected_device_names_cannot_be_paths_or_inventory_requests() {
        for valid in ["nvme0n1", "sda", "dm-0", "loop_0"] {
            assert!(valid_device_name(valid));
        }
        for invalid in [
            "", ".", "..", "../sda", "/dev/sda", "sda/sda1", "*", "sda\n",
        ] {
            assert!(!valid_device_name(invalid));
        }
    }

    #[test]
    fn process_deltas_reject_pid_reuse_and_counter_resets() {
        let before = parse_linux_process_io(PROCESS_IO).unwrap();
        let mut after = before.clone();
        after.read_bytes += 4096;
        let identity = process_identity();
        let delta = checked_process_delta(&identity, &before, &identity, &after).unwrap();
        assert_eq!(delta.read_bytes, 4096);
        assert_eq!(delta.write_bytes, 0);
        let reused = ProcessIdentity {
            start_token: 901,
            ..identity.clone()
        };
        assert!(checked_process_delta(&identity, &before, &reused, &after).is_err());
        assert!(checked_process_delta(&identity, &after, &identity, &before).is_err());
    }

    #[test]
    fn device_deltas_reject_reattachment_reboot_change_or_counter_reset() {
        let before = parse_linux_block_stat(BLOCK_STAT).unwrap();
        let mut after = before.clone();
        after.read_ops_completed += 3;
        let identity = device_identity();
        assert_eq!(
            checked_block_delta(&identity, &before, &identity, &after)
                .unwrap()
                .read_ops_completed,
            3
        );
        for changed in [
            DeviceIdentity {
                disk_sequence: 15,
                ..identity.clone()
            },
            DeviceIdentity {
                major: 260,
                ..identity.clone()
            },
            DeviceIdentity {
                boot_id: "another boot".into(),
                ..identity.clone()
            },
            DeviceIdentity {
                name: "sda".into(),
                ..identity.clone()
            },
        ] {
            assert!(checked_block_delta(&identity, &before, &changed, &after).is_err());
        }
        assert!(checked_block_delta(&identity, &after, &identity, &before).is_err());
    }

    #[test]
    fn disabled_observation_does_not_call_the_observer() {
        let result = collect_when_enabled(false, || {
            panic!("disabled observer performed an OS read or clock")
        });
        assert_eq!(result, None::<()>);
        assert_eq!(collect_when_enabled(true, || 9), Some(9));
    }

    #[test]
    fn device_collection_brackets_the_counter_read_with_matching_identity() {
        let identity = device_identity();
        let counters = BlockCounters {
            read_ops_completed: 5,
            write_ops_completed: 9,
            sectors_read: 8,
            sectors_written: 16,
            read_bytes: 4096,
            write_bytes: 8192,
        };
        let mut observations = [identity.clone(), identity.clone()].into_iter();
        assert_eq!(collect_consistent_device(
            || Ok(observations.next().unwrap()), || Ok(counters.clone()),
        ).unwrap(), (identity.clone(), counters.clone()));
        let mut observations = [
            identity.clone(),
            DeviceIdentity {
                disk_sequence: 15,
                ..identity
            },
        ]
        .into_iter();
        assert!(
            collect_consistent_device(|| Ok(observations.next().unwrap()), || Ok(counters),)
                .is_err()
        );
    }

    #[test]
    fn wire_counters_preserve_exact_u64_decimal_strings() {
        let counters = LinuxProcessCounters {
            rchar: u64::MAX,
            wchar: 0,
            syscr: 1,
            syscw: 2,
            read_bytes: 9007199254740993,
            write_bytes: 8192,
            cancelled_write_bytes: 3,
        };
        let value = serde_json::to_value(counters).unwrap();
        assert_eq!(value["rchar"], "18446744073709551615");
        assert_eq!(value["read_bytes"], "9007199254740993");
        assert_eq!(value["wchar"], "0");
    }

    #[test]
    fn macos_registry_selection_requires_one_canonical_nonzero_u64_id() {
        assert_eq!(
            parse_registry_entry_id("18446744073709551615").unwrap(),
            u64::MAX
        );
        assert_eq!(parse_registry_entry_id("4294967312").unwrap(), 4294967312);
        for invalid in [
            "0",
            "01",
            "0x10",
            "+1",
            "-1",
            " 1",
            "1\n",
            "*",
            "18446744073709551616",
        ] {
            assert!(
                parse_registry_entry_id(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    fn mac_read(id: Option<u64>, numbers: Option<[MacNumberRead; 4]>) -> MacDriverRead {
        MacDriverRead {
            registry_entry_id: id,
            is_block_storage_driver: true,
            numbers,
        }
    }
    fn mac_numbers() -> [MacNumberRead; 4] {
        [5, 9, 4096, 8192].map(|signed_value| MacNumberRead::Read {
            exact: true,
            signed_value,
        })
    }

    #[test]
    fn macos_driver_decode_requires_matching_id_class_and_all_four_typed_exact_counters() {
        let expected = MacDriverCounters {
            read_operations_processed: 5,
            write_operations_processed: 9,
            read_bytes: 4096,
            write_bytes: 8192,
        };
        assert_eq!(
            decode_mac_driver(71, mac_read(Some(71), Some(mac_numbers()))).unwrap(),
            expected
        );
        assert!(decode_mac_driver(71, mac_read(None, Some(mac_numbers()))).is_err());
        assert!(decode_mac_driver(71, mac_read(Some(72), Some(mac_numbers()))).is_err());
        assert!(decode_mac_driver(71, mac_read(Some(71), None)).is_err());
        let mut wrong_class = mac_read(Some(71), Some(mac_numbers()));
        wrong_class.is_block_storage_driver = false;
        assert!(decode_mac_driver(71, wrong_class).is_err());
        for invalid in [
            MacNumberRead::Missing,
            MacNumberRead::NotNumber,
            MacNumberRead::NotInteger,
            MacNumberRead::Read {
                exact: false,
                signed_value: 4096,
            },
            MacNumberRead::Read {
                exact: true,
                signed_value: -1,
            },
        ] {
            for index in 0..4 {
                let mut numbers = mac_numbers();
                numbers[index] = invalid;
                assert!(decode_mac_driver(71, mac_read(Some(71), Some(numbers))).is_err());
            }
        }
    }

    #[test]
    fn macos_driver_delta_rejects_driver_change_process_generation_change_and_reset() {
        let identity = MacDriverIdentity {
            registry_entry_id: 71,
            observer_process: process_identity(),
        };
        let before = MacDriverCounters {
            read_operations_processed: 5,
            write_operations_processed: 9,
            read_bytes: 4096,
            write_bytes: 8192,
        };
        let mut after = before.clone();
        after.read_operations_processed += 3;
        assert_eq!(
            checked_mac_driver_delta(&identity, &before, &identity, &after)
                .unwrap()
                .read_operations_processed,
            3
        );
        let changed_driver = MacDriverIdentity {
            registry_entry_id: 72,
            ..identity.clone()
        };
        assert!(checked_mac_driver_delta(&identity, &before, &changed_driver, &after).is_err());
        let changed_observer = MacDriverIdentity {
            observer_process: ProcessIdentity {
                start_token: 901,
                ..process_identity()
            },
            ..identity.clone()
        };
        assert!(checked_mac_driver_delta(&identity, &before, &changed_observer, &after).is_err());
        assert!(checked_mac_driver_delta(&identity, &after, &identity, &before).is_err());
    }

    fn process_snapshot(counters: LinuxProcessCounters) -> Snapshot {
        Snapshot {
            enabled: true,
            process_disk: Observation::Available {
                sample: ProcessRecord {
                    source: "linux_proc_self_io",
                    identity: process_identity(),
                    read_bytes: counters.read_bytes,
                    write_bytes: counters.write_bytes,
                    linux_counters: Some(counters),
                },
            },
            host_block_device: Observation::Unselected,
            observer_elapsed_ns: Some(3),
            observed_at: None,
        }
    }

    #[test]
    fn disabled_snapshot_preserves_unavailability_without_zero_counters_or_interval() {
        let disabled = Snapshot::disabled();
        let value = disabled.delta_for_interval(&disabled, None);
        assert_eq!(value["process_disk"]["status"], "disabled");
        assert_eq!(value["process_disk"]["complete"], false);
        assert_eq!(value["host_block_device"]["status"], "disabled");
        assert!(value["interval_ns"].is_null());
        assert!(value["process_disk"]["counters"].is_null());
        assert!(disabled.observed_at.is_none());
        assert!(disabled.observer_elapsed_ns.is_none());
    }

    #[test]
    fn phase_envelope_retains_raw_snapshots_exact_interval_and_checked_delta() {
        let before = process_snapshot(parse_linux_process_io(PROCESS_IO).unwrap());
        let mut after_counters = parse_linux_process_io(PROCESS_IO).unwrap();
        after_counters.read_bytes += 4096;
        let after = process_snapshot(after_counters);
        let value = after.delta_for_interval(&before, Some(1000));
        assert_eq!(value["interval_ns"], "1000");
        assert_eq!(value["process_disk"]["status"], "available");
        assert_eq!(value["process_disk"]["complete"], true);
        assert_eq!(value["process_disk"]["counters"]["read_bytes"], "4096");
        assert_eq!(
            value["process_disk"]["before"]["sample"]["read_bytes"],
            "4096"
        );
        assert_eq!(
            value["process_disk"]["after"]["sample"]["read_bytes"],
            "8192"
        );
        assert_eq!(value["host_block_device"]["status"], "unselected");
        assert_eq!(value["host_block_device"]["complete"], false);
        assert_eq!(value["observer_elapsed_start_ns"], "3");
        assert_eq!(value["observer_elapsed_end_ns"], "3");
    }

    #[test]
    fn invalid_interval_or_reset_retains_raw_evidence_and_marks_incomplete() {
        let snapshot = process_snapshot(parse_linux_process_io(PROCESS_IO).unwrap());
        for interval in [None, Some(0)] {
            let value = snapshot.delta_for_interval(&snapshot, interval);
            assert_eq!(value["process_disk"]["status"], "unavailable");
            assert_eq!(value["process_disk"]["complete"], false);
            assert_eq!(
                value["process_disk"]["reason"],
                "OS I/O interval unavailable"
            );
            assert_eq!(
                value["process_disk"]["before"]["sample"]["read_bytes"],
                "4096"
            );
        }
        let mut reset_counters = parse_linux_process_io(PROCESS_IO).unwrap();
        reset_counters.syscw = 1;
        let reset = process_snapshot(reset_counters);
        let value = reset.delta_for_interval(&snapshot, Some(1000));
        assert_eq!(value["process_disk"]["status"], "unavailable");
        assert_eq!(
            value["process_disk"]["reason"],
            "OS process I/O counter reset"
        );
        assert!(value["process_disk"]["counters"].is_null());
        assert_eq!(
            value["process_disk"]["after"]["sample"]["linux_counters"]["syscw"],
            "1"
        );
    }
}

#[cfg(test)]
#[test]
#[ignore = "explicit read-only own-process OS boundary; device selection must be unset"]
fn own_process_os_io_boundary_retains_native_identity_and_observer_cost() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert!(std::env::var_os("MOUNT_RS_PROFILE_BLOCK_DEVICE").is_none());
    assert!(std::env::var_os("MOUNT_RS_PROFILE_IOREGISTRY_ENTRY_ID").is_none());
    let before = super::Snapshot::capture_process_io_boundary().unwrap();
    let after = super::Snapshot::capture_connections_io_boundary(&[]).unwrap();
    let delta = after.delta(&before).unwrap();
    let io = &delta["os_io"];
    assert_eq!(io["process_disk"]["status"], "available");
    assert_eq!(io["process_disk"]["complete"], true);
    assert_eq!(
        io["process_disk"]["before"]["sample"]["identity"]["pid"],
        std::process::id()
    );
    assert_eq!(io["host_block_device"]["status"], "unselected");
    assert!(io["interval_ns"].as_str().unwrap().parse::<u64>().unwrap() > 0);
    assert!(io["observer_elapsed_start_ns"].as_str().is_some());
    assert!(io["observer_elapsed_end_ns"].as_str().is_some());
    let ordinary = super::Snapshot::capture_process().unwrap();
    assert_eq!(
        ordinary.os_io.delta(&ordinary.os_io)["process_disk"]["status"],
        "disabled"
    );
    println!(
        "OS_IO_NATIVE_JSON={}",
        serde_json::json!({"raw_before":before.os_io,"raw_after":after.os_io,"phase":io})
    );
}
