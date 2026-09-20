import Foundation
import FSKit

@objc
final class MountRsFileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations {
    func probeResource(
        resource: FSResource,
        replyHandler: @escaping (FSProbeResult?, (any Error)?) -> Void
    ) {
        guard !resource.isRevoked else {
            replyHandler(.notRecognized, nil)
            return
        }
        replyHandler(
            .usable(name: "mount-rs", containerID: FSContainerIdentifier()),
            nil
        )
    }

    func loadResource(
        resource: FSResource,
        options: FSTaskOptions,
        replyHandler: @escaping (FSVolume?, (any Error)?) -> Void
    ) {
        let readOnly = options.taskOptions.contains { option in
            option == "-r" || option == "--rdonly" || option == "ro" || option == "read-only" || option == "readonly"
        }
        Task {
            do {
                let client = try MountRsWorkerClient(xpcService: MountRsXPCServiceName)
                let volume = MountRsFSVolume(
                    worker: client,
                    volumeName: "mount-rs",
                    readOnly: readOnly
                )
                replyHandler(volume, nil)
            } catch {
                replyHandler(nil, error)
            }
        }
    }

    func unloadResource(resource: FSResource, options: FSTaskOptions) async throws {
        // The volume's deactivate callback owns worker shutdown. FSKit calls
        // this hook after the resource has been unloaded, so there is no
        // second worker lifetime to terminate here.
    }
}
