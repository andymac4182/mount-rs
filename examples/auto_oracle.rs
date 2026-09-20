//! Deterministic, side-effect-free output for the `mount-rs/auto` parity check.
//!
//! The example asks the Rust facade about the same platform overrides used by
//! the pinned TypeScript oracle. It deliberately does not mount anything: the
//! native mount path is covered by the ignored platform harness instead.

use mount_rs_auto::{
    AutoProbe, P9ClientProbe, P9Platform, Transport, live_mounts, loaded_transports,
    p9_module_refusal, probe_transports_for,
};
use serde_json::{Value, json};

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Fuse => "fuse",
        Transport::P9 => "9p",
        Transport::Nfs => "nfs",
    }
}

fn probe_json(probe: &AutoProbe) -> Value {
    let one = |probe: &mount_rs_auto::TransportProbe| {
        json!({
            "usable": probe.usable,
            "reason": probe.reason,
        })
    };
    json!({
        "platform": probe.platform,
        "preference": probe.preference.iter().copied().map(transport_name).collect::<Vec<_>>(),
        "chosen": probe.chosen.map(transport_name),
        "fuse": one(&probe.fuse),
        "9p": one(&probe.p9),
        "nfs": one(&probe.nfs),
        "reason": probe.reason,
    })
}

fn p9_probe(transport: bool, modules: bool) -> P9ClientProbe {
    P9ClientProbe {
        usable: true,
        platform: Some(P9Platform::Linux),
        kernel: true,
        transport,
        modules,
        root: true,
        reason: None,
    }
}

fn main() {
    let platforms = ["linux", "darwin", "win32"]
        .into_iter()
        .map(|platform| probe_json(&probe_transports_for(platform)))
        .collect::<Vec<_>>();
    let p9_refusal = json!({
        "loaded": p9_module_refusal(&p9_probe(true, true)),
        "loadable": p9_module_refusal(&p9_probe(false, true)),
        "refused": p9_module_refusal(&p9_probe(false, false)),
    });
    let output = json!({
        "platforms": platforms,
        "p9_refusal": p9_refusal,
        "registry": {
            "loaded": loaded_transports().into_iter().map(transport_name).collect::<Vec<_>>(),
            "live": live_mounts().len(),
        },
    });
    println!("{output}");
}
