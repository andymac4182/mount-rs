import FSKit

/// The FSKit-side identity for a Rust-driver path.
///
/// FSKit owns the lifetime of this object. The Rust worker owns the provider
/// state; keeping the normalized path here gives each callback a stable key
/// without duplicating inode or data state in Swift.
final class MountRsFSItem: FSItem {
    var path: String
    let itemID: UInt64
    let mode: UInt32
    var handle: UInt64?
    var openModes: FSVolume.OpenModes = []

    init(path: String, stats: MountRsWorkerStats) {
        self.path = path
        self.itemID = stats.ino
        self.mode = stats.mode
        self.handle = nil
        super.init()
    }
}
