use mount_rs_fuse::init::{KernelInit, Negotiation, Preferences, negotiate};

fn main() {
    for major in [6, 7, 8] {
        for minor in [4, 9, 13, 23, 28, 35, 36, 40, 41, 99] {
            for flags in [0, u32::MAX] {
                for max_write in [0, 4095, 131072, 1048576, u32::MAX] {
                    let kernel = KernelInit {
                        major,
                        minor,
                        flags,
                        flags2: u32::MAX,
                        max_readahead: 65536,
                    };
                    let preferences = Preferences {
                        max_write,
                        ..Preferences::default()
                    };
                    let (status, reply) = match negotiate(kernel, &preferences) {
                        Negotiation::UnsupportedMajor => {
                            println!("error");
                            continue;
                        }
                        Negotiation::Ready(reply) => ("ok", reply),
                        Negotiation::Retry(reply) => ("retry", reply),
                    };
                    println!(
                        "{status} {} {} {} {} {} {} {} {} {} {}",
                        reply.major,
                        reply.minor,
                        reply.max_readahead,
                        reply.flags,
                        reply.max_background,
                        reply.congestion_threshold,
                        reply.max_write,
                        reply.time_gran,
                        reply.max_pages,
                        reply.max_stack_depth
                    );
                }
            }
        }
    }
}
