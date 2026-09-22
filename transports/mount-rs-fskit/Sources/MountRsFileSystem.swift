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
        guard #available(macOS 26.0, *),
              let pathResource = resource as? FSPathURLResource,
              pathResource.url.isFileURL,
              !pathResource.url.path.isEmpty
        else {
            replyHandler(nil, NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(22),
                userInfo: [NSLocalizedDescriptionKey: "mount-rs FSKit requires a file path resource"]
            ))
            return
        }

        let resourceURL = pathResource.url.standardizedFileURL
        let effectiveReadOnly = readOnly || !pathResource.isWritable
        let didStartAccessing = resourceURL.startAccessingSecurityScopedResource()
        let configuration = MountRsWorkerConfiguration(
            backend: .host(root: resourceURL.path),
            readOnly: effectiveReadOnly
        )
        do {
            let client = try MountRsWorkerClient(configuration: configuration)
            let volume = MountRsFSVolume(
                worker: client,
                volumeName: "mount-rs",
                readOnly: effectiveReadOnly,
                resourceURL: didStartAccessing ? resourceURL : nil
            )
            replyHandler(volume, nil)
        } catch {
            if didStartAccessing { resourceURL.stopAccessingSecurityScopedResource() }
            replyHandler(nil, error)
        }
    }

    func unloadResource(resource: FSResource, options: FSTaskOptions) async throws {
        // The volume's deactivate callback owns worker shutdown. FSKit calls
        // this hook after the resource has been unloaded, so there is no
        // second worker lifetime to terminate here.
    }
}
