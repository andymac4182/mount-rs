//! What the filesystem's one exclusion is for, checked from several
//! threads. Every call is synchronous inside it and calls nothing a test
//! could park, so these are stress runs.
use mount_rs_core::FsDriver;
use mount_rs_memfs::MemoryFs;
use std::future::Future;
use std::sync::Barrier;
use std::task::{Context, Poll, Waker};

fn run<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("MemoryFs operation unexpectedly required an async runtime"),
    }
}

/// A write at the handle's cursor loads the cursor, writes there and
/// advances it as one step, so two of them never land on the same byte.
#[test]
fn cursor_writes_racing_on_one_handle_each_land_at_their_own_offset() {
    const THREADS: usize = 4;
    const WRITES: usize = 2000;
    let fs = MemoryFs::new(Default::default());
    // Already full length, so the writes only fill it in and a missing
    // one shows as a byte left at zero.
    run(fs.write_file("/f", &vec![0; THREADS * WRITES])).unwrap();
    let handle = run(fs.open("/f", "r+", 0o644)).unwrap();
    let start = Barrier::new(THREADS);
    std::thread::scope(|threads| {
        for thread in 0..THREADS {
            let (handle, start) = (&handle, &start);
            threads.spawn(move || {
                start.wait();
                for _ in 0..WRITES {
                    run(handle.write(&[thread as u8 + 1], None)).unwrap();
                }
            });
        }
    });
    let mut stored = vec![0xff; THREADS * WRITES + 1];
    let length = run(handle.read(&mut stored, Some(0))).unwrap();
    run(handle.close()).unwrap();
    assert_eq!(length, THREADS * WRITES);
    stored.truncate(length);
    for thread in 0..THREADS {
        let landed = stored
            .iter()
            .filter(|byte| **byte == thread as u8 + 1)
            .count();
        assert_eq!(landed, WRITES, "thread {thread} lost cursor writes");
    }
}
