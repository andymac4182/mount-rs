//! Not a test: what the filesystem's one exclusion costs under contention.
//! `cargo test -p mount-rs-memfs --release --test profile -- --ignored --nocapture`
use mount_rs_core::FsDriver;
use mount_rs_memfs::MemoryFs;
use std::future::Future;
use std::task::{Context, Poll, Waker};
use std::time::Instant;

fn run<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("MemoryFs operation unexpectedly required an async runtime"),
    }
}

#[test]
#[ignore]
fn profile_contended_reads_and_stats() {
    let fs = MemoryFs::new(Default::default());
    for file in 0..64 {
        run(fs.write_file(&format!("/f{file}"), &[7_u8; 65536])).unwrap();
    }
    for (name, write_every) in [("read+stat", 0_usize), ("with writes", 8)] {
        let started = Instant::now();
        let mut waits: Vec<u64> = std::thread::scope(|threads| {
            let handles: Vec<_> = (0..8_usize)
                .map(|thread| {
                    let fs = &fs;
                    threads.spawn(move || {
                        let mut waits = Vec::with_capacity(100_000);
                        let mut buffer = vec![0_u8; 4096];
                        for n in 0..50_000_usize {
                            let path = format!("/f{}", (n * 31 + thread * 17) % 64);
                            let began = Instant::now();
                            let handle = run(fs.open(&path, "r+", 0o644)).unwrap();
                            if write_every != 0 && n % write_every == 0 {
                                run(handle.write(&buffer, Some(4096))).unwrap();
                            } else {
                                run(handle.read(&mut buffer, Some(8192))).unwrap();
                            }
                            run(handle.close()).unwrap();
                            waits.push(began.elapsed().as_nanos() as u64);
                            let began = Instant::now();
                            run(fs.stat(&path)).unwrap();
                            waits.push(began.elapsed().as_nanos() as u64);
                        }
                        waits
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect()
        });
        let seconds = started.elapsed().as_secs_f64();
        waits.sort_unstable();
        let at = |q: f64| waits[((waits.len() - 1) as f64 * q) as usize] as f64 / 1e3;
        println!(
            "{name}: {:.0} ops/s p50={:.2} p99={:.2} p99.9={:.2} max={:.0} us",
            waits.len() as f64 / seconds,
            at(0.5),
            at(0.99),
            at(0.999),
            at(1.0)
        );
    }
}
