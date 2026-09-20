import Foundation
import FSKit

/// Compile-only FSKit delegate.
///
/// This target proves that the current FSKit V1 callback surface is available
/// at the selected deployment target. It deliberately performs no mounting or
/// Rust/IPC work.
@objc
final class MountRsFileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations {
    func probeResource(
        resource: FSResource,
        replyHandler: @escaping (FSProbeResult?, (any Error)?) -> Void
    ) {
        replyHandler(.notRecognized, nil)
    }

    func loadResource(
        resource: FSResource,
        options: FSTaskOptions,
        replyHandler: @escaping (FSVolume?, (any Error)?) -> Void
    ) {
        let error = NSError(
            domain: "com.andymac4182.mount-rs.fskit.compile-only",
            code: 1,
            userInfo: [
                NSLocalizedDescriptionKey:
                    "mount-rs FSKit target is compile-only; no volume implementation is installed"
            ]
        )
        replyHandler(nil, error)
    }

    func unloadResource(resource: FSResource, options: FSTaskOptions) async throws {
        // The V1 API requires an unload callback. There is no worker or volume
        // to stop in this compile-only target.
    }
}
