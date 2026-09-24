//! Codec-only diagnostic. Excludes QUIC, filesystem/storage, and input preparation.
use mount_rs_remote_protocol::{Message, Operation, OperationName};
use serde_json::{Value, json};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};
struct Counter;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counter = Counter;

fn measure(name: &str, size: usize, iterations: u64, mut work: impl FnMut()) {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        work();
    }
    let elapsed = start.elapsed();
    ENABLED.store(false, Ordering::Relaxed);
    println!(
        "WIRE_PROFILE name={name} payload_bytes={size} iterations={iterations} ns_per_op={:.0} allocs_per_op={:.2} requested_bytes_per_op={:.0}",
        elapsed.as_nanos() as f64 / iterations as f64,
        ALLOCS.load(Ordering::Relaxed) as f64 / iterations as f64,
        BYTES.load(Ordering::Relaxed) as f64 / iterations as f64
    );
}
fn response(payload: &[u8]) -> Message {
    Message::Response {
        request_id: 1,
        result: Ok(serde_json::to_value(payload).unwrap()),
    }
}
fn write_request(payload: &[u8]) -> Message {
    Message::Request {
        request_id: 1,
        drive_id: "sandbox-drive".into(),
        operation: Operation {
            name: OperationName::HandleWrite,
            body: json!({"handle":1,"position":0,"data":payload}),
        },
    }
}
fn main() {
    println!(
        "WIRE_PROFILE serde_value_bytes={}",
        std::mem::size_of::<Value>()
    );
    for (size, iterations) in [(4096, 1000), (65536, 100), (1048576, 10)] {
        let payload: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let read = serde_json::to_vec(&response(&payload)).unwrap();
        let write = serde_json::to_vec(&write_request(&payload)).unwrap();
        let Message::Response {
            result: Ok(value), ..
        } = serde_json::from_slice(&read).unwrap()
        else {
            panic!("response");
        };
        assert_eq!(serde_json::from_value::<Vec<u8>>(value).unwrap(), payload);
        for (pattern, bytes) in [("zeros", vec![0; size]), ("all_255", vec![255; size])] {
            println!(
                "WIRE_SIZE pattern={pattern} payload_bytes={size} read_frame_bytes={}",
                serde_json::to_vec(&response(&bytes)).unwrap().len() + 4
            );
        }
        println!(
            "WIRE_SIZE pattern=uniform_bytes payload_bytes={size} read_frame_bytes={} write_frame_bytes={} value_array_storage_bytes={}",
            read.len() + 4,
            write.len() + 4,
            size * std::mem::size_of::<Value>()
        );
        measure("read_encode", size, iterations, || {
            black_box(serde_json::to_vec(&response(black_box(&payload))).unwrap());
        });
        measure("read_decode", size, iterations, || {
            let Message::Response {
                result: Ok(value), ..
            } = serde_json::from_slice(black_box(&read)).unwrap()
            else {
                panic!("response");
            };
            black_box(serde_json::from_value::<Vec<u8>>(value).unwrap());
        });
        measure("write_encode", size, iterations, || {
            black_box(serde_json::to_vec(&write_request(black_box(&payload))).unwrap());
        });
        measure("write_decode", size, iterations, || {
            let Message::Request { operation, .. } =
                serde_json::from_slice(black_box(&write)).unwrap()
            else {
                panic!("request");
            };
            let values = operation.body["data"].as_array().unwrap();
            let bytes: Vec<u8> = values
                .iter()
                .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
                .collect();
            black_box(bytes);
        });
    }
}
