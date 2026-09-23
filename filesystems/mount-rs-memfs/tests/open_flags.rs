use std::future::Future;
use std::task::{Context, Poll, Waker};

use mount_rs_core::{ErrorCode, Loopback, OpenFlags};
use mount_rs_memfs::MemoryFs;

fn run<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("MemoryFs operation unexpectedly required an async runtime"),
    }
}

fn flags(mask: u8) -> OpenFlags {
    OpenFlags {
        read: mask & 1 != 0,
        write: mask & 2 != 0,
        create: mask & 4 != 0,
        truncate: mask & 8 != 0,
        append: mask & 16 != 0,
        exclusive: mask & 32 != 0,
    }
}

#[test]
fn every_decoded_flag_combination_checks_truncation_before_mutating() {
    // Each mask starts with a fresh existing file, so one open cannot mask a
    // later combination's effect on its bytes.
    for mask in 0..64_u8 {
        let fs = Loopback::new(MemoryFs::empty());
        run(async {
            fs.write_file("/file", b"keep").await.expect("seed file");
            let decoded = flags(mask);
            let result = fs.open_flags("/file", decoded, 0).await;
            if decoded.truncate && !decoded.write {
                let error = match result {
                    Ok(_) => panic!("mask {mask}: truncation without write succeeded"),
                    Err(error) => error,
                };
                assert_eq!(error.code, ErrorCode::Einval, "mask {mask}");
            } else if decoded.exclusive {
                let error = match result {
                    Ok(_) => panic!("mask {mask}: exclusive existing open succeeded"),
                    Err(error) => error,
                };
                assert_eq!(error.code, ErrorCode::Eexist, "mask {mask}");
            } else {
                result
                    .expect("allowed decoded open")
                    .close()
                    .await
                    .expect("close");
            }
            let expected: &[u8] = if decoded.truncate && decoded.write && !decoded.exclusive {
                b""
            } else {
                b"keep"
            };
            assert_eq!(
                fs.read_file("/file").await.expect("readback"),
                expected,
                "mask {mask}"
            );
        });
    }
}
