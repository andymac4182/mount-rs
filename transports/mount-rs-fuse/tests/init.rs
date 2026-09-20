use mount_rs_fuse::constants::{
    FUSE_BIG_WRITES, FUSE_HAS_EXPIRE_ONLY, FUSE_INIT_EXT, FUSE_KERNEL_VERSION, FUSE_MAX_MAX_PAGES,
    FUSE_MAX_PAGES, FUSE_PAGE_SIZE,
};
use mount_rs_fuse::init::{
    DEFAULT_MAX_WRITE, InitReply, KernelInit, Negotiation, Preferences, negotiate,
};
use mount_rs_fuse::protocol::{
    FuseInitIn, FuseInitOut, decode_init_in, decode_init_out, encode_init_in, init_out_size,
};

fn kernel(flags: u64, minor: u32) -> KernelInit {
    let (flags, flags2) = mount_rs_fuse::init::split_init_flags(flags);
    KernelInit {
        major: FUSE_KERNEL_VERSION,
        minor,
        max_readahead: 131_072,
        flags,
        flags2,
    }
}

fn ready(kernel: KernelInit, preferences: &Preferences) -> InitReply {
    let Negotiation::Ready(reply) = negotiate(kernel, preferences) else {
        panic!("expected a ready FUSE INIT negotiation");
    };
    reply
}

#[test]
fn version_downgrade_and_major_retry_follow_the_wire_rules() {
    let preferences = Preferences {
        minor: 40,
        ..Preferences::default()
    };

    let downgraded = ready(kernel(0, 99), &preferences);
    assert_eq!(downgraded.major, FUSE_KERNEL_VERSION);
    assert_eq!(downgraded.minor, 40);

    let retry = KernelInit {
        major: FUSE_KERNEL_VERSION + 1,
        minor: 99,
        ..kernel(0, 99)
    };
    let Negotiation::Retry(reply) = negotiate(retry, &preferences) else {
        panic!("a newer kernel major must trigger a retry");
    };
    assert_eq!(reply.major, FUSE_KERNEL_VERSION);
    assert_eq!(reply.minor, 40);
    assert_eq!(reply.flags, 0);
    assert_eq!(reply.max_write, 0);
    assert_eq!(reply.encode().len(), init_out_size(40));
    assert_eq!(decode_init_out(&reply.encode()).unwrap(), reply.as_wire());

    let unsupported = KernelInit {
        major: FUSE_KERNEL_VERSION - 1,
        ..kernel(0, 99)
    };
    assert!(matches!(
        negotiate(unsupported, &preferences),
        Negotiation::UnsupportedMajor
    ));
}

#[test]
fn flag_words_only_use_flags2_when_the_extension_is_negotiated() {
    let high = FUSE_HAS_EXPIRE_ONLY;
    let offered = FUSE_INIT_EXT | high;
    let reply = ready(
        kernel(offered, 41),
        &Preferences {
            flags: high,
            ..Preferences::default()
        },
    );
    let wire = reply.as_wire();
    assert_eq!(reply.flags, offered);
    assert_eq!(wire.flags, FUSE_INIT_EXT as u32);
    assert_eq!(wire.flags2, 1 << 3);
    assert_eq!(InitReply::from_wire(wire), reply);

    let no_extension = ready(
        kernel(high, 41),
        &Preferences {
            flags: high,
            ..Preferences::default()
        },
    );
    assert_eq!(no_extension.flags, 0);
    assert_eq!(no_extension.as_wire().flags2, 0);

    let old_minor = ready(
        kernel(offered, 35),
        &Preferences {
            flags: high,
            ..Preferences::default()
        },
    );
    assert_eq!(old_minor.flags, 0);

    let manually_constructed = InitReply {
        major: FUSE_KERNEL_VERSION,
        minor: 35,
        max_readahead: 0,
        flags: offered,
        max_background: 0,
        congestion_threshold: 0,
        max_write: 0,
        time_gran: 0,
        max_pages: 0,
        max_stack_depth: 0,
    };
    let normalized = manually_constructed.as_wire();
    assert_eq!(normalized.flags, 0);
    assert_eq!(normalized.flags2, 0);
    let normalized_reply = InitReply::from_wire(FuseInitOut {
        major: normalized.major,
        minor: normalized.minor,
        max_readahead: normalized.max_readahead,
        flags: normalized.flags,
        max_background: normalized.max_background,
        congestion_threshold: normalized.congestion_threshold,
        max_write: normalized.max_write,
        time_gran: normalized.time_gran,
        max_pages: normalized.max_pages,
        map_alignment: normalized.map_alignment,
        flags2: normalized.flags2,
        max_stack_depth: normalized.max_stack_depth,
    });
    assert_eq!(normalized_reply.flags, 0);
}

#[test]
fn from_wire_discards_unnegotiated_extension_words() {
    let old = InitReply::from_wire(FuseInitOut {
        major: FUSE_KERNEL_VERSION,
        minor: 35,
        max_readahead: 0,
        flags: FUSE_INIT_EXT as u32,
        max_background: 0,
        congestion_threshold: 0,
        max_write: 0,
        time_gran: 0,
        max_pages: 0,
        map_alignment: 0,
        flags2: u32::MAX,
        max_stack_depth: 0,
    });
    assert_eq!(old.flags, 0);

    let missing_marker = InitReply::from_wire(FuseInitOut {
        major: FUSE_KERNEL_VERSION,
        minor: 41,
        max_readahead: 0,
        flags: 0,
        max_background: 0,
        congestion_threshold: 0,
        max_write: 0,
        time_gran: 0,
        max_pages: 0,
        map_alignment: 0,
        flags2: u32::MAX,
        max_stack_depth: 0,
    });
    assert_eq!(missing_marker.flags, 0);
}

#[test]
fn max_pages_and_max_write_are_clamped_at_the_negotiated_limits() {
    let preferences = |max_write| Preferences {
        flags: FUSE_MAX_PAGES,
        max_write,
        ..Preferences::default()
    };
    let cases = [
        (0, 1, 4096),
        (4095, 1, 4096),
        (4096, 1, 4096),
        (4097, 2, 4097),
        (131_072, 32, 131_072),
        (1_048_576, FUSE_MAX_MAX_PAGES, 1_048_576),
        (u32::MAX, FUSE_MAX_MAX_PAGES, 1_048_576),
    ];
    for (requested, max_pages, max_write) in cases {
        let reply = ready(kernel(FUSE_MAX_PAGES, 41), &preferences(requested));
        assert_eq!(
            reply.max_pages, max_pages,
            "requested max_write={requested}"
        );
        assert_eq!(
            reply.max_write, max_write,
            "requested max_write={requested}"
        );
    }

    let no_max_pages = ready(
        kernel(FUSE_BIG_WRITES, 41),
        &Preferences {
            flags: FUSE_BIG_WRITES,
            max_write: DEFAULT_MAX_WRITE,
            ..Preferences::default()
        },
    );
    assert_eq!(no_max_pages.max_pages, 0);
    assert_eq!(no_max_pages.max_write, 32 * FUSE_PAGE_SIZE as u32);
    assert_eq!(no_max_pages.flags & FUSE_MAX_PAGES, 0);

    let pre_max_pages = ready(
        kernel(FUSE_MAX_PAGES, 27),
        &Preferences {
            flags: FUSE_MAX_PAGES,
            max_write: DEFAULT_MAX_WRITE,
            ..Preferences::default()
        },
    );
    assert_eq!(pre_max_pages.max_pages, 0);
    assert_eq!(pre_max_pages.max_write, 32 * FUSE_PAGE_SIZE as u32);
    assert_eq!(pre_max_pages.flags & FUSE_MAX_PAGES, 0);
}

#[test]
fn older_minor_replies_use_the_compatibility_layouts() {
    for (minor, expected_len) in [(0, 8), (4, 8), (5, 24), (22, 24), (23, 64)] {
        let reply = ready(kernel(0, minor), &Preferences::default());
        assert_eq!(init_out_size(minor), expected_len);
        assert_eq!(reply.encode().len(), expected_len);
        let decoded = decode_init_out(&reply.encode()).unwrap();
        assert_eq!((decoded.major, decoded.minor), (reply.major, reply.minor));
        if minor < 5 {
            assert_eq!(decoded.max_readahead, 0);
            assert_eq!(decoded.max_write, 0);
        } else {
            let wire = reply.as_wire();
            assert_eq!(decoded.max_readahead, wire.max_readahead);
            assert_eq!(decoded.flags, wire.flags);
            assert_eq!(decoded.max_background, wire.max_background);
            assert_eq!(decoded.congestion_threshold, wire.congestion_threshold);
            assert_eq!(decoded.max_write, wire.max_write);
        }
    }

    for minor in [27, 28, 35, 36, 40, 41] {
        let reply = ready(kernel(FUSE_MAX_PAGES, minor), &Preferences::default());
        assert_eq!(reply.encode().len(), 64, "minor={minor}");
    }
}

#[test]
fn init_wire_decoders_reject_truncated_fixed_headers_without_panicking() {
    for len in 0..8 {
        let bytes = vec![0; len];
        assert!(decode_init_in(&bytes).is_err(), "init request len={len}");
        assert!(decode_init_out(&bytes).is_err(), "init reply len={len}");
    }

    let full = encode_init_in(&FuseInitIn {
        major: 7,
        minor: 41,
        max_readahead: 131_072,
        flags: 0x1234,
        flags2: 0x5678,
    });
    assert_eq!(full.len(), 64);
    assert_eq!(decode_init_in(&full[..8]).unwrap().major, 7);
    assert_eq!(decode_init_in(&full[..16]).unwrap().flags, 0x1234);
    assert_eq!(decode_init_in(&full[..20]).unwrap().flags2, 0x5678);
}
