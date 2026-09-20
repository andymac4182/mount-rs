import Foundation

private let mountRsWorkerStatusOK: Int32 = 0
private let mountRsWorkerStatusMalformedRequest: Int32 = -2
private let mountRsWorkerStatusResponseTooSmall: Int32 = -3

@_silgen_name("mount_rs_fskit_create_worker")
private func mountRsCreateWorker(
    _ configuration: UnsafePointer<UInt8>?,
    _ configurationLength: Int
) -> OpaquePointer?

@_silgen_name("mount_rs_fskit_worker_dispatch")
private func mountRsWorkerDispatch(
    _ worker: OpaquePointer?,
    _ request: UnsafePointer<UInt8>?,
    _ requestLength: Int,
    _ response: UnsafeMutablePointer<UInt8>?,
    _ responseCapacity: Int,
    _ responseLength: UnsafeMutablePointer<Int>?
) -> Int32

@_silgen_name("mount_rs_fskit_destroy_worker")
private func mountRsDestroyWorker(_ worker: OpaquePointer?)

/// Provider selected when the XPC worker is created.
///
/// `splitSQLite` intentionally carries two paths. The Rust bridge opens them
/// as independent `SqliteMetadataStore` and `SqliteBlockStore` providers,
/// matching the existing split-store configuration instead of treating the
/// pair as one database.
public enum MountRsWorkerBackend: Sendable {
    case memory
    case sqlite(databasePath: String)
    case splitSQLite(metadataPath: String, blocksPath: String, chunkSize: Int = 64 * 1024)
}

public struct MountRsWorkerConfiguration: Sendable {
    public let backend: MountRsWorkerBackend
    public let readOnly: Bool

    public init(backend: MountRsWorkerBackend, readOnly: Bool = false) {
        self.backend = backend
        self.readOnly = readOnly
    }

    fileprivate func encoded() -> Data {
        var object: [String: Any] = [
            "readOnly": readOnly,
        ]
        switch backend {
        case .memory:
            object["backend"] = "memory"
        case .sqlite(let databasePath):
            object["backend"] = "sqlite"
            object["databasePath"] = databasePath
        case .splitSQLite(let metadataPath, let blocksPath, let chunkSize):
            object["backend"] = "splitSqlite"
            object["metadataPath"] = metadataPath
            object["blocksPath"] = blocksPath
            object["chunkSize"] = chunkSize
        }
        return (try? JSONSerialization.data(withJSONObject: object)) ?? Data()
    }

    /// Resolve the optional launch-environment configuration used by the
    /// separately packaged XPC service. An omitted backend defaults to the
    /// deterministic memory provider; a malformed explicitly requested
    /// provider returns nil so the service fails closed at startup.
    static func fromEnvironment(_ environment: [String: String] = ProcessInfo.processInfo.environment) -> Self? {
        let readOnly = environment["MOUNT_RS_FSKIT_READ_ONLY"] == "1"
        guard let backend = environment["MOUNT_RS_FSKIT_BACKEND"] else {
            return Self(backend: .memory, readOnly: readOnly)
        }
        switch backend {
        case "memory":
            return Self(backend: .memory, readOnly: readOnly)
        case "sqlite":
            guard let databasePath = environment["MOUNT_RS_FSKIT_DATABASE_PATH"], !databasePath.isEmpty else {
                return nil
            }
            return Self(backend: .sqlite(databasePath: databasePath), readOnly: readOnly)
        case "splitSqlite":
            guard let metadataPath = environment["MOUNT_RS_FSKIT_METADATA_PATH"],
                  !metadataPath.isEmpty,
                  let blocksPath = environment["MOUNT_RS_FSKIT_BLOCKS_PATH"],
                  !blocksPath.isEmpty,
                  metadataPath != blocksPath
            else {
                return nil
            }
            guard let chunkSize = Int(environment["MOUNT_RS_FSKIT_CHUNK_SIZE"] ?? "65536"),
                  chunkSize > 0
            else {
                return nil
            }
            return Self(
                backend: .splitSQLite(
                    metadataPath: metadataPath,
                    blocksPath: blocksPath,
                    chunkSize: chunkSize
                ),
                readOnly: readOnly
            )
        default:
            return nil
        }
    }
}

/// Swift lifetime wrapper around the persistent Rust `DriverWorker`.
///
/// The service owns one instance for its lifetime. Calls are serialized by
/// the XPC listener queue, and malformed outer frames return `nil` so the
/// service never invents a filesystem error for a request that Rust rejected
/// before dispatch.
final class MountRsRustWorker: @unchecked Sendable {
    private var worker: OpaquePointer?

    init?(configuration: MountRsWorkerConfiguration) {
        let configurationData = configuration.encoded()
        let worker = configurationData.withUnsafeBytes { bytes in
            mountRsCreateWorker(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                configurationData.count
            )
        }
        guard let worker else { return nil }
        self.worker = worker
    }

    convenience init(readOnly: Bool) {
        self.init(
            configuration: MountRsWorkerConfiguration(
                backend: .memory,
                readOnly: readOnly
            )
        )!
    }

    deinit {
        mountRsDestroyWorker(worker)
    }

    func dispatch(_ request: Data) -> Data? {
        guard let worker,
              request.count <= MountRsXPCFrame.headerLength + MountRsXPCFrame.maximumBodyLength
        else { return nil }
        var response = Data(count: MountRsXPCFrame.headerLength + MountRsXPCFrame.maximumBodyLength)
        let responseCapacity = response.count
        var responseLength = 0
        let status = response.withUnsafeMutableBytes { responseBytes in
            request.withUnsafeBytes { requestBytes in
                mountRsWorkerDispatch(
                    worker,
                    requestBytes.bindMemory(to: UInt8.self).baseAddress,
                    request.count,
                    responseBytes.bindMemory(to: UInt8.self).baseAddress,
                    responseCapacity,
                    &responseLength
                )
            }
        }
        guard status == mountRsWorkerStatusOK else {
            // A short caller buffer is not expected with the fixed maximum,
            // while malformed input intentionally has no fabricated reply.
            if status == mountRsWorkerStatusResponseTooSmall {
                return dispatch(request, capacity: responseLength)
            }
            guard status != mountRsWorkerStatusMalformedRequest else { return nil }
            return nil
        }
        return response.prefix(responseLength)
    }

    private func dispatch(_ request: Data, capacity: Int) -> Data? {
        guard let worker,
              capacity >= 0,
              capacity <= MountRsXPCFrame.headerLength + MountRsXPCFrame.maximumBodyLength
        else { return nil }
        var response = Data(count: capacity)
        let responseCapacity = response.count
        var responseLength = 0
        let status = response.withUnsafeMutableBytes { responseBytes in
            request.withUnsafeBytes { requestBytes in
                mountRsWorkerDispatch(
                    worker,
                    requestBytes.bindMemory(to: UInt8.self).baseAddress,
                    request.count,
                    responseBytes.bindMemory(to: UInt8.self).baseAddress,
                    responseCapacity,
                    &responseLength
                )
            }
        }
        guard status == mountRsWorkerStatusOK, responseLength <= response.count else { return nil }
        return response.prefix(responseLength)
    }
}
