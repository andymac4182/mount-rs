use std::sync::{Arc, Mutex};

use mount_rs_memfs::{MemoryFs, MemoryOptions};
use napi::bindgen_prelude::{Env, Function, JsObjectValue, Object};
use napi_derive::napi;

use super::Filesystem;

#[napi(object)]
pub struct JsMemoryOptions {
    pub uid: Option<f64>,
    pub gid: Option<f64>,
    pub umask: Option<f64>,
    pub root_mode: Option<f64>,
}

pub(crate) fn process_id(env: &Env, property: &str) -> napi::Result<u32> {
    let global = env.get_global()?;
    let process: Object = global.get_named_property("process")?;
    if !process.has_named_property(property)? {
        return Ok(0);
    }

    let getter: Function<(), u32> = process.get_named_property(property)?;
    getter.apply(process, ())
}

#[cfg(unix)]
unsafe extern "C" {
    fn getgid() -> std::os::raw::c_uint;
    fn getuid() -> std::os::raw::c_uint;
}

pub(crate) fn native_process_identity() -> (u32, u32) {
    #[cfg(unix)]
    {
        // SAFETY: POSIX getuid/getgid have no pointer arguments or retained state.
        unsafe { (getuid(), getgid()) }
    }
    #[cfg(not(unix))]
    {
        (0, 0)
    }
}

/// Construct the N-API memory driver with the same identity and mode defaults
/// as mountx's TypeScript memory driver. `Env` is used only to read Node's
/// optional process identity helpers; the filesystem itself is entirely Rust.
#[napi]
pub fn create_memory_driver(
    env: Env,
    options: Option<JsMemoryOptions>,
) -> napi::Result<Filesystem> {
    let options = options.unwrap_or(JsMemoryOptions {
        uid: None,
        gid: None,
        umask: None,
        root_mode: None,
    });
    let uid = match options.uid {
        Some(value) => super::validate_u32("uid", value)?,
        None => process_id(&env, "getuid")?,
    };
    let gid = match options.gid {
        Some(value) => super::validate_u32("gid", value)?,
        None => process_id(&env, "getgid")?,
    };
    let umask = super::optional_u32("umask", options.umask, 0)?;
    let root_mode = super::optional_u32("rootMode", options.root_mode, 0o755)?;

    Ok(Filesystem {
        driver: super::instrument_driver(Arc::new(MemoryFs::new(MemoryOptions {
            uid,
            gid,
            umask,
            root_mode,
        }))),
        shutdown: None,
        reconcile: Mutex::new(None),
    })
}
