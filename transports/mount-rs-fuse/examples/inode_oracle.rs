use mount_rs_core::Stats;
use mount_rs_fuse::inodes::InodeTable;
fn stat(ino: u64) -> Stats {
    Stats {
        dev: 0,
        ino,
        mode: 0o100644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 0,
        blksize: 4096,
        blocks: 0,
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    }
}
fn main() {
    let mut t = InodeTable::default();
    let a = t.bind("/old/file", &stat(2));
    t.acquire(a).unwrap();
    let hard = t.bind("/hard", &stat(2));
    let victim = t.bind("/new/file", &stat(3));
    t.remap("/old", "/new");
    println!(
        "{} {} {} {}",
        a,
        hard,
        t.require_path(a).unwrap(),
        t.require_path(victim).unwrap_err().code.as_str()
    );
    t.unbind("/hard");
    println!("{}", t.require_path(a).unwrap());
    t.unbind("/new/file");
    let reused = t.bind("/reused", &stat(2));
    println!("{} {}", reused, t.forget(a, 1));
    println!("{} {}", t.bind("/alias", &stat(2)), t.forget(1, u64::MAX));
}
