// Copyright (c) 2026 mount-rs contributors.
//
// Licensed under the Apache License, Version 2.0, or the MIT license, at your option.

//! Deterministic wire controls for the private borrowed binary-row extension.
//! Every test uses Conn::new and the ordinary packet reader; no database is required.

use std::{cell::Cell, panic::AssertUnwindSafe, rc::Rc, time::Duration};

use futures_util::{FutureExt, TryStreamExt};
use mysql_common::constants::{CapabilityFlags, ColumnFlags, ColumnType, StatusFlags};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

use super::QueryResult;
use crate::{
    conn::PendingResult, from_row, prelude::Queryable, BinaryProtocol, Column, Conn, Error,
    OptsBuilder, Row, ServerError, TxOpts, Value,
};

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(5);
const STMT_ID: u32 = 1;

#[derive(Clone, Copy)]
struct ColumnSpec {
    name: &'static [u8],
    kind: ColumnType,
}

const MEMBER: [ColumnSpec; 1] = [ColumnSpec {
    name: b"id",
    kind: ColumnType::MYSQL_TYPE_LONGLONG,
}];

const PAIR: [ColumnSpec; 2] = [
    MEMBER[0],
    ColumnSpec {
        name: b"name",
        kind: ColumnType::MYSQL_TYPE_VAR_STRING,
    },
];

const DENTRY: [ColumnSpec; 5] = [
    MEMBER[0],
    ColumnSpec {
        name: b"parent",
        kind: ColumnType::MYSQL_TYPE_LONGLONG,
    },
    PAIR[1],
    ColumnSpec {
        name: b"hash",
        kind: ColumnType::MYSQL_TYPE_BLOB,
    },
    ColumnSpec {
        name: b"revision",
        kind: ColumnType::MYSQL_TYPE_LONGLONG,
    },
];

enum Reply {
    Packets(Vec<Vec<u8>>),
    /// Write a prefix of one framed packet, then wait for explicit cancellation/release.
    Paused {
        before: Vec<Vec<u8>>,
        packet: Vec<u8>,
        split: usize,
        ready: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
        after: Vec<Vec<u8>>,
        close_on_release: bool,
    },
    /// Deliver metadata, then terminate in the middle of a row packet.
    Truncated(Vec<Vec<u8>>),
    /// A command has reached the server, but its routine cannot finish yet.
    Blocked {
        ready: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
}

struct Exchange {
    command: Vec<u8>,
    reply: Reply,
}

impl Exchange {
    fn query(sql: &str, packets: Vec<Vec<u8>>) -> Self {
        Self {
            command: command(0x03, sql.as_bytes()),
            reply: Reply::Packets(packets),
        }
    }

    fn prepare(sql: &str, columns: &[ColumnSpec]) -> Self {
        Self::prepare_with_id(sql, columns, STMT_ID)
    }

    fn prepare_with_id(sql: &str, columns: &[ColumnSpec], statement_id: u32) -> Self {
        let mut prepare_ok = vec![0x00];
        prepare_ok.extend_from_slice(&statement_id.to_le_bytes());
        prepare_ok.extend_from_slice(&(columns.len() as u16).to_le_bytes());
        prepare_ok.extend_from_slice(&[0, 0, 0, 0, 0]); // params, filler, warnings
        let mut packets = vec![prepare_ok];
        if !columns.is_empty() {
            packets.extend(columns.iter().map(column_packet));
            packets.push(eof(false));
        }
        Self {
            command: command(0x16, sql.as_bytes()),
            reply: Reply::Packets(packets),
        }
    }

    fn execute(packets: Vec<Vec<u8>>) -> Self {
        Self::execute_with_id(packets, STMT_ID)
    }

    fn execute_with_id(packets: Vec<Vec<u8>>, statement_id: u32) -> Self {
        Self {
            command: execute_command_for(statement_id),
            reply: Reply::Packets(packets),
        }
    }

    fn quit() -> Self {
        Self {
            command: vec![0x01],
            reply: Reply::Packets(vec![]),
        }
    }
}

struct ScriptedServer(JoinHandle<()>);

impl ScriptedServer {
    async fn connect(script: Vec<Exchange>) -> (Conn, Self) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            timeout(SCRIPT_TIMEOUT, async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                write_packet(&mut stream, 0, &handshake()).await;
                let (sequence, response) = read_packet(&mut stream).await;
                assert_eq!(sequence, 1, "handshake response sequence");
                assert!(
                    response.len() >= 32,
                    "complete protocol-41 handshake response"
                );
                let negotiated = CapabilityFlags::from_bits_truncate(u32::from_le_bytes(
                    response[..4].try_into().unwrap(),
                ));
                assert!(negotiated.contains(CapabilityFlags::CLIENT_PROTOCOL_41));
                assert!(!negotiated.intersects(
                    CapabilityFlags::CLIENT_DEPRECATE_EOF
                        | CapabilityFlags::CLIENT_SSL
                        | CapabilityFlags::CLIENT_COMPRESS
                ));
                write_packet(&mut stream, 2, &ok(false)).await;

                for (index, exchange) in script.into_iter().enumerate() {
                    let (sequence, received) = read_packet(&mut stream).await;
                    assert_eq!(sequence, 0, "command {index} sequence");
                    assert_eq!(received, exchange.command, "command {index}");
                    match exchange.reply {
                        Reply::Packets(packets) => {
                            write_packets(&mut stream, 1, &packets).await;
                        }
                        Reply::Paused {
                            before,
                            packet,
                            split,
                            ready,
                            release,
                            after,
                            close_on_release,
                        } => {
                            let sequence = write_packets(&mut stream, 1, &before).await;
                            let frame = framed(sequence, &packet);
                            assert!(split > 0 && split < frame.len());
                            stream.write_all(&frame[..split]).await.unwrap();
                            ready.send(()).unwrap();
                            release.await.unwrap();
                            if close_on_release {
                                stream.shutdown().await.unwrap();
                            } else {
                                stream.write_all(&frame[split..]).await.unwrap();
                                write_packets(&mut stream, sequence.wrapping_add(1), &after).await;
                            }
                        }
                        Reply::Truncated(before) => {
                            let sequence = write_packets(&mut stream, 1, &before).await;
                            // A complete framing header advertises ten bytes; only one follows.
                            stream.write_all(&[10, 0, 0, sequence, 0]).await.unwrap();
                            stream.shutdown().await.unwrap();
                        }
                        Reply::Blocked { ready, release } => {
                            ready.send(()).unwrap();
                            release.await.unwrap();
                            stream.shutdown().await.unwrap();
                        }
                    }
                }

                // This also rejects an extra fallback/query/QUIT outside the declared script.
                let mut extra = [0; 1];
                assert_eq!(stream.read(&mut extra).await.unwrap(), 0, "end of script");
            })
            .await
            .expect("scripted server timed out");
        });
        let opts = OptsBuilder::default()
            .ip_or_hostname(address.ip().to_string())
            .tcp_port(address.port())
            .prefer_socket(false)
            .max_allowed_packet(Some(16 * 1024 * 1024))
            .wait_timeout(Some(3600));
        let conn = timeout(SCRIPT_TIMEOUT, Conn::new(opts))
            .await
            .expect("scripted connection timed out")
            .unwrap();
        (conn, Self(task))
    }

    async fn finish(self, conn: Conn) {
        conn.disconnect().await.unwrap();
        timeout(SCRIPT_TIMEOUT, self.0)
            .await
            .expect("end-of-script assertion timed out")
            .unwrap();
    }
}

fn command(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut command = vec![kind];
    command.extend_from_slice(body);
    command
}

fn execute_command() -> Vec<u8> {
    execute_command_for(STMT_ID)
}

fn execute_command_for(statement_id: u32) -> Vec<u8> {
    let mut command = vec![0x17];
    command.extend_from_slice(&statement_id.to_le_bytes());
    command.extend_from_slice(&[0, 1, 0, 0, 0]); // no cursor, one iteration, no parameters
    command
}

fn handshake() -> Vec<u8> {
    // 8.0.36, native-password, legacy EOF; no TLS/compression/optional metadata.
    b"\x0a8.0.36\x00\x01\x00\x00\x00abcdefgh\x00\x01\xa2\x21\x02\x00\x0f\x00\x15\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00ijklmnopqrst\x00mysql_native_password\x00"
        .to_vec()
}

fn ok(more: bool) -> Vec<u8> {
    let status = status(more).to_le_bytes();
    vec![0, 0, 0, status[0], status[1], 0, 0]
}

fn eof(more: bool) -> Vec<u8> {
    let status = status(more).to_le_bytes();
    vec![0xfe, 0, 0, status[0], status[1]]
}

fn status(more: bool) -> u16 {
    (StatusFlags::SERVER_STATUS_AUTOCOMMIT
        | if more {
            StatusFlags::SERVER_MORE_RESULTS_EXISTS
        } else {
            StatusFlags::empty()
        })
    .bits()
}

fn server_error() -> Vec<u8> {
    let mut packet = vec![0xff];
    packet.extend_from_slice(&1064_u16.to_le_bytes());
    packet.extend_from_slice(b"#42000scripted row failure");
    packet
}

fn column_packet(column: &ColumnSpec) -> Vec<u8> {
    let mut packet = Vec::new();
    for field in [b"def".as_slice(), b"", b"", b"", column.name, b""] {
        push_bytes(&mut packet, field);
    }
    packet.push(0x0c);
    packet.extend_from_slice(&63_u16.to_le_bytes()); // binary character set
    packet.extend_from_slice(&1024_u32.to_le_bytes());
    packet.push(column.kind as u8);
    packet.extend_from_slice(&ColumnFlags::empty().bits().to_le_bytes());
    packet.extend_from_slice(&[0, 0, 0]); // decimals and filler
    packet
}

fn metadata(columns: &[ColumnSpec]) -> Vec<Vec<u8>> {
    assert!(!columns.is_empty());
    let mut packets = vec![vec![columns.len() as u8]];
    packets.extend(columns.iter().map(column_packet));
    packets.push(eof(false));
    packets
}

fn binary_set(columns: &[ColumnSpec], rows: &[Vec<Value>], more: bool) -> Vec<Vec<u8>> {
    let mut packets = metadata(columns);
    packets.extend(rows.iter().map(|row| binary_row(row)));
    packets.push(eof(more));
    packets
}

fn text_set(columns: &[ColumnSpec], rows: &[(i64, Vec<u8>)]) -> Vec<Vec<u8>> {
    let mut packets = metadata(columns);
    for (number, bytes) in rows {
        let mut row = Vec::new();
        push_bytes(&mut row, number.to_string().as_bytes());
        push_bytes(&mut row, bytes);
        packets.push(row);
    }
    packets.push(eof(false));
    packets
}

fn binary_row(values: &[Value]) -> Vec<u8> {
    let bitmap_length = (values.len() + 9) / 8;
    let mut packet = vec![0; 1 + bitmap_length];
    for (index, value) in values.iter().enumerate() {
        match value {
            Value::Int(number) => packet.extend_from_slice(&number.to_le_bytes()),
            Value::Bytes(bytes) => push_bytes(&mut packet, bytes),
            Value::NULL => packet[1 + (index + 2) / 8] |= 1 << ((index + 2) % 8),
            other => panic!("unsupported fixture value: {other:?}"),
        }
    }
    packet
}

fn push_bytes(packet: &mut Vec<u8>, bytes: &[u8]) {
    match bytes.len() {
        length @ 0..=250 => packet.push(length as u8),
        length @ 251..=65535 => {
            packet.push(0xfc);
            packet.extend_from_slice(&(length as u16).to_le_bytes());
        }
        length => {
            assert!(length < 1 << 24);
            packet.push(0xfd);
            packet.extend_from_slice(&(length as u32).to_le_bytes()[..3]);
        }
    }
    packet.extend_from_slice(bytes);
}

/// Test observation only: its owned output is deliberately outside an allocation meter.
fn observe(packet: &[u8], columns: &[Column]) -> Vec<Value> {
    assert_eq!(packet[0], 0);
    let bitmap_length = (columns.len() + 9) / 8;
    let mut data = &packet[1 + bitmap_length..];
    let mut values = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if packet[1 + (index + 2) / 8] & (1 << ((index + 2) % 8)) != 0 {
            values.push(Value::NULL);
            continue;
        }
        match column.column_type() {
            ColumnType::MYSQL_TYPE_LONGLONG => {
                assert!(!column.flags().contains(ColumnFlags::UNSIGNED_FLAG));
                values.push(Value::Int(i64::from_le_bytes(
                    data[..8].try_into().unwrap(),
                )));
                data = &data[8..];
            }
            ColumnType::MYSQL_TYPE_VAR_STRING | ColumnType::MYSQL_TYPE_BLOB => {
                let (length, prefix) = match data[0] {
                    0xfc => (
                        u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize,
                        3,
                    ),
                    0xfd => (
                        u32::from_le_bytes([data[1], data[2], data[3], 0]) as usize,
                        4,
                    ),
                    length @ 0..=250 => (length as usize, 1),
                    other => panic!("unsupported fixture length prefix: {other}"),
                };
                values.push(Value::Bytes(data[prefix..prefix + length].to_vec()));
                data = &data[prefix + length..];
            }
            other => panic!("unsupported fixture column: {other:?}"),
        }
    }
    assert!(
        data.is_empty(),
        "the callback observed the whole row packet"
    );
    values
}

fn observe_member(packet: &[u8], columns: &[Column]) -> i64 {
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].name_ref(), b"id");
    assert_eq!(columns[0].column_type(), ColumnType::MYSQL_TYPE_LONGLONG);
    assert!(!columns[0].flags().contains(ColumnFlags::UNSIGNED_FLAG));
    assert_eq!(&packet[..2], &[0, 0]);
    assert_eq!(packet.len(), 10);
    i64::from_le_bytes(packet[2..].try_into().unwrap())
}

fn members(numbers: &[i64]) -> Vec<Vec<Value>> {
    numbers
        .iter()
        .map(|&number| vec![Value::Int(number)])
        .collect()
}

fn pending(result: &QueryResult<'_, '_, BinaryProtocol>) -> bool {
    matches!(
        result.conn.get_pending_result(),
        Ok(Some(PendingResult::Pending(_)))
    )
}

fn framed(sequence: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 1 << 24);
    let mut frame = (payload.len() as u32).to_le_bytes()[..3].to_vec();
    frame.push(sequence);
    frame.extend_from_slice(payload);
    frame
}

async fn write_packet(stream: &mut TcpStream, sequence: u8, payload: &[u8]) {
    stream.write_all(&framed(sequence, payload)).await.unwrap();
}

async fn write_packets(stream: &mut TcpStream, mut sequence: u8, packets: &[Vec<u8>]) -> u8 {
    for packet in packets {
        write_packet(stream, sequence, packet).await;
        sequence = sequence.wrapping_add(1);
    }
    sequence
}

async fn read_packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0; 4];
    stream.read_exact(&mut header).await.unwrap();
    let length = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await.unwrap();
    (header[3], payload)
}

#[tokio::test]
async fn ordinary_text_and_binary_next_collect_stream_keep_parity() {
    let expected = vec![
        (-1, b"alpha".to_vec()),
        (0, b"".to_vec()),
        (i64::MAX, b"omega".to_vec()),
    ];
    let binary_rows: Vec<_> = expected
        .iter()
        .map(|(number, bytes)| vec![Value::Int(*number), Value::Bytes(bytes.clone())])
        .collect();
    let mut script: Vec<_> = (0..3)
        .map(|_| Exchange::query("TEXT", text_set(&PAIR, &expected)))
        .collect();
    script.push(Exchange::prepare("BINARY", &PAIR));
    script.extend((0..3).map(|_| Exchange::execute(binary_set(&PAIR, &binary_rows, false))));
    script.push(Exchange::quit());
    let (mut conn, server) = ScriptedServer::connect(script).await;

    for mode in 0..3 {
        let mut result = conn.query_iter("TEXT").await.unwrap();
        let rows = match mode {
            0 => {
                let mut rows = Vec::new();
                while let Some(row) = result.next().await.unwrap() {
                    rows.push(from_row::<(i64, Vec<u8>)>(row));
                }
                rows
            }
            1 => result.collect::<(i64, Vec<u8>)>().await.unwrap(),
            _ => result
                .stream::<(i64, Vec<u8>)>()
                .await
                .unwrap()
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
        };
        assert_eq!(rows, expected);
        result.drop_result().await.unwrap();
    }

    let statement = conn.prep("BINARY").await.unwrap();
    for mode in 0..3 {
        let mut result = conn.exec_iter(&statement, ()).await.unwrap();
        let rows = match mode {
            0 => {
                let mut rows = Vec::new();
                while let Some(row) = result.next().await.unwrap() {
                    rows.push(from_row::<(i64, Vec<u8>)>(row));
                }
                rows
            }
            1 => result.collect::<(i64, Vec<u8>)>().await.unwrap(),
            _ => result
                .stream::<(i64, Vec<u8>)>()
                .await
                .unwrap()
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap(),
        };
        assert_eq!(rows, expected);
        result.drop_result().await.unwrap();
    }
    server.finish(conn).await;
}

#[tokio::test]
async fn borrowed_member_and_five_column_packets_match_owned_rows() {
    let member_rows = members(&[i64::MIN, i64::MAX, -1, 0]);
    let dentry_rows = vec![
        vec![
            Value::Int(i64::MIN),
            Value::Int(i64::MAX),
            Value::Bytes("目录-🦀".as_bytes().to_vec()),
            Value::Bytes(vec![0, 0xff, 0x80]),
            Value::Int(-1),
        ],
        vec![
            Value::Int(0),
            Value::Int(-1),
            Value::Bytes(vec![]),
            Value::Bytes(vec![b'x'; 251]),
            Value::Int(0),
        ],
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Bytes(vec![0xff, b'a']),
            Value::NULL,
            Value::Int(i64::MAX),
        ],
    ];
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("MEMBERS", &MEMBER),
        Exchange::execute(binary_set(&MEMBER, &member_rows, false)),
        Exchange::execute(binary_set(&MEMBER, &member_rows, false)),
        Exchange::prepare_with_id("DENTRIES", &DENTRY, 2),
        Exchange::execute_with_id(binary_set(&DENTRY, &dentry_rows, false), 2),
        Exchange::execute_with_id(binary_set(&DENTRY, &dentry_rows, false), 2),
        Exchange::quit(),
    ])
    .await;

    for (sql, specifications, expected) in [
        ("MEMBERS", MEMBER.as_slice(), &member_rows),
        ("DENTRIES", DENTRY.as_slice(), &dentry_rows),
    ] {
        let statement = conn.prep(sql).await.unwrap();
        let owned: Vec<_> = conn
            .exec_iter(&statement, ())
            .await
            .unwrap()
            .collect_and_drop::<Row>()
            .await
            .unwrap()
            .into_iter()
            .map(Row::unwrap)
            .collect();
        let mut result = conn.exec_iter(&statement, ()).await.unwrap();
        let mut borrowed = Vec::new();
        while let Some(values) = result
            .next_binary_row_with(|packet, columns| {
                assert_eq!(columns.len(), specifications.len());
                for (column, specification) in columns.iter().zip(specifications) {
                    assert_eq!(column.name_ref(), specification.name);
                    assert_eq!(column.column_type(), specification.kind);
                    assert_eq!(column.character_set(), 63);
                    assert_eq!(column.column_length(), 1024);
                }
                observe(packet, columns)
            })
            .await
            .unwrap()
        {
            borrowed.push(values);
        }
        assert_eq!(borrowed, owned);
        assert_eq!(&borrowed, expected);
        assert!(!pending(&result));
        result.drop_result().await.unwrap();
    }
    server.finish(conn).await;
}

#[tokio::test]
async fn eof_empty_columns_and_no_pending_never_invoke_the_callback() {
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("EMPTY", &MEMBER),
        Exchange::execute(binary_set(&MEMBER, &[], false)),
        Exchange::execute(vec![ok(false)]),
        Exchange::query("AFTER", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("EMPTY").await.unwrap();
    let calls = Cell::new(0);
    for _ in 0..2 {
        let mut result = conn.exec_iter(&statement, ()).await.unwrap();
        for _ in 0..2 {
            assert_eq!(
                result
                    .next_binary_row_with(|_, _| calls.set(calls.get() + 1))
                    .await
                    .unwrap(),
                None
            );
            assert!(!result.conn.has_pending_result());
        }
        result.drop_result().await.unwrap();
    }
    assert_eq!(calls.get(), 0);
    conn.query_drop("AFTER").await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn pending_eof_returns_none_before_the_next_result_set() {
    let mut packets = binary_set(&MEMBER, &members(&[1]), true);
    packets.extend(binary_set(&MEMBER, &members(&[2]), false));
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("MULTI", &MEMBER),
        Exchange::execute(packets),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("MULTI").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (Some(1), false)
    );
    let calls = Cell::new(0);
    assert_eq!(
        result
            .next_binary_row_with_boundary(|_, _| calls.set(calls.get() + 1))
            .await
            .unwrap(),
        (None, true)
    );
    assert_eq!(calls.get(), 0);
    assert!(pending(&result));
    assert!(!result.is_empty());
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (Some(2), false)
    );
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (None, false)
    );
    assert!(result.is_empty());
    result.drop_result().await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn boundary_flag_retains_an_extra_empty_set_before_drain_and_reuse() {
    let mut packets = binary_set(&MEMBER, &members(&[1]), true);
    packets.push(ok(false));
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("EXTRA EMPTY", &MEMBER),
        Exchange::execute(packets),
        Exchange::query("AFTER EXTRA EMPTY", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("EXTRA EMPTY").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (Some(1), false)
    );
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (None, true)
    );
    assert!(
        result.is_empty(),
        "an empty installed set has no rows or server-more flag"
    );
    assert!(
        pending(&result),
        "the extra empty set still requires settlement"
    );
    let calls = Cell::new(0);
    assert_eq!(
        result
            .next_binary_row_with_boundary(|_, _| calls.set(calls.get() + 1))
            .await
            .unwrap(),
        (None, false)
    );
    assert_eq!(calls.get(), 0);
    assert!(!result.conn.has_pending_result());
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (None, false)
    );
    result.drop_result().await.unwrap();
    conn.query_drop("AFTER EXTRA EMPTY").await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn empty_columns_advance_the_next_set_without_invoking_the_callback() {
    let mut packets = vec![ok(true)];
    packets.extend(binary_set(&MEMBER, &members(&[7]), false));
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("EMPTY THEN ROW", &MEMBER),
        Exchange::execute(packets),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("EMPTY THEN ROW").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(
        result
            .next_binary_row_with(|_, _| -> () { panic!("empty set callback") })
            .await
            .unwrap(),
        None
    );
    assert!(pending(&result));
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        Some(7)
    );
    result.drop_result().await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn a_dropped_taken_stream_is_drained_before_the_raw_callback() {
    let mut packets = binary_set(&MEMBER, &members(&[1, 2, 3]), true);
    packets.extend(binary_set(&MEMBER, &members(&[9]), false));
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("TAKEN", &MEMBER),
        Exchange::execute(packets),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("TAKEN").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    let mut stream = result.stream::<i64>().await.unwrap().unwrap();
    assert_eq!(stream.try_next().await.unwrap(), Some(1));
    drop(stream);
    assert!(matches!(
        result.conn.get_pending_result(),
        Ok(Some(PendingResult::Taken(_)))
    ));
    let mut calls = 0;
    assert_eq!(
        result
            .next_binary_row_with(|packet, columns| {
                calls += 1;
                observe_member(packet, columns)
            })
            .await
            .unwrap(),
        Some(9)
    );
    assert_eq!(calls, 1, "taken rows 2 and 3 use ordinary cleanup");
    result.drop_result().await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn callback_errors_remain_inner_and_the_same_result_can_be_drained() {
    #[derive(Debug, PartialEq)]
    enum Rejection {
        InvalidHint,
    }
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("REJECT", &MEMBER),
        Exchange::execute(binary_set(&MEMBER, &members(&[1, 2, 3]), false)),
        Exchange::query("AFTER DRAIN", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("REJECT").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        Some(1)
    );
    assert_eq!(
        result
            .next_binary_row_with(|packet, columns| {
                assert_eq!(observe_member(packet, columns), 2);
                Err::<(), _>(Rejection::InvalidHint)
            })
            .await
            .unwrap(),
        Some(Err(Rejection::InvalidHint))
    );
    assert!(pending(&result));
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        Some(3)
    );
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        None
    );
    assert!(!result.conn.has_pending_result());
    result.drop_result().await.unwrap();
    conn.query_drop("AFTER DRAIN").await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn server_row_errors_match_owned_errors_and_clear_pending_state() {
    for borrowed in [false, true] {
        let mut packets = metadata(&MEMBER);
        packets.push(server_error());
        let (mut conn, server) = ScriptedServer::connect(vec![
            Exchange::prepare("SERVER ERROR", &MEMBER),
            Exchange::execute(packets),
            Exchange::query("AFTER ERROR", vec![ok(false)]),
            Exchange::quit(),
        ])
        .await;
        let statement = conn.prep("SERVER ERROR").await.unwrap();
        let mut result = conn.exec_iter(&statement, ()).await.unwrap();
        let calls = Cell::new(0);
        let error = if borrowed {
            result
                .next_binary_row_with(|_, _| calls.set(calls.get() + 1))
                .await
                .unwrap_err()
        } else {
            result.next().await.unwrap_err()
        };
        assert!(
            matches!(error, Error::Server(ServerError { code: 1064, ref state, ref message })
            if state == "42000" && message == "scripted row failure")
        );
        assert_eq!(calls.get(), 0);
        assert!(!result.conn.has_pending_result());
        assert!(!result.conn.is_disconnected());
        result.drop_result().await.unwrap();
        conn.query_drop("AFTER ERROR").await.unwrap();
        server.finish(conn).await;
    }
}

#[tokio::test]
async fn pending_setter_error_keeps_precedence_over_the_packet_read_error() {
    let mut packets = metadata(&MEMBER);
    packets.push(server_error());
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("SETTER PRECEDENCE", &MEMBER),
        Exchange::execute(packets),
        Exchange::query("AFTER SETTER ERROR", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("SETTER PRECEDENCE").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    let columns = result.columns().unwrap();
    let injected = ServerError {
        code: 1234,
        state: "HY000".to_owned(),
        message: "pending setter error".to_owned(),
    };
    assert!(result
        .conn
        .as_mut()
        .set_pending_result_error(injected.clone())
        .unwrap()
        .is_some());
    let calls = Cell::new(0);
    // Enter the shared packet reader directly so the existing setter error remains pending
    // until the scripted row read fails. Public dispatch would consume it before reading.
    let error = result
        .next_row_with(columns, |_, _| {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .await
        .unwrap_err();
    match error {
        Error::Server(error) => assert_eq!(error, injected),
        other => panic!("expected pending setter error, got {other:?}"),
    }
    assert_eq!(calls.get(), 0);
    assert!(!result.conn.has_pending_result());
    result.drop_result().await.unwrap();
    conn.query_drop("AFTER SETTER ERROR").await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn truncated_row_errors_match_owned_errors_and_disconnect() {
    for borrowed in [false, true] {
        let (mut conn, server) = ScriptedServer::connect(vec![
            Exchange::prepare("TRUNCATED", &MEMBER),
            Exchange {
                command: execute_command(),
                reply: Reply::Truncated(metadata(&MEMBER)),
            },
        ])
        .await;
        let statement = conn.prep("TRUNCATED").await.unwrap();
        let mut result = conn.exec_iter(&statement, ()).await.unwrap();
        let calls = Cell::new(0);
        let error = if borrowed {
            result
                .next_binary_row_with(|_, _| calls.set(calls.get() + 1))
                .await
                .unwrap_err()
        } else {
            result.next().await.unwrap_err()
        };
        assert!(matches!(error, Error::Io(_)));
        assert_eq!(calls.get(), 0);
        assert!(!result.conn.has_pending_result());
        assert!(result.conn.is_disconnected());
        drop(result);
        server.finish(conn).await;
    }
}

#[tokio::test]
async fn a_later_drain_error_is_outer_and_does_not_run_a_fallback() {
    let mut packets = metadata(&MEMBER);
    packets.push(binary_row(&[Value::Int(1)]));
    packets.push(server_error());
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("DRAIN ERROR", &MEMBER),
        Exchange::execute(packets),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("DRAIN ERROR").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(
        result
            .next_binary_row_with(|_, _| Err::<(), _>("hint miss"))
            .await
            .unwrap(),
        Some(Err("hint miss"))
    );
    assert!(pending(&result));
    let calls = Cell::new(0);
    assert!(matches!(
        result
            .next_binary_row_with(|_, _| calls.set(calls.get() + 1))
            .await,
        Err(Error::Server(_))
    ));
    assert_eq!(calls.get(), 0);
    assert!(!result.conn.has_pending_result());
    result.drop_result().await.unwrap();
    // The script accepts only QUIT here: an owned retry would fail its command assertion.
    server.finish(conn).await;
}

#[tokio::test]
async fn mixed_owned_and_borrowed_reads_do_not_repeat_or_lose_rows() {
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("MIXED", &MEMBER),
        Exchange::execute(binary_set(&MEMBER, &members(&[1, 2, 3, 4]), false)),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("MIXED").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(from_row::<i64>(result.next().await.unwrap().unwrap()), 1);
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        Some(2)
    );
    assert_eq!(from_row::<i64>(result.next().await.unwrap().unwrap()), 3);
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        Some(4)
    );
    assert!(result.next().await.unwrap().is_none());
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        None
    );
    result.drop_result().await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn ordinary_decode_errors_preserve_pending_for_later_cleanup() {
    let mut packets = metadata(&MEMBER);
    packets.push(vec![0, 0]); // Valid row framing, missing the required i64.
    packets.push(binary_row(&[Value::Int(8)]));
    packets.push(eof(false));
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("DECODE ERROR", &MEMBER),
        Exchange::execute(packets),
        Exchange::query("AFTER DECODE", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("DECODE ERROR").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert!(result.next().await.is_err());
    assert!(
        pending(&result),
        "owned decoder errors retain upstream state"
    );
    assert_eq!(
        result.next_binary_row_with(observe_member).await.unwrap(),
        Some(8)
    );
    result.drop_result().await.unwrap();
    conn.query_drop("AFTER DECODE").await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn callback_and_output_may_be_non_send_or_capture_an_external_reference() {
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("LIFETIMES", &MEMBER),
        Exchange::execute(binary_set(&MEMBER, &members(&[1, 2, 3]), false)),
        Exchange::quit(),
    ])
    .await;
    let statement = conn.prep("LIFETIMES").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    let state = Rc::new(Cell::new(0));
    let output = result
        .next_binary_row_with({
            let state = state.clone();
            move |packet, columns| {
                state.set(observe_member(packet, columns));
                state
            }
        })
        .await
        .unwrap()
        .unwrap();
    assert!(Rc::ptr_eq(&output, &state));
    assert_eq!(state.get(), 1);
    let external = String::from("external owner");
    let borrowed_external = result
        .next_binary_row_with(|_, _| external.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(borrowed_external, "external owner");
    fn assert_send<T: Send>(_: &T) {}
    let scalar_future =
        result.next_binary_row_with(|packet, columns| Ok::<_, ()>(observe_member(packet, columns)));
    assert_send(&scalar_future);
    assert_eq!(scalar_future.await.unwrap(), Some(Ok(3)));
    result.drop_result().await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn cancelling_a_partial_row_retains_dirty_cleanup_and_transaction_rollback() {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::query("START TRANSACTION", vec![ok(false)]),
        Exchange::prepare("CANCEL ROW", &MEMBER),
        Exchange {
            command: execute_command(),
            reply: Reply::Paused {
                before: metadata(&MEMBER),
                packet: binary_row(&[Value::Int(1)]),
                split: 7,
                ready: ready_tx,
                release: release_rx,
                after: vec![binary_row(&[Value::Int(2)]), eof(false)],
                close_on_release: false,
            },
        },
        Exchange::query("ROLLBACK", vec![ok(false)]),
        Exchange::query("AFTER CANCEL", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let mut tx = conn.start_transaction(TxOpts::default()).await.unwrap();
    let statement = tx.prep("CANCEL ROW").await.unwrap();
    let mut result = tx.exec_iter(&statement, ()).await.unwrap();
    ready_rx.await.unwrap();
    let calls = Cell::new(0);
    let mut read = Box::pin(result.next_binary_row_with(|_, _| calls.set(calls.get() + 1)));
    assert!(futures_util::poll!(read.as_mut()).is_pending());
    drop(read);
    assert_eq!(calls.get(), 0, "cancelled row produced no callback output");
    assert!(pending(&result));
    assert!(
        !result.conn.is_disconnected(),
        "row waits retain ordinary cancellation behavior"
    );
    drop(result);
    drop(tx);
    release_tx.send(()).unwrap();
    // clean_dirty must drain both rows before sending ROLLBACK and this command.
    conn.query_drop("AFTER CANCEL").await.unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn cancelling_query_initiation_keeps_the_existing_disconnect_guard() {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (mut conn, server) = ScriptedServer::connect(vec![Exchange {
        command: command(0x03, b"CANCEL INIT"),
        reply: Reply::Blocked {
            ready: ready_tx,
            release: release_rx,
        },
    }])
    .await;
    let mut query = Box::pin(conn.query_iter("CANCEL INIT"));
    tokio::select! {
        result = query.as_mut() => panic!("blocked query completed: {result:?}"),
        ready = ready_rx => ready.unwrap(),
    }
    drop(query);
    assert!(conn.is_disconnected());
    release_tx.send(()).unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn cancelling_next_set_keeps_the_existing_disconnect_guard() {
    let (ready_tx, ready_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::prepare("CANCEL NEXT SET", &MEMBER),
        Exchange {
            command: execute_command(),
            reply: Reply::Paused {
                before: binary_set(&MEMBER, &members(&[1]), true),
                packet: vec![1], // The next result set's column-count packet.
                split: 2,        // Hold an incomplete framing header, before metadata can complete.
                ready: ready_tx,
                release: release_rx,
                after: vec![],
                close_on_release: true,
            },
        },
    ])
    .await;
    let statement = conn.prep("CANCEL NEXT SET").await.unwrap();
    let mut result = conn.exec_iter(&statement, ()).await.unwrap();
    assert_eq!(
        result
            .next_binary_row_with_boundary(observe_member)
            .await
            .unwrap(),
        (Some(1), false)
    );
    ready_rx.await.unwrap();
    let calls = Cell::new(0);
    timeout(SCRIPT_TIMEOUT, async {
        loop {
            let mut read =
                Box::pin(result.next_binary_row_with_boundary(|_, _| calls.set(calls.get() + 1)));
            assert!(
                futures_util::poll!(read.as_mut()).is_pending(),
                "next metadata is deliberately incomplete"
            );
            drop(read);
            if result.conn.is_disconnected() {
                break;
            }
            // A poll may initially wait for socket readiness while reading the first EOF.
            // Cancellation of that row wait leaves framing in Conn; another poll resumes it.
            // Stop immediately when the actual NextSetRoutine has installed its guard.
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("next-set routine did not receive its scripted EOF");
    assert_eq!(calls.get(), 0);
    assert!(
        result.conn.is_disconnected(),
        "cancelling a next-set routine cannot reuse its connection"
    );
    drop(result);
    release_tx.send(()).unwrap();
    server.finish(conn).await;
}

#[tokio::test]
async fn a_caught_callback_panic_leaves_cleanup_and_rollback_observable() {
    let (mut conn, server) = ScriptedServer::connect(vec![
        Exchange::query("START TRANSACTION", vec![ok(false)]),
        Exchange::prepare("PANIC ROW", &MEMBER),
        Exchange::execute(binary_set(&MEMBER, &members(&[1, 2, 3]), false)),
        Exchange::query("ROLLBACK", vec![ok(false)]),
        Exchange::query("AFTER PANIC", vec![ok(false)]),
        Exchange::quit(),
    ])
    .await;
    let mut tx = conn.start_transaction(TxOpts::default()).await.unwrap();
    let statement = tx.prep("PANIC ROW").await.unwrap();
    let mut result = tx.exec_iter(&statement, ()).await.unwrap();
    let calls = Cell::new(0);
    let outcome = AssertUnwindSafe(result.next_binary_row_with(|packet, columns| -> () {
        calls.set(calls.get() + 1);
        assert_eq!(observe_member(packet, columns), 1);
        panic!("scripted callback panic")
    }))
    .catch_unwind()
    .await;
    assert!(
        outcome.is_err(),
        "panic did not produce a successful observation"
    );
    assert_eq!(calls.get(), 1);
    assert!(pending(&result));
    drop(result);
    drop(tx);
    conn.query_drop("AFTER PANIC").await.unwrap();
    server.finish(conn).await;
}
