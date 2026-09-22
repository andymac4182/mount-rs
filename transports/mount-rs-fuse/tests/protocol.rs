use mount_rs_fuse::constants::*;
use mount_rs_fuse::protocol::*;

fn hex(value: &str) -> Vec<u8> {
    let compact: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    assert!(compact.len().is_multiple_of(2), "fixture has a half byte");
    compact
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

fn zeros(count: usize) -> Vec<u8> {
    vec![0; count]
}

fn append_hex(target: &mut Vec<u8>, value: &str) {
    target.extend(hex(value));
}

#[derive(Debug)]
struct OracleBodyFixture {
    direction: String,
    opcode: u32,
    context: ProtocolContext,
    bytes: Vec<u8>,
}

fn oracle_fixture(name: &str) -> OracleBodyFixture {
    let row = include_str!("fixtures/protocol-oracle.txt")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find(|line| line.split('|').next() == Some(name))
        .unwrap_or_else(|| panic!("missing oracle fixture {name}"));
    let fields: Vec<_> = row.split('|').collect();
    assert_eq!(fields.len(), 6, "malformed oracle fixture row: {row}");
    OracleBodyFixture {
        direction: fields[1].to_owned(),
        opcode: fields[2].parse().unwrap(),
        context: ProtocolContext {
            minor: fields[3].parse().unwrap(),
            setxattr_ext: fields[4] == "1",
        },
        bytes: hex(fields[5]),
    }
}

fn assert_request_oracle(name: &str, opcode: u32, context: ProtocolContext, body: FuseRequestBody) {
    let fixture = oracle_fixture(name);
    assert_eq!(fixture.direction, "request", "{name}");
    assert_eq!(fixture.opcode, opcode, "{name}");
    assert_eq!(fixture.context, context, "{name}");
    assert_eq!(
        encode_request_body(opcode, &body, Some(context)).unwrap(),
        fixture.bytes,
        "Rust request encoder differs from mountx oracle for {name}"
    );
    assert_eq!(
        decode_request_body(opcode, &fixture.bytes, Some(context)).unwrap(),
        body,
        "Rust request decoder differs from mountx oracle for {name}"
    );
}

fn assert_reply_oracle(name: &str, opcode: u32, context: ProtocolContext, body: FuseReplyBody) {
    let fixture = oracle_fixture(name);
    assert_eq!(fixture.direction, "reply", "{name}");
    assert_eq!(fixture.opcode, opcode, "{name}");
    assert_eq!(fixture.context, context, "{name}");
    assert_eq!(
        encode_reply_body(opcode, &body, Some(context)).unwrap(),
        fixture.bytes,
        "Rust reply encoder differs from mountx oracle for {name}"
    );
    assert_eq!(
        decode_reply_body(opcode, &fixture.bytes, Some(context)).unwrap(),
        body,
        "Rust reply decoder differs from mountx oracle for {name}"
    );
}

fn regular_attr() -> FuseAttr {
    FuseAttr {
        ino: 2,
        size: 13,
        blocks: 1,
        atime: 1_000_000_000,
        mtime: 1_000_000_000,
        ctime: 1_000_000_000,
        atime_nsec: 0,
        mtime_nsec: 0,
        ctime_nsec: 0,
        mode: 0o100_644,
        nlink: 1,
        uid: 1000,
        gid: 1000,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn oracle_attr(old: bool) -> FuseAttr {
    FuseAttr {
        ino: 0x1122_3344_5566_7788,
        size: 0x0102_0304_0506_0708,
        blocks: 0x1112_1314_1516_1718,
        atime: 0x2122_2324_2526_2728,
        mtime: 0x3132_3334_3536_3738,
        ctime: 0x4142_4344_4546_4748,
        atime_nsec: 101,
        mtime_nsec: 202,
        ctime_nsec: 303,
        mode: 0o100640,
        nlink: 3,
        uid: 1001,
        gid: 1002,
        rdev: 0x1234,
        blksize: if old { 0 } else { 4096 },
        flags: if old { 0 } else { 0xa5a5_a5a5 },
    }
}

fn oracle_lock(lock_type: u32, pid: u32) -> FuseFileLock {
    FuseFileLock {
        start: 0x0102_0304_0506_0708,
        end: 0x1112_1314_1516_1718,
        type_: lock_type,
        pid,
    }
}

fn oracle_entry() -> FuseEntryOut {
    FuseEntryOut {
        nodeid: 0x1020_3040_5060_7080,
        generation: 0x0102_0304_0506_0708,
        entry_valid: 11,
        attr_valid: 13,
        entry_valid_nsec: 17,
        attr_valid_nsec: 19,
        attr: oracle_attr(false),
    }
}

fn append_regular_attr(target: &mut Vec<u8>, ino: &str, size: &str, uid: &str, gid: &str) {
    append_hex(target, ino);
    append_hex(target, size);
    append_hex(target, "01000000 00000000");
    append_hex(target, "00ca9a3b 00000000");
    append_hex(target, "00ca9a3b 00000000");
    append_hex(target, "00ca9a3b 00000000");
    append_hex(target, "00000000 00000000 00000000");
    append_hex(target, "a4810000 01000000");
    append_hex(target, uid);
    append_hex(target, gid);
    append_hex(target, "00000000 00100000 00000000");
}

#[test]
fn init_request_and_reply_match_upstream_golden_bytes() {
    let request = encode_request(
        &EncodeRequest {
            opcode: FUSE_INIT,
            unique: 2,
            body: Some(FuseRequestBody::Init(FuseInitIn {
                major: 7,
                minor: 41,
                max_readahead: 131_072,
                flags: 0x3b,
                flags2: 1,
            })),
            ..EncodeRequest::default()
        },
        None,
    )
    .unwrap();
    let mut expected_request = hex(concat!(
        "68000000 1a000000 02000000 00000000 00000000 00000000 ",
        "00000000 00000000 00000000 0000 0000 ",
        "07000000 29000000 00000200 3b000000 01000000"
    ));
    expected_request.extend(zeros(44));
    assert_eq!(request, expected_request);
    assert_eq!(
        decode_request(&request, None).unwrap().body,
        Some(FuseRequestBody::Init(FuseInitIn {
            major: 7,
            minor: 41,
            max_readahead: 131_072,
            flags: 0x3b,
            flags2: 1,
        },))
    );

    let init = FuseInitOut {
        major: 7,
        minor: 41,
        max_readahead: 131_072,
        flags: 0x3b,
        max_background: 64,
        congestion_threshold: 48,
        max_write: 1_048_576,
        time_gran: 1,
        max_pages: 256,
        map_alignment: 0,
        flags2: 0,
        max_stack_depth: 0,
    };
    let reply = encode_reply_for(2, FUSE_INIT, &FuseReplyBody::Init(init), None).unwrap();
    let mut expected_reply = hex(concat!(
        "50000000 00000000 02000000 00000000 ",
        "07000000 29000000 00000200 3b000000 4000 3000 ",
        "00001000 01000000 0001 0000 00000000 00000000"
    ));
    expected_reply.extend(zeros(24));
    assert_eq!(reply, expected_reply);
    assert_eq!(
        decode_reply(&reply, FUSE_INIT, None).unwrap().body,
        Some(FuseReplyBody::Init(init))
    );
}

#[test]
fn lookup_request_and_entry_reply_match_upstream_golden_bytes() {
    let request = encode_request(
        &EncodeRequest {
            opcode: FUSE_LOOKUP,
            unique: 3,
            nodeid: FUSE_ROOT_ID,
            uid: 1000,
            gid: 1000,
            pid: 4242,
            body: Some(FuseRequestBody::Name(FuseNameIn {
                name: "readme.md".to_owned(),
            })),
            ..EncodeRequest::default()
        },
        None,
    )
    .unwrap();
    let expected_request = hex(concat!(
        "32000000 01000000 03000000 00000000 01000000 00000000 ",
        "e8030000 e8030000 92100000 0000 0000 ",
        "726561646d652e6d6400"
    ));
    assert_eq!(request, expected_request);
    assert_eq!(
        decode_request(&request, None).unwrap().body,
        Some(FuseRequestBody::Name(FuseNameIn {
            name: "readme.md".to_owned(),
        }))
    );

    let entry = FuseEntryOut {
        nodeid: 2,
        generation: 1,
        entry_valid: 1,
        attr_valid: 1,
        entry_valid_nsec: 0,
        attr_valid_nsec: 0,
        attr: regular_attr(),
    };
    let reply =
        encode_reply_for(3, FUSE_LOOKUP, &FuseReplyBody::Entry(entry.clone()), None).unwrap();
    let mut expected_reply = hex(concat!(
        "90000000 00000000 03000000 00000000 ",
        "02000000 00000000 01000000 00000000 ",
        "01000000 00000000 01000000 00000000 00000000 00000000"
    ));
    append_regular_attr(
        &mut expected_reply,
        "02000000 00000000",
        "0d000000 00000000",
        "e8030000",
        "e8030000",
    );
    assert_eq!(expected_reply.len(), 144);
    assert_eq!(reply, expected_reply);
    assert_eq!(
        decode_reply(&reply, FUSE_LOOKUP, None).unwrap().body,
        Some(FuseReplyBody::Entry(entry))
    );
}

#[test]
fn readdirplus_page_matches_upstream_golden_bytes() {
    let zero_entry = FuseEntryOut {
        nodeid: 0,
        generation: 0,
        entry_valid: 0,
        attr_valid: 0,
        entry_valid_nsec: 0,
        attr_valid_nsec: 0,
        attr: FuseAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            atime_nsec: 0,
            mtime_nsec: 0,
            ctime_nsec: 0,
            mode: 0,
            nlink: 0,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 0,
            flags: 0,
        },
    };
    let entries = vec![
        FuseDirentPlus {
            entry: zero_entry,
            dirent: FuseDirent {
                ino: 1,
                off: 1,
                type_: DT_DIR,
                name: ".".to_owned(),
            },
        },
        FuseDirentPlus {
            entry: FuseEntryOut {
                nodeid: 3,
                generation: 0,
                entry_valid: 1,
                attr_valid: 1,
                entry_valid_nsec: 0,
                attr_valid_nsec: 0,
                attr: FuseAttr {
                    ino: 3,
                    size: 5,
                    uid: 0,
                    gid: 0,
                    ..regular_attr()
                },
            },
            dirent: FuseDirent {
                ino: 3,
                off: 2,
                type_: DT_REG,
                name: "file".to_owned(),
            },
        },
    ];
    let packed = pack_dirents_plus(&entries, 4096, None).unwrap();
    assert_eq!(packed.packed, 2);
    assert_eq!(packed.buffer.len(), 320);

    let mut expected = zeros(128);
    append_hex(
        &mut expected,
        "01000000 00000000 01000000 00000000 01000000 04000000 2e",
    );
    expected.extend(zeros(7));
    append_hex(
        &mut expected,
        concat!(
            "03000000 00000000 00000000 00000000 01000000 00000000 ",
            "01000000 00000000 00000000 00000000"
        ),
    );
    append_regular_attr(
        &mut expected,
        "03000000 00000000",
        "05000000 00000000",
        "00000000",
        "00000000",
    );
    append_hex(
        &mut expected,
        "03000000 00000000 02000000 00000000 04000000 08000000 66696c65",
    );
    expected.extend(zeros(4));

    assert_eq!(expected.len(), 320);
    assert_eq!(packed.buffer, expected);
    assert_eq!(unpack_dirents_plus(&expected, None).unwrap(), entries);
}

#[test]
fn write_payload_and_reply_match_upstream_golden_bytes() {
    let body = FuseWriteIn {
        fh: 0x01_02_03_04_05_06_07_08,
        offset: 4096,
        size: 5,
        write_flags: 0,
        lock_owner: 0,
        flags: 0o100_001,
        data: b"hello".to_vec(),
    };
    let request = encode_request(
        &EncodeRequest {
            opcode: FUSE_WRITE,
            unique: 5,
            nodeid: 3,
            uid: 1000,
            gid: 1000,
            pid: 4242,
            body: Some(FuseRequestBody::Write(body.clone())),
            ..EncodeRequest::default()
        },
        None,
    )
    .unwrap();
    let expected_request = hex(concat!(
        "55000000 10000000 05000000 00000000 03000000 00000000 ",
        "e8030000 e8030000 92100000 0000 0000 ",
        "08070605 04030201 00100000 00000000 05000000 00000000 ",
        "00000000 00000000 01800000 00000000 68656c6c6f"
    ));
    assert_eq!(request, expected_request);
    assert_eq!(
        decode_request(&request, None).unwrap().body,
        Some(FuseRequestBody::Write(body))
    );

    let reply = encode_reply_for(
        5,
        FUSE_WRITE,
        &FuseReplyBody::Write(FuseWriteOut { size: 5 }),
        None,
    )
    .unwrap();
    assert_eq!(
        reply,
        hex("18000000 00000000 05000000 00000000 05000000 00000000")
    );
}

#[test]
fn ioctl_request_and_reply_have_strict_typed_codecs() {
    let request = FuseRequestBody::Ioctl(FuseIoctlIn {
        fh: 0x0102_0304_0506_0708,
        flags: 0x1112_1314,
        cmd: 0x2122_2324,
        arg: 0x3132_3334_3536_3738,
        in_size: 2,
        out_size: 0x5152_5354,
        data: vec![0xa5, 0x5a],
    });
    let request_bytes = encode_request_body(FUSE_IOCTL, &request, None).unwrap();
    assert_eq!(request_bytes.len(), 34);
    assert_eq!(
        decode_request_body(FUSE_IOCTL, &request_bytes, None).unwrap(),
        request
    );
    assert!(decode_request_body(FUSE_IOCTL, &request_bytes[..33], None).is_err());
    assert!(
        decode_request_body(FUSE_IOCTL, &[request_bytes.as_slice(), &[0]].concat(), None).is_err()
    );
    assert!(
        encode_request_body(
            FUSE_IOCTL,
            &FuseRequestBody::Ioctl(FuseIoctlIn {
                fh: 0x0102_0304_0506_0708,
                flags: 0x1112_1314,
                cmd: 0x2122_2324,
                arg: 0x3132_3334_3536_3738,
                in_size: 1,
                out_size: 0x5152_5354,
                data: Vec::new(),
            }),
            None,
        )
        .is_err()
    );

    let reply = FuseReplyBody::Ioctl(FuseIoctlOut {
        result: -25,
        flags: 0x6162_6364,
        in_iovs: 0x7172_7374,
        out_iovs: 0x8182_8384,
    });
    let reply_bytes = encode_reply_body(FUSE_IOCTL, &reply, None).unwrap();
    assert_eq!(reply_bytes.len(), 16);
    assert_eq!(
        decode_reply_body(FUSE_IOCTL, &reply_bytes, None).unwrap(),
        reply
    );
    assert!(decode_reply_body(FUSE_IOCTL, &reply_bytes[..15], None).is_err());
    assert!(decode_reply_body(FUSE_IOCTL, &[reply_bytes.as_slice(), &[0]].concat(), None).is_err());
}

#[test]
fn remaining_typed_families_match_pinned_mountx_oracle_fixtures() {
    assert_reply_oracle(
        "attr-out-old",
        FUSE_GETATTR,
        ProtocolContext {
            minor: 8,
            setxattr_ext: false,
        },
        FuseReplyBody::Attr(FuseAttrOut {
            attr_valid: 23,
            attr_valid_nsec: 29,
            attr: oracle_attr(true),
        }),
    );
    assert_reply_oracle(
        "attr-out-new",
        FUSE_GETATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Attr(FuseAttrOut {
            attr_valid: 23,
            attr_valid_nsec: 29,
            attr: oracle_attr(false),
        }),
    );
    assert_reply_oracle(
        "setattr-reply",
        FUSE_SETATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Attr(FuseAttrOut {
            attr_valid: 23,
            attr_valid_nsec: 29,
            attr: oracle_attr(false),
        }),
    );

    assert_request_oracle(
        "xattr-set-legacy",
        FUSE_SETXATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Setxattr(FuseSetxattrIn {
            flags: 1,
            setxattr_flags: 0,
            name: "user.mountx".to_owned(),
            value: vec![0, 1, 2, 3, 255],
        }),
    );
    assert_request_oracle(
        "xattr-set-ext",
        FUSE_SETXATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: true,
        },
        FuseRequestBody::Setxattr(FuseSetxattrIn {
            flags: 2,
            setxattr_flags: 1,
            name: "trusted.mountx".to_owned(),
            value: vec![9, 8, 7, 6],
        }),
    );
    assert_reply_oracle(
        "xattr-set-empty-reply",
        FUSE_SETXATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "xattr-get-request",
        FUSE_GETXATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Getxattr(FuseGetxattrIn {
            size: 128,
            name: "user.mountx".to_owned(),
        }),
    );
    assert_reply_oracle(
        "xattr-list-reply",
        FUSE_LISTXATTR,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Raw(b"user.mountx\0user.other\0".to_vec()),
    );

    assert_request_oracle(
        "lock-getlk-request",
        FUSE_GETLK,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Lk(FuseLkIn {
            fh: 0x2122_2324_2526_2728,
            owner: 0x3132_3334_3536_3738,
            lk: oracle_lock(1, 4242),
            lk_flags: 1,
        }),
    );
    assert_reply_oracle(
        "lock-getlk-reply",
        FUSE_GETLK,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Lk(FuseLkOut {
            lk: oracle_lock(2, 777),
        }),
    );
    assert_request_oracle(
        "lock-setlk-request",
        FUSE_SETLK,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Lk(FuseLkIn {
            fh: 0x2122_2324_2526_2728,
            owner: 0x3132_3334_3536_3738,
            lk: oracle_lock(1, 4242),
            lk_flags: 0,
        }),
    );
    assert_reply_oracle(
        "lock-setlk-empty-reply",
        FUSE_SETLK,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Empty,
    );

    assert_request_oracle(
        "rename-request",
        FUSE_RENAME,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Rename(FuseRenameIn {
            newdir: 0x1020_3040_5060_7080,
            old_name: "old-name".to_owned(),
            new_name: "new-name".to_owned(),
        }),
    );
    assert_request_oracle(
        "rename2-request",
        FUSE_RENAME2,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Rename2(FuseRename2In {
            newdir: 0x1020_3040_5060_7080,
            flags: 3,
            old_name: "old-name".to_owned(),
            new_name: "new-name".to_owned(),
        }),
    );
    assert_reply_oracle(
        "rename-empty-reply",
        FUSE_RENAME,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Empty,
    );
    assert_reply_oracle(
        "rename2-empty-reply",
        FUSE_RENAME2,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "create-request",
        FUSE_CREATE,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Create(FuseCreateIn {
            flags: 0x241,
            mode: 0o100640,
            umask: 0o022,
            open_flags: 1,
            name: "created".to_owned(),
        }),
    );
    assert_reply_oracle(
        "create-reply",
        FUSE_CREATE,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Create(FuseCreateOut {
            entry: oracle_entry(),
            open: FuseOpenOut {
                fh: 0x8877_6655_4433_2211,
                open_flags: 0x42,
                backing_id: -7,
            },
        }),
    );

    assert_reply_oracle(
        "statfs-old",
        FUSE_STATFS,
        ProtocolContext {
            minor: 3,
            setxattr_ext: false,
        },
        FuseReplyBody::Statfs(FuseKstatfs {
            blocks: 101,
            bfree: 202,
            bavail: 303,
            files: 404,
            ffree: 505,
            bsize: 4096,
            namelen: 255,
            frsize: 0,
        }),
    );
    assert_reply_oracle(
        "statfs-new",
        FUSE_STATFS,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Statfs(FuseKstatfs {
            blocks: 101,
            bfree: 202,
            bavail: 303,
            files: 404,
            ffree: 505,
            bsize: 4096,
            namelen: 255,
            frsize: 4096,
        }),
    );

    assert_request_oracle(
        "poll-request",
        FUSE_POLL,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Poll(FusePollIn {
            fh: 0x0102_0304_0506_0708,
            kh: 0x1112_1314_1516_1718,
            flags: 1,
            events: 5,
        }),
    );
    assert_reply_oracle(
        "poll-reply",
        FUSE_POLL,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Poll(FusePollOut { revents: 4 }),
    );
    assert_request_oracle(
        "fallocate-request",
        FUSE_FALLOCATE,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Fallocate(FuseFallocateIn {
            fh: 0x2122_2324_2526_2728,
            offset: 4096,
            length: 8192,
            mode: 1,
        }),
    );
    assert_reply_oracle(
        "fallocate-empty-reply",
        FUSE_FALLOCATE,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "lseek-request",
        FUSE_LSEEK,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::Lseek(FuseLseekIn {
            fh: 0x3132_3334_3536_3738,
            offset: 123,
            whence: 3,
        }),
    );
    assert_reply_oracle(
        "lseek-reply",
        FUSE_LSEEK,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseReplyBody::Lseek(FuseLseekOut { offset: 8192 }),
    );

    assert_request_oracle(
        "batch-forget-request",
        FUSE_BATCH_FORGET,
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
        FuseRequestBody::BatchForget(FuseBatchForgetIn {
            forgets: vec![
                FuseForgetOne {
                    nodeid: 2,
                    nlookup: 3,
                },
                FuseForgetOne {
                    nodeid: 4,
                    nlookup: 5,
                },
            ],
        }),
    );
}

#[test]
fn remaining_typed_opcodes_match_pinned_mountx_oracle_fixtures() {
    let latest = ProtocolContext {
        minor: 41,
        setxattr_ext: false,
    };
    let old = ProtocolContext {
        minor: 8,
        setxattr_ext: false,
    };

    assert_request_oracle(
        "forget-request",
        FUSE_FORGET,
        latest,
        FuseRequestBody::Forget(FuseForgetIn { nlookup: 9 }),
    );
    assert_request_oracle(
        "getattr-request",
        FUSE_GETATTR,
        latest,
        FuseRequestBody::Getattr(FuseGetattrIn {
            getattr_flags: 1,
            fh: 0x0102_0304_0506_0708,
        }),
    );
    assert_request_oracle(
        "setattr-request",
        FUSE_SETATTR,
        latest,
        FuseRequestBody::Setattr(FuseSetattrIn {
            valid: 0x7ff,
            fh: 0x1112_1314_1516_1718,
            size: 0x2122_2324_2526_2728,
            lock_owner: 0x3132_3334_3536_3738,
            atime: 0x4142_4344_4546_4748,
            mtime: 0x5152_5354_5556_5758,
            ctime: 0x6162_6364_6566_6768,
            atime_nsec: 101,
            mtime_nsec: 202,
            ctime_nsec: 303,
            mode: 0o100640,
            uid: 1001,
            gid: 1002,
        }),
    );

    assert_request_oracle(
        "readlink-request",
        FUSE_READLINK,
        latest,
        FuseRequestBody::Empty,
    );
    assert_reply_oracle(
        "readlink-reply",
        FUSE_READLINK,
        latest,
        FuseReplyBody::Readlink(FuseReadlinkOut {
            target: "target/file".to_owned(),
        }),
    );
    assert_request_oracle(
        "symlink-request",
        FUSE_SYMLINK,
        latest,
        FuseRequestBody::Symlink(FuseSymlinkIn {
            name: "link".to_owned(),
            target: "target/file".to_owned(),
        }),
    );
    assert_reply_oracle(
        "symlink-reply",
        FUSE_SYMLINK,
        latest,
        FuseReplyBody::Entry(oracle_entry()),
    );

    assert_request_oracle(
        "mknod-request",
        FUSE_MKNOD,
        latest,
        FuseRequestBody::Mknod(FuseMknodIn {
            mode: 0o100640,
            rdev: 0x1234,
            umask: 0o022,
            name: "device".to_owned(),
        }),
    );
    assert_reply_oracle(
        "mknod-reply",
        FUSE_MKNOD,
        latest,
        FuseReplyBody::Entry(oracle_entry()),
    );
    assert_request_oracle(
        "mkdir-request",
        FUSE_MKDIR,
        latest,
        FuseRequestBody::Mkdir(FuseMkdirIn {
            mode: 0o40755,
            umask: 0o022,
            name: "directory".to_owned(),
        }),
    );
    assert_reply_oracle(
        "mkdir-reply",
        FUSE_MKDIR,
        latest,
        FuseReplyBody::Entry(oracle_entry()),
    );
    assert_request_oracle(
        "unlink-request",
        FUSE_UNLINK,
        latest,
        FuseRequestBody::Name(FuseNameIn {
            name: "victim".to_owned(),
        }),
    );
    assert_reply_oracle(
        "unlink-empty-reply",
        FUSE_UNLINK,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "rmdir-request",
        FUSE_RMDIR,
        latest,
        FuseRequestBody::Name(FuseNameIn {
            name: "empty".to_owned(),
        }),
    );
    assert_reply_oracle(
        "rmdir-empty-reply",
        FUSE_RMDIR,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "link-request",
        FUSE_LINK,
        latest,
        FuseRequestBody::Link(FuseLinkIn {
            oldnodeid: 3,
            name: "hard-link".to_owned(),
        }),
    );
    assert_reply_oracle(
        "link-reply",
        FUSE_LINK,
        latest,
        FuseReplyBody::Entry(oracle_entry()),
    );

    assert_request_oracle(
        "open-request",
        FUSE_OPEN,
        latest,
        FuseRequestBody::Open(FuseOpenIn {
            flags: 0x241,
            open_flags: 1,
        }),
    );
    assert_reply_oracle(
        "open-reply",
        FUSE_OPEN,
        latest,
        FuseReplyBody::Open(FuseOpenOut {
            fh: 0x8877_6655_4433_2211,
            open_flags: 0x42,
            backing_id: -7,
        }),
    );
    assert_request_oracle(
        "read-request",
        FUSE_READ,
        latest,
        FuseRequestBody::Read(FuseReadIn {
            fh: 0x0102_0304_0506_0708,
            offset: 4096,
            size: 7,
            read_flags: 2,
            lock_owner: 0x1112_1314_1516_1718,
            flags: 0x241,
        }),
    );
    assert_reply_oracle(
        "read-reply",
        FUSE_READ,
        latest,
        FuseReplyBody::Raw(vec![0, 1, 2, 3, 254, 255, 9]),
    );
    assert_reply_oracle(
        "xattr-get-reply",
        FUSE_GETXATTR,
        latest,
        FuseReplyBody::Raw(vec![5, 4, 3, 2, 1]),
    );
    assert_request_oracle(
        "xattr-list-request",
        FUSE_LISTXATTR,
        latest,
        FuseRequestBody::Listxattr(FuseListxattrIn { size: 256 }),
    );

    assert_request_oracle(
        "statfs-request",
        FUSE_STATFS,
        latest,
        FuseRequestBody::Empty,
    );
    assert_request_oracle(
        "release-request",
        FUSE_RELEASE,
        latest,
        FuseRequestBody::Release(FuseReleaseIn {
            fh: 0x0102_0304_0506_0708,
            flags: 0x241,
            release_flags: 3,
            lock_owner: 9,
        }),
    );
    assert_reply_oracle(
        "release-empty-reply",
        FUSE_RELEASE,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "fsync-request",
        FUSE_FSYNC,
        latest,
        FuseRequestBody::Fsync(FuseFsyncIn {
            fh: 0x1112_1314_1516_1718,
            fsync_flags: 1,
        }),
    );
    assert_reply_oracle(
        "fsync-empty-reply",
        FUSE_FSYNC,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "removexattr-request",
        FUSE_REMOVEXATTR,
        latest,
        FuseRequestBody::Name(FuseNameIn {
            name: "user.mountx".to_owned(),
        }),
    );
    assert_reply_oracle(
        "removexattr-empty-reply",
        FUSE_REMOVEXATTR,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "flush-request",
        FUSE_FLUSH,
        latest,
        FuseRequestBody::Flush(FuseFlushIn {
            fh: 0x2122_2324_2526_2728,
            lock_owner: 0x3132_3334_3536_3738,
        }),
    );
    assert_reply_oracle(
        "flush-empty-reply",
        FUSE_FLUSH,
        latest,
        FuseReplyBody::Empty,
    );

    assert_request_oracle(
        "opendir-request",
        FUSE_OPENDIR,
        latest,
        FuseRequestBody::Open(FuseOpenIn {
            flags: 0,
            open_flags: 0,
        }),
    );
    assert_reply_oracle(
        "opendir-reply",
        FUSE_OPENDIR,
        latest,
        FuseReplyBody::Open(FuseOpenOut {
            fh: 0x8877_6655_4433_2211,
            open_flags: 0x42,
            backing_id: -7,
        }),
    );
    assert_request_oracle(
        "readdir-request",
        FUSE_READDIR,
        latest,
        FuseRequestBody::Read(FuseReadIn {
            fh: 0x0102_0304_0506_0708,
            offset: 4096,
            size: 7,
            read_flags: 2,
            lock_owner: 0x1112_1314_1516_1718,
            flags: 0x241,
        }),
    );
    assert_reply_oracle(
        "readdir-reply",
        FUSE_READDIR,
        latest,
        FuseReplyBody::Dirents(vec![FuseDirent {
            ino: 3,
            off: 4,
            type_: DT_REG,
            name: "child".to_owned(),
        }]),
    );
    assert_request_oracle(
        "readdirplus-request",
        FUSE_READDIRPLUS,
        latest,
        FuseRequestBody::Read(FuseReadIn {
            fh: 0x0102_0304_0506_0708,
            offset: 4096,
            size: 7,
            read_flags: 2,
            lock_owner: 0x1112_1314_1516_1718,
            flags: 0x241,
        }),
    );
    assert_reply_oracle(
        "readdirplus-reply",
        FUSE_READDIRPLUS,
        latest,
        FuseReplyBody::DirentsPlus(vec![FuseDirentPlus {
            entry: oracle_entry(),
            dirent: FuseDirent {
                ino: 3,
                off: 4,
                type_: DT_REG,
                name: "child".to_owned(),
            },
        }]),
    );
    assert_request_oracle(
        "releasedir-request",
        FUSE_RELEASEDIR,
        latest,
        FuseRequestBody::Release(FuseReleaseIn {
            fh: 0x4142_4344_4546_4748,
            flags: 0,
            release_flags: 0,
            lock_owner: 0,
        }),
    );
    assert_reply_oracle(
        "releasedir-empty-reply",
        FUSE_RELEASEDIR,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "fsyncdir-request",
        FUSE_FSYNCDIR,
        latest,
        FuseRequestBody::Fsync(FuseFsyncIn {
            fh: 0x5152_5354_5556_5758,
            fsync_flags: 0,
        }),
    );
    assert_reply_oracle(
        "fsyncdir-empty-reply",
        FUSE_FSYNCDIR,
        latest,
        FuseReplyBody::Empty,
    );

    assert_request_oracle(
        "setlkw-request",
        FUSE_SETLKW,
        latest,
        FuseRequestBody::Lk(FuseLkIn {
            fh: 0x2122_2324_2526_2728,
            owner: 0x3132_3334_3536_3738,
            lk: oracle_lock(1, 4242),
            lk_flags: 1,
        }),
    );
    assert_reply_oracle(
        "setlkw-empty-reply",
        FUSE_SETLKW,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "access-request",
        FUSE_ACCESS,
        latest,
        FuseRequestBody::Access(FuseAccessIn { mask: 7 }),
    );
    assert_reply_oracle(
        "access-empty-reply",
        FUSE_ACCESS,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "interrupt-request",
        FUSE_INTERRUPT,
        latest,
        FuseRequestBody::Interrupt(FuseInterruptIn {
            unique: 0x6162_6364_6566_6768,
        }),
    );
    assert_reply_oracle(
        "interrupt-empty-reply",
        FUSE_INTERRUPT,
        latest,
        FuseReplyBody::Empty,
    );
    assert_request_oracle(
        "bmap-request",
        FUSE_BMAP,
        latest,
        FuseRequestBody::Bmap(FuseBmapIn {
            block: 17,
            blocksize: 4096,
        }),
    );
    assert_reply_oracle(
        "bmap-reply",
        FUSE_BMAP,
        latest,
        FuseReplyBody::Bmap(FuseBmapOut { block: 33 }),
    );
    assert_request_oracle(
        "destroy-request",
        FUSE_DESTROY,
        latest,
        FuseRequestBody::Empty,
    );
    assert_reply_oracle(
        "destroy-empty-reply",
        FUSE_DESTROY,
        latest,
        FuseReplyBody::Empty,
    );

    assert_request_oracle(
        "mknod-request-old",
        FUSE_MKNOD,
        old,
        FuseRequestBody::Mknod(FuseMknodIn {
            mode: 0o100640,
            rdev: 0x1234,
            umask: 0,
            name: "device-old".to_owned(),
        }),
    );
    assert_request_oracle(
        "create-request-old",
        FUSE_CREATE,
        old,
        FuseRequestBody::Create(FuseCreateIn {
            flags: 0x241,
            mode: 0o100640,
            umask: 0,
            open_flags: 0,
            name: "created-old".to_owned(),
        }),
    );
    assert_reply_oracle(
        "init-out-old",
        FUSE_INIT,
        ProtocolContext {
            minor: 3,
            setxattr_ext: false,
        },
        FuseReplyBody::Init(FuseInitOut {
            major: 7,
            minor: 3,
            max_readahead: 0,
            flags: 0,
            max_background: 0,
            congestion_threshold: 0,
            max_write: 0,
            time_gran: 0,
            max_pages: 0,
            map_alignment: 0,
            flags2: 0,
            max_stack_depth: 0,
        }),
    );
    assert_request_oracle(
        "read-request-old",
        FUSE_READ,
        old,
        FuseRequestBody::Read(FuseReadIn {
            fh: 0x0102_0304_0506_0708,
            offset: 4096,
            size: 7,
            read_flags: 2,
            lock_owner: 0,
            flags: 0,
        }),
    );
}

#[test]
fn malformed_inputs_return_protocol_errors_without_unbounded_allocations() {
    let statfs = FuseKstatfs {
        blocks: 1,
        bfree: 2,
        bavail: 3,
        files: 4,
        ffree: 5,
        bsize: 4096,
        namelen: 255,
        frsize: 4096,
    };
    let full = encode_statfs_out(&statfs, None);
    for length in 0..full.len() {
        assert!(
            decode_statfs_out(&full[..length], None).is_err(),
            "length {length}"
        );
    }

    assert!(decode_request_body(FUSE_FORGET, &[0; 9], None).is_err());
    assert!(decode_request_body(FUSE_LOOKUP, b"unterminated", None).is_err());

    let mut huge_count = [0u8; 8];
    huge_count[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_request_body(FUSE_BATCH_FORGET, &huge_count, None).is_err());

    let batch = [0u8; 8];
    assert!(matches!(
        decode_request_body(FUSE_BATCH_FORGET, &batch, None),
        Ok(FuseRequestBody::BatchForget(value)) if value.forgets.is_empty()
    ));
    for length in 0..batch.len() {
        assert!(
            decode_request_body(FUSE_BATCH_FORGET, &batch[..length], None).is_err(),
            "batch forget length {length}"
        );
    }
    let mut batch_with_trailing = batch.to_vec();
    batch_with_trailing.extend([0; 16]);
    assert!(decode_request_body(FUSE_BATCH_FORGET, &batch_with_trailing, None).is_err());

    let interrupt = 0x6162_6364_6566_6768u64.to_le_bytes();
    assert!(matches!(
        decode_request_body(FUSE_INTERRUPT, &interrupt, None),
        Ok(FuseRequestBody::Interrupt(FuseInterruptIn { unique }))
            if unique == 0x6162_6364_6566_6768
    ));
    for length in 0..interrupt.len() {
        assert!(
            decode_request_body(FUSE_INTERRUPT, &interrupt[..length], None).is_err(),
            "interrupt length {length}"
        );
    }
    let mut interrupt_with_trailing = interrupt.to_vec();
    interrupt_with_trailing.push(0);
    assert!(decode_request_body(FUSE_INTERRUPT, &interrupt_with_trailing, None).is_err());

    let mut short_write = vec![0u8; 42];
    short_write[16..20].copy_from_slice(&1000u32.to_le_bytes());
    assert!(decode_request_body(FUSE_WRITE, &short_write, None).is_err());

    let short_setxattr = [100, 0, 0, 0, 0, 0, 0, 0, b'a', 0, 1];
    assert!(decode_request_body(FUSE_SETXATTR, &short_setxattr, None).is_err());

    for opcode in UNIMPLEMENTED_OPCODES {
        assert!(decode_request_body(*opcode, &[], None).is_err());
        assert!(decode_reply_body(*opcode, &[], None).is_err());
    }
}

#[test]
fn readlink_and_statfs_wire_edges_match_empty_and_legacy_layouts() {
    let contexts = [
        ProtocolContext {
            minor: 3,
            setxattr_ext: false,
        },
        ProtocolContext {
            minor: 41,
            setxattr_ext: false,
        },
    ];

    for context in contexts {
        for opcode in [FUSE_READLINK, FUSE_STATFS] {
            let request = encode_request(
                &EncodeRequest {
                    opcode,
                    unique: 7,
                    nodeid: FUSE_ROOT_ID,
                    body: Some(FuseRequestBody::Empty),
                    ..EncodeRequest::default()
                },
                Some(context),
            )
            .unwrap();
            assert_eq!(request.len(), mount_rs_fuse::IN_HEADER_SIZE);
            assert_eq!(
                decode_request(&request, Some(context)).unwrap().body,
                Some(FuseRequestBody::Empty)
            );
            assert_eq!(
                decode_request_body(opcode, &[], Some(context)).unwrap(),
                FuseRequestBody::Empty
            );
            assert!(decode_request_body(opcode, &[0], Some(context)).is_err());
        }
    }

    for target in ["", "target/λ"] {
        let body = FuseReplyBody::Readlink(FuseReadlinkOut {
            target: target.to_owned(),
        });
        for context in contexts {
            let wire = encode_reply_body(FUSE_READLINK, &body, Some(context)).unwrap();
            assert_eq!(wire, target.as_bytes());
            assert_eq!(
                decode_reply_body(FUSE_READLINK, &wire, Some(context)).unwrap(),
                body
            );
        }
    }
    assert!(
        encode_reply_body(
            FUSE_READLINK,
            &FuseReplyBody::Readlink(FuseReadlinkOut {
                target: "target\0tail".to_owned(),
            }),
            None,
        )
        .is_err()
    );

    let statfs = FuseKstatfs {
        blocks: 0x0102_0304_0506_0708,
        bfree: 0x1112_1314_1516_1718,
        bavail: 0x2122_2324_2526_2728,
        files: 0x3132_3334_3536_3738,
        ffree: 0x4142_4344_4546_4748,
        bsize: 4096,
        namelen: 255,
        frsize: 8192,
    };
    for context in contexts {
        let wire = encode_reply_body(
            FUSE_STATFS,
            &FuseReplyBody::Statfs(statfs.clone()),
            Some(context),
        )
        .unwrap();
        assert_eq!(wire.len(), kstatfs_size(context.minor));
        let expected = FuseKstatfs {
            frsize: if context.minor >= 4 { statfs.frsize } else { 0 },
            ..statfs.clone()
        };
        assert_eq!(
            decode_reply_body(FUSE_STATFS, &wire, Some(context)).unwrap(),
            FuseReplyBody::Statfs(expected)
        );
        for length in 0..wire.len() {
            assert!(
                decode_reply_body(FUSE_STATFS, &wire[..length], Some(context)).is_err(),
                "truncated STATFS body at {length} bytes for minor {}",
                context.minor
            );
        }
        let mut trailing = wire;
        trailing.push(0);
        assert!(decode_reply_body(FUSE_STATFS, &trailing, Some(context)).is_err());
    }

    for context in contexts {
        let request = FuseRequestBody::Syncfs(FuseSyncfsIn {
            padding: 0x0102_0304_0506_0708,
        });
        let wire = encode_request_body(FUSE_SYNCFS, &request, Some(context)).unwrap();
        assert_eq!(wire, 0x0102_0304_0506_0708_u64.to_le_bytes());
        assert_eq!(
            decode_request_body(FUSE_SYNCFS, &wire, Some(context)).unwrap(),
            request
        );
        for length in 0..wire.len() {
            assert!(decode_request_body(FUSE_SYNCFS, &wire[..length], Some(context)).is_err());
        }
        let mut trailing = wire.clone();
        trailing.push(0);
        assert!(decode_request_body(FUSE_SYNCFS, &trailing, Some(context)).is_err());
        assert_eq!(
            decode_reply_body(FUSE_SYNCFS, &[], Some(context)).unwrap(),
            FuseReplyBody::Empty
        );
        assert_eq!(
            encode_reply_body(FUSE_SYNCFS, &FuseReplyBody::Empty, Some(context)).unwrap(),
            Vec::<u8>::new()
        );
    }
}

#[test]
fn poll_wire_edges_reject_truncation_and_trailing_bytes() {
    let request = FuseRequestBody::Poll(FusePollIn {
        fh: 0x0102_0304_0506_0708,
        kh: 0x1112_1314_1516_1718,
        flags: FUSE_POLL_SCHEDULE_NOTIFY,
        events: 0x2122_2324,
    });
    let request_wire = encode_request_body(FUSE_POLL, &request, None).unwrap();
    assert_eq!(request_wire.len(), 24);
    assert_eq!(
        decode_request_body(FUSE_POLL, &request_wire, None).unwrap(),
        request
    );
    for length in 0..request_wire.len() {
        assert!(
            decode_request_body(FUSE_POLL, &request_wire[..length], None).is_err(),
            "truncated POLL request at {length} bytes"
        );
    }
    let mut request_trailing = request_wire;
    request_trailing.push(0);
    assert!(decode_request_body(FUSE_POLL, &request_trailing, None).is_err());

    let reply = FuseReplyBody::Poll(FusePollOut {
        revents: 0x3132_3334,
    });
    let reply_wire = encode_reply_body(FUSE_POLL, &reply, None).unwrap();
    assert_eq!(reply_wire.len(), 8);
    assert_eq!(
        decode_reply_body(FUSE_POLL, &reply_wire, None).unwrap(),
        reply
    );
    for length in 0..reply_wire.len() {
        assert!(
            decode_reply_body(FUSE_POLL, &reply_wire[..length], None).is_err(),
            "truncated POLL reply at {length} bytes"
        );
    }
    let mut reply_trailing = reply_wire;
    reply_trailing.push(0);
    assert!(decode_reply_body(FUSE_POLL, &reply_trailing, None).is_err());
}

#[test]
fn framing_extensions_and_compatibility_layouts_are_bounded() {
    let request = encode_request(
        &EncodeRequest {
            opcode: FUSE_LOOKUP,
            unique: 7,
            nodeid: 1,
            body: Some(FuseRequestBody::Name(FuseNameIn {
                name: "x".to_owned(),
            })),
            extensions: (1u8..=16).collect(),
            ..EncodeRequest::default()
        },
        None,
    )
    .unwrap();
    let decoded = decode_request(&request, None).unwrap();
    assert_eq!(decoded.payload, vec![b'x', 0]);
    assert_eq!(decoded.extensions, (1u8..=16).collect::<Vec<_>>());

    let mut claimed_long = request.clone();
    claimed_long[36..38].copy_from_slice(&8u16.to_le_bytes());
    assert!(decode_request(&claimed_long, None).is_err());

    let mut claimed_short = request.clone();
    claimed_short[0..4].copy_from_slice(&8u32.to_le_bytes());
    assert!(decode_request(&claimed_short, None).is_err());

    assert_eq!(attr_size(8), 80);
    assert_eq!(entry_out_size(8), FUSE_COMPAT_ENTRY_OUT_SIZE);
    assert_eq!(attr_out_size(8), FUSE_COMPAT_ATTR_OUT_SIZE);
    assert_eq!(kstatfs_size(3), FUSE_COMPAT_STATFS_SIZE);
    assert_eq!(read_write_in_size(8), FUSE_COMPAT_WRITE_IN_SIZE);
    assert_eq!(init_out_size(4), FUSE_COMPAT_INIT_OUT_SIZE);
    assert_eq!(init_out_size(22), FUSE_COMPAT_22_INIT_OUT_SIZE);

    let old = ProtocolContext {
        minor: 8,
        setxattr_ext: false,
    };
    let read = FuseReadIn {
        fh: 7,
        offset: 4096,
        size: 128,
        read_flags: FUSE_READ_LOCKOWNER,
        lock_owner: 9,
        flags: 2,
    };
    let read_wire =
        encode_request_body(FUSE_READ, &FuseRequestBody::Read(read), Some(old)).unwrap();
    assert_eq!(read_wire.len(), FUSE_COMPAT_WRITE_IN_SIZE);
    assert_eq!(
        decode_request_body(FUSE_READ, &read_wire, Some(old)).unwrap(),
        FuseRequestBody::Read(FuseReadIn {
            lock_owner: 0,
            flags: 0,
            ..read
        })
    );
}

#[test]
fn opcode_names_and_utf8_lengths_match_the_public_surface() {
    assert_eq!(opcode_name(FUSE_LOOKUP), "LOOKUP");
    assert_eq!(opcode_name(9999), "UNKNOWN(9999)");
    assert!(SUPPORTED_OPCODES.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(name_byte_length("abc"), 3);
    assert_eq!(name_byte_length("é"), 2);
    assert_eq!(name_byte_length("🙂"), 4);
    assert!(
        encode_request_body(
            FUSE_LOOKUP,
            &FuseRequestBody::Name(FuseNameIn {
                name: "a\0b".to_owned()
            }),
            None,
        )
        .is_err()
    );
}
