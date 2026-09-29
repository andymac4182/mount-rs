// Copyright (c) 2026 mount-rs contributors.
//
// Licensed under the Apache License, Version 2.0, or the MIT license, at your option.

//! Isolated decoder control. This window excludes packet framing, I/O, TLS,
//! query setup, and cursor/provider work; it proves only owned Row/Value removal.

use super::inspect_binary_row;
use crate::{buffer_pool::BufferPool, queryable::Protocol, BinaryProtocol, Column, Row, Value};
use mysql_common::constants::ColumnType;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counts {
    pub(crate) calls: usize,
    pub(crate) bytes: usize,
}

thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { calls: 0, bytes: 0 }) };
}

fn record(bytes: usize) {
    let _ = BUSY.try_with(|busy| {
        if busy.replace(true) {
            return;
        }
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = COUNTS.try_with(|counts| {
                    let previous = counts.get();
                    counts.set(Counts {
                        calls: previous.calls.saturating_add(1),
                        bytes: previous.bytes.saturating_add(bytes),
                    });
                });
            }
        });
        busy.set(false);
    });
}

// This test-only allocator forwards each allocation unchanged to System. Its
// thread-local counter follows the repository's existing allocation controls.
struct Meter;
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Meter = Meter;

pub(crate) struct Window;
impl Window {
    pub(crate) fn begin() -> Self {
        assert!(!ACTIVE.with(Cell::get));
        COUNTS.with(|counts| counts.set(Counts::default()));
        ACTIVE.with(|active| active.set(true));
        Self
    }
    pub(crate) fn finish(self) -> Counts {
        drop(self);
        COUNTS.with(Cell::get)
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Summary {
    signed: i64,
    name_len: usize,
    name_hash: u64,
    raw_len: usize,
    raw_hash: u64,
    columns: usize,
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |value, byte| {
        (value ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn bytes(value: &Value) -> &[u8] {
    match value {
        Value::Bytes(bytes) => bytes,
        other => panic!("expected bytes, received {other:?}"),
    }
}

fn owned_summary(row: &Row) -> Summary {
    let signed = match row.as_ref(0).unwrap() {
        Value::Int(value) => *value,
        other => panic!("expected signed i64, received {other:?}"),
    };
    let name = bytes(row.as_ref(1).unwrap());
    let raw = bytes(row.as_ref(2).unwrap());
    Summary {
        signed,
        name_len: name.len(),
        name_hash: hash(name),
        raw_len: raw.len(),
        raw_hash: hash(raw),
        columns: row.columns_ref().len(),
    }
}

fn lenenc(packet: &[u8], offset: &mut usize) -> usize {
    let first = packet[*offset];
    *offset += 1;
    match first {
        0..=250 => usize::from(first),
        0xfc => {
            let len = u16::from_le_bytes(packet[*offset..*offset + 2].try_into().unwrap());
            *offset += 2;
            usize::from(len)
        }
        other => panic!("fixture length marker {other:x} is unsupported"),
    }
}

fn raw_summary(packet: &[u8], columns: &[Column]) -> Summary {
    // These are deliberately fixed, valid fixture bytes. Domain validation is a
    // provider responsibility and has its own malformed-packet test suite.
    assert_eq!(&packet[..2], &[0, 0]);
    let signed = i64::from_le_bytes(packet[2..10].try_into().unwrap());
    let mut offset = 10;
    let name_len = lenenc(packet, &mut offset);
    let name = &packet[offset..offset + name_len];
    offset += name_len;
    let raw_len = lenenc(packet, &mut offset);
    let raw = &packet[offset..offset + raw_len];
    offset += raw_len;
    assert_eq!(offset, packet.len());
    Summary {
        signed,
        name_len,
        name_hash: hash(name),
        raw_len,
        raw_hash: hash(raw),
        columns: columns.len(),
    }
}

fn write_lenenc(packet: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() <= 250 {
        packet.push(bytes.len() as u8);
    } else {
        packet.push(0xfc);
        packet.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    }
    packet.extend_from_slice(bytes);
}

#[test]
fn borrowed_binary_row_decode_removes_owned_value_allocations() {
    const ROWS: usize = 16;
    let columns: Arc<[Column]> = vec![
        Column::new(ColumnType::MYSQL_TYPE_LONGLONG),
        Column::new(ColumnType::MYSQL_TYPE_VAR_STRING),
        Column::new(ColumnType::MYSQL_TYPE_BLOB),
    ]
    .into();
    let pool = Arc::new(BufferPool::new());

    for (signed, name_len) in [(i64::MIN, 0), (-1, 9), (0, 300), (i64::MAX, 8192)] {
        let name = vec![b'n'; name_len];
        let raw = [0xff, 0, 0xfe, b'x'];
        let mut payload = vec![0, 0]; // Binary header and one-byte NULL bitmap.
        payload.extend_from_slice(&signed.to_le_bytes());
        write_lenenc(&mut payload, &name);
        write_lenenc(&mut payload, &raw);
        let expected = Summary {
            signed,
            name_len: name.len(),
            name_hash: hash(&name),
            raw_len: raw.len(),
            raw_hash: hash(&raw),
            columns: columns.len(),
        };

        // Warm the packet pool and both decoder paths before either meter starts.
        drop(pool.get_with(&payload));
        let packet = pool.get_with(&payload);
        let owned = BinaryProtocol::read_result_set_row(&packet, columns.clone()).unwrap();
        assert_eq!(owned_summary(&owned), expected);
        assert_eq!(
            inspect_binary_row(&packet, columns.clone(), raw_summary).unwrap(),
            expected
        );
        drop(owned);
        let mut owned_observations = [Summary::default(); ROWS];
        let mut raw_observations = [Summary::default(); ROWS];

        let window = Window::begin();
        for observation in &mut owned_observations {
            let row = BinaryProtocol::read_result_set_row(&packet, columns.clone()).unwrap();
            *observation = owned_summary(std::hint::black_box(&row));
        }
        let owned_counts = window.finish();

        let window = Window::begin();
        for observation in &mut raw_observations {
            *observation = inspect_binary_row(&packet, columns.clone(), raw_summary).unwrap();
        }
        let raw_counts = window.finish();

        assert!(owned_observations
            .iter()
            .all(|observation| *observation == expected));
        assert!(raw_observations
            .iter()
            .all(|observation| *observation == expected));
        assert!(
            owned_counts.calls >= ROWS,
            "owned allocating control: {owned_counts:?}"
        );
        assert_eq!(
            raw_counts.calls, 0,
            "borrowed callback retained owned decoding: {raw_counts:?}"
        );
        assert_eq!(raw_counts.bytes, 0);
        eprintln!("name_len={name_len} owned={owned_counts:?} raw={raw_counts:?}");
    }
}
