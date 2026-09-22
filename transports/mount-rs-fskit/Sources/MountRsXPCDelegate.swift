import Foundation
import XPC

public let MountRsXPCServiceName = "com.andymac4182.mount-rs.worker"
private let mountRsWorkerDataChunkLength = 128 * 1024

/// Message kinds shared with mount-rs-fskit-bridge/src/lib.rs.
public enum MountRsXPCMessageKind: UInt8, Sendable {
    case hello = 1
    case shutdown = 2
    case operation = 3
    case reply = 0x80
    case error = 0x81
}

public enum MountRsXPCFrameError: Error, Equatable {
    case tooShort(actual: Int)
    case invalidMagic
    case unsupportedVersion(UInt16)
    case unsupportedFlags(UInt8)
    case unknownKind(UInt8)
    case bodyTooLarge(Int)
    case lengthMismatch(expected: Int, actual: Int)
    case requestIDMismatch(expected: UInt64, actual: UInt64)
    case unexpectedResponseKind(MountRsXPCMessageKind)
}

/// The fixed envelope transported as one XPC Codable message.
///
/// Wire layout, all little-endian:
///
///     magic[4] = "MRFS"
///     version:u16, kind:u8, flags:u8
///     request_id:u64, body_length:u32, body[body_length]
///
/// XPC provides the outer message framing. The envelope still validates its
/// own length and body bound so the Rust worker has the same input contract
/// when it is called through an injected test transport or the XPC service.
public struct MountRsXPCFrame: Equatable, Sendable {
    public static let protocolVersion: UInt16 = 1
    public static let headerLength = 20
    public static let maximumBodyLength = 1024 * 1024

    private static let magic: [UInt8] = [0x4D, 0x52, 0x46, 0x53]

    public let kind: MountRsXPCMessageKind
    public let requestID: UInt64
    public let body: Data

    public init(
        kind: MountRsXPCMessageKind,
        requestID: UInt64,
        body: Data = Data()
    ) throws {
        guard body.count <= Self.maximumBodyLength else {
            throw MountRsXPCFrameError.bodyTooLarge(body.count)
        }
        self.kind = kind
        self.requestID = requestID
        self.body = body
    }

    public func encoded() -> Data {
        var result = Data(capacity: Self.headerLength + body.count)
        result.append(contentsOf: Self.magic)
        result.mountRsAppendUInt16LE(Self.protocolVersion)
        result.append(kind.rawValue)
        result.append(0)
        result.mountRsAppendUInt64LE(requestID)
        result.mountRsAppendUInt32LE(UInt32(body.count))
        result.append(body)
        return result
    }

    public static func decode(_ data: Data) throws -> Self {
        guard data.count >= Self.headerLength else {
            throw MountRsXPCFrameError.tooShort(actual: data.count)
        }
        guard Array(data.prefix(Self.magic.count)) == Self.magic else {
            throw MountRsXPCFrameError.invalidMagic
        }

        let version = readUInt16(from: data, at: 4)
        guard version == Self.protocolVersion else {
            throw MountRsXPCFrameError.unsupportedVersion(version)
        }

        let flags = data[7]
        guard flags == 0 else {
            throw MountRsXPCFrameError.unsupportedFlags(flags)
        }

        guard let kind = MountRsXPCMessageKind(rawValue: data[6]) else {
            throw MountRsXPCFrameError.unknownKind(data[6])
        }

        let requestID = readUInt64(from: data, at: 8)
        let bodyLength = Int(readUInt32(from: data, at: 16))
        guard bodyLength <= Self.maximumBodyLength else {
            throw MountRsXPCFrameError.bodyTooLarge(bodyLength)
        }

        let expectedLength = Self.headerLength + bodyLength
        guard data.count == expectedLength else {
            throw MountRsXPCFrameError.lengthMismatch(
                expected: expectedLength,
                actual: data.count
            )
        }

        return try Self(
            kind: kind,
            requestID: requestID,
            body: data.subdata(in: Self.headerLength..<expectedLength)
        )
    }

    private static func readUInt16(from data: Data, at offset: Int) -> UInt16 {
        UInt16(data[offset]) | (UInt16(data[offset + 1]) << 8)
    }

    private static func readUInt32(from data: Data, at offset: Int) -> UInt32 {
        var value: UInt32 = 0
        for byteOffset in 0..<MemoryLayout<UInt32>.size {
            value |= UInt32(data[offset + byteOffset]) << UInt32(byteOffset * 8)
        }
        return value
    }

    private static func readUInt64(from data: Data, at offset: Int) -> UInt64 {
        var value: UInt64 = 0
        for byteOffset in 0..<MemoryLayout<UInt64>.size {
            value |= UInt64(data[offset + byteOffset]) << UInt64(byteOffset * 8)
        }
        return value
    }
}

private extension Data {
    mutating func mountRsAppendUInt16LE(_ value: UInt16) {
        append(UInt8(truncatingIfNeeded: value))
        append(UInt8(truncatingIfNeeded: value >> 8))
    }

    mutating func mountRsAppendUInt32LE(_ value: UInt32) {
        for shift in stride(from: 0, through: 24, by: 8) {
            append(UInt8(truncatingIfNeeded: value >> UInt32(shift)))
        }
    }

    mutating func mountRsAppendUInt64LE(_ value: UInt64) {
        for shift in stride(from: 0, through: 56, by: 8) {
            append(UInt8(truncatingIfNeeded: value >> UInt64(shift)))
        }
    }
}

/// Injectable transport boundary used by the FSKit delegate.
///
/// Tests can provide an in-memory responder that calls the Rust crate's
/// protocol delegate. Production wiring uses `MountRsXPCSessionTransport`
/// with the separately built XPC service; signed containing-app packaging is
/// intentionally outside this unsigned checkpoint.
public protocol MountRsXPCTransport {
    func send(_ request: Data) async throws -> Data
}

public struct MountRsXPCDelegate {
    private let transport: any MountRsXPCTransport

    public init(transport: any MountRsXPCTransport) {
        self.transport = transport
    }

    @discardableResult
    public func send(_ request: MountRsXPCFrame) async throws -> MountRsXPCFrame {
        let responseData = try await transport.send(request.encoded())
        let response = try MountRsXPCFrame.decode(responseData)
        guard response.requestID == request.requestID else {
            throw MountRsXPCFrameError.requestIDMismatch(
                expected: request.requestID,
                actual: response.requestID
            )
        }
        guard response.kind == .reply || response.kind == .error else {
            throw MountRsXPCFrameError.unexpectedResponseKind(response.kind)
        }
        return response
    }

    public func hello(requestID: UInt64) async throws -> MountRsXPCFrame {
        try await send(
            MountRsXPCFrame(kind: .hello, requestID: requestID)
        )
    }

    public func shutdown(requestID: UInt64) async throws -> MountRsXPCFrame {
        try await send(
            MountRsXPCFrame(kind: .shutdown, requestID: requestID)
        )
    }

    public func operation(requestID: UInt64, body: Data) async throws -> MountRsXPCFrame {
        try await send(
            MountRsXPCFrame(kind: .operation, requestID: requestID, body: body)
        )
    }
}

/// Codable payload used by the modern Swift XPC session API.
///
/// The Rust protocol remains responsible for the inner bytes. This wrapper
/// only gives XPC a typed, bounded message value; it is not a filesystem
/// operation schema.
public struct MountRsXPCDataMessage: Codable, Sendable {
    public let payload: Data

    public init(payload: Data) {
        self.payload = payload
    }
}

/// Real XPC-session adapter. Keeping it behind `MountRsXPCTransport` makes the
/// delegate independently unit-testable while the service packaging/signing
/// boundary remains in the approved containing-app phase.
@available(macOS 14.0, *)
public final class MountRsXPCSessionTransport: MountRsXPCTransport {
    private let session: XPCSession

    public init(session: XPCSession) {
        self.session = session
    }

    public convenience init(xpcService: String) throws {
        try self.init(session: XPCSession(xpcService: xpcService))
    }

    public func send(_ request: Data) async throws -> Data {
        let message = MountRsXPCDataMessage(payload: request)
        return try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<Data, any Error>) in
            do {
                try session.send(message) {
                    (result: Result<MountRsXPCDataMessage, any Error>) in
                    switch result {
                    case .success(let reply):
                        continuation.resume(returning: reply.payload)
                    case .failure(let error):
                        continuation.resume(throwing: error)
                    }
                }
            } catch {
                continuation.resume(throwing: error)
            }
        }
    }
}

public struct MountRsWorkerStats: Codable, Sendable {
    public let dev: UInt64
    public let ino: UInt64
    public let mode: UInt32
    public let nlink: UInt64
    public let uid: UInt32
    public let gid: UInt32
    public let rdev: UInt64
    public let size: UInt64
    public let blksize: UInt64
    public let blocks: UInt64
    public let atimeMS: Int64
    public let mtimeMS: Int64
    public let ctimeMS: Int64
    public let birthtimeMS: Int64

    enum CodingKeys: String, CodingKey {
        case dev, ino, mode, nlink, uid, gid, rdev, size, blksize, blocks
        case atimeMS = "atime_ms"
        case mtimeMS = "mtime_ms"
        case ctimeMS = "ctime_ms"
        case birthtimeMS = "birthtime_ms"
    }

    public var isDirectory: Bool { mode & 0o170000 == 0o040000 }
    public var isSymlink: Bool { mode & 0o170000 == 0o120000 }
}

public struct MountRsWorkerStatsFS: Codable, Sendable {
    public let filesystemType: UInt64
    public let blockSize: UInt64
    public let blocks: UInt64
    public let blocksFree: UInt64
    public let blocksAvailable: UInt64
    public let files: UInt64
    public let filesFree: UInt64

    enum CodingKeys: String, CodingKey {
        case filesystemType = "filesystem_type"
        case blockSize = "block_size"
        case blocks
        case blocksFree = "blocks_free"
        case blocksAvailable = "blocks_available"
        case files
        case filesFree = "files_free"
    }
}

public struct MountRsWorkerCapabilities: Codable, Sendable {
    public let handles: Bool
    public let hardlinks: Bool
    public let symlinks: Bool
    public let permissions: Bool
    public let times: Bool
    public let truncate: Bool
    public let atomicRename: Bool
    public let caseSensitive: Bool
    public let statfs: Bool
    public let readOnly: Bool
    public let durableWrites: Bool
    public let mknod: Bool

    enum CodingKeys: String, CodingKey {
        case handles, hardlinks, symlinks, permissions, times, truncate
        case atomicRename = "atomic_rename"
        case caseSensitive = "case_sensitive"
        case statfs
        case readOnly = "read_only"
        case durableWrites = "durable_writes"
        case mknod
    }
}

public struct MountRsWorkerDirectoryEntry: Decodable, Sendable {
    public let name: String
    public let parentPath: String
    public let fileType: String
    public let stats: MountRsWorkerStats

    enum CodingKeys: String, CodingKey {
        case entry, stats
    }

    enum EntryKeys: String, CodingKey {
        case name
        case parentPath = "parent_path"
        case fileType = "file_type"
    }

    public init(name: String, parentPath: String, fileType: String, stats: MountRsWorkerStats) {
        self.name = name
        self.parentPath = parentPath
        self.fileType = fileType
        self.stats = stats
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let entry = try container.nestedContainer(keyedBy: EntryKeys.self, forKey: .entry)
        name = try entry.decode(String.self, forKey: .name)
        parentPath = try entry.decode(String.self, forKey: .parentPath)
        fileType = try entry.decode(String.self, forKey: .fileType)
        stats = try container.decode(MountRsWorkerStats.self, forKey: .stats)
    }
}

public struct MountRsWorkerError: Error, LocalizedError, Sendable {
    public let errno: Int32
    public let code: String
    public let message: String
    public let syscall: String?
    public let path: String?
    public let destination: String?

    public var errorDescription: String? { message }

    public func asNSError() -> NSError {
        var userInfo: [String: Any] = [NSLocalizedDescriptionKey: message, "mount-rs.code": code]
        if let syscall { userInfo["mount-rs.syscall"] = syscall }
        if let path { userInfo["mount-rs.path"] = path }
        if let destination { userInfo["mount-rs.dest"] = destination }
        return NSError(domain: NSPOSIXErrorDomain, code: Int(errno), userInfo: userInfo)
    }
}

public final class MountRsWorkerClient: @unchecked Sendable {
    private let delegate: MountRsXPCDelegate
    private let requestIDLock = NSLock()
    private var nextRequestID: UInt64 = 1

    public init(transport: any MountRsXPCTransport) {
        self.delegate = MountRsXPCDelegate(transport: transport)
    }

    @available(macOS 14.0, *)
    public convenience init(xpcService: String) throws {
        try self.init(transport: MountRsXPCSessionTransport(xpcService: xpcService))
    }

    private func allocateRequestID() -> UInt64 {
        requestIDLock.lock()
        defer { requestIDLock.unlock() }
        defer { nextRequestID &+= 1 }
        return nextRequestID
    }

    private func send(_ object: [String: Any]) async throws -> [String: Any] {
        guard JSONSerialization.isValidJSONObject(object) else {
            throw MountRsWorkerError(
                errno: 22,
                code: "EINVAL",
                message: "operation request is not valid JSON",
                syscall: "fskit-worker",
                path: nil,
                destination: nil
            )
        }
        let body = try JSONSerialization.data(withJSONObject: object)
        let response = try await delegate.operation(requestID: allocateRequestID(), body: body)
        if response.kind == .error {
            throw try decodeError(response.body)
        }
        guard response.kind == .reply,
              let object = try JSONSerialization.jsonObject(with: response.body) as? [String: Any]
        else {
            throw MountRsWorkerError(
                errno: 71,
                code: "EPROTO",
                message: "worker returned a non-JSON operation reply",
                syscall: "fskit-worker",
                path: nil,
                destination: nil
            )
        }
        return object
    }

    private func decodeError(_ body: Data) throws -> MountRsWorkerError {
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let errno = object["errno"] as? NSNumber,
              let code = object["code"] as? String,
              let message = object["message"] as? String
        else {
            return MountRsWorkerError(
                errno: 71,
                code: "EPROTO",
                message: "worker returned an unstructured error frame",
                syscall: "fskit-worker",
                path: nil,
                destination: nil
            )
        }
        return MountRsWorkerError(
            errno: errno.int32Value,
            code: code,
            message: message,
            syscall: object["syscall"] as? String,
            path: object["path"] as? String,
            destination: object["dest"] as? String
        )
    }

    private func decode<T: Decodable>(_ value: Any, as type: T.Type) throws -> T {
        // JSONSerialization requires an object or array at the top level,
        // while worker replies also contain scalar fields such as handles
        // and counts. Wrap the value without changing its JSON shape before
        // handing it to Codable.
        let data = try JSONSerialization.data(withJSONObject: [value])
        guard let decoded = try JSONDecoder().decode([T].self, from: data).first else {
            throw MountRsWorkerError(
                errno: 71,
                code: "EPROTO",
                message: "worker reply contained an empty value",
                syscall: "fskit-worker",
                path: nil,
                destination: nil
            )
        }
        return decoded
    }

    private func require<T: Decodable>(_ key: String, in response: [String: Any], as type: T.Type) throws -> T {
        guard let value = response[key] else {
            throw MountRsWorkerError(
                errno: 71,
                code: "EPROTO",
                message: "worker reply omitted '\(key)'",
                syscall: "fskit-worker",
                path: nil,
                destination: nil
            )
        }
        return try decode(value, as: type)
    }

    private func requireOK(_ response: [String: Any]) throws {
        guard response["response"] as? String == "ok" else {
            throw MountRsWorkerError(
                errno: 71,
                code: "EPROTO",
                message: "worker reply was not an ok response",
                syscall: "fskit-worker",
                path: nil,
                destination: nil
            )
        }
    }

    public func handshake(readOnly: Bool) async throws -> MountRsWorkerCapabilities {
        let response = try await send(["operation": "handshake", "readOnly": readOnly])
        return try require("capabilities", in: response, as: MountRsWorkerCapabilities.self)
    }

    public func capabilities() async throws -> MountRsWorkerCapabilities {
        let response = try await send(["operation": "capabilities"])
        return try require("capabilities", in: response, as: MountRsWorkerCapabilities.self)
    }

    public func synchronize() async throws {
        try requireOK(await send(["operation": "sync"]))
    }

    public func stat(_ path: String) async throws -> MountRsWorkerStats {
        try require("stats", in: await send(["operation": "stat", "path": path]), as: MountRsWorkerStats.self)
    }

    public func lstat(_ path: String) async throws -> MountRsWorkerStats {
        try require("stats", in: await send(["operation": "lstat", "path": path]), as: MountRsWorkerStats.self)
    }

    public func statfs(_ path: String) async throws -> MountRsWorkerStatsFS {
        try require("stats", in: await send(["operation": "statfs", "path": path]), as: MountRsWorkerStatsFS.self)
    }

    public func readdir(_ path: String) async throws -> [MountRsWorkerDirectoryEntry] {
        try require("entries", in: await send(["operation": "readdir", "path": path]), as: [MountRsWorkerDirectoryEntry].self)
    }

    public func open(_ path: String, flags: String, mode: UInt32 = 0) async throws -> (handle: UInt64, stats: MountRsWorkerStats) {
        let response = try await send(["operation": "open", "path": path, "flags": flags, "mode": mode])
        return (
            try require("handle", in: response, as: UInt64.self),
            try require("stats", in: response, as: MountRsWorkerStats.self)
        )
    }

    public func read(handle: UInt64, offset: UInt64, length: UInt64) async throws -> [UInt8] {
        guard length > 0 else { return [] }
        var result: [UInt8] = []
        result.reserveCapacity(Int(min(length, UInt64(mountRsWorkerDataChunkLength))))
        var cursor = offset
        var remaining = length
        while remaining > 0 {
            let requested = min(remaining, UInt64(mountRsWorkerDataChunkLength))
            let response = try await send([
                "operation": "read",
                "handle": handle,
                "offset": cursor,
                "length": requested,
            ])
            let chunk = try require("data", in: response, as: [UInt8].self)
            guard UInt64(chunk.count) <= requested else {
                throw MountRsWorkerError(
                    errno: 71,
                    code: "EPROTO",
                    message: "worker returned more bytes than requested",
                    syscall: "read",
                    path: nil,
                    destination: nil
                )
            }
            result.append(contentsOf: chunk)
            if UInt64(chunk.count) < requested { break }
            remaining -= requested
            let (nextCursor, overflow) = cursor.addingReportingOverflow(requested)
            guard !overflow else {
                throw MountRsWorkerError(
                    errno: 75,
                    code: "EOVERFLOW",
                    message: "read offset overflow",
                    syscall: "read",
                    path: nil,
                    destination: nil
                )
            }
            cursor = nextCursor
        }
        return result
    }

    public func write(handle: UInt64, offset: UInt64, data: [UInt8]) async throws -> Int {
        guard !data.isEmpty else { return 0 }
        var total = 0
        while total < data.count {
            let end = min(data.count, total + mountRsWorkerDataChunkLength)
            let (chunkOffset, overflow) = offset.addingReportingOverflow(UInt64(total))
            guard !overflow else {
                throw MountRsWorkerError(
                    errno: 75,
                    code: "EOVERFLOW",
                    message: "write offset overflow",
                    syscall: "write",
                    path: nil,
                    destination: nil
                )
            }
            let response = try await send([
                "operation": "write",
                "handle": handle,
                "offset": chunkOffset,
                "data": data[total..<end].map(Int.init),
            ])
            let count = try require("count", in: response, as: Int.self)
            guard count >= 0, count <= end - total else {
                throw MountRsWorkerError(
                    errno: 71,
                    code: "EPROTO",
                    message: "worker returned an invalid write count",
                    syscall: "write",
                    path: nil,
                    destination: nil
                )
            }
            total += count
            if count == 0 { break }
        }
        return total
    }

    public func handleStat(_ handle: UInt64) async throws -> MountRsWorkerStats {
        try require("stats", in: await send(["operation": "handleStat", "handle": handle]), as: MountRsWorkerStats.self)
    }

    public func close(_ handle: UInt64) async throws {
        try requireOK(await send(["operation": "close", "handle": handle]))
    }

    public func mkdir(_ path: String, recursive: Bool = false, mode: UInt32? = nil) async throws {
        var object: [String: Any] = ["operation": "mkdir", "path": path, "recursive": recursive]
        object["mode"] = mode.map { NSNumber(value: $0) } ?? NSNull()
        try requireOK(await send(object))
    }

    public func rmdir(_ path: String) async throws { try requireOK(await send(["operation": "rmdir", "path": path])) }
    public func unlink(_ path: String) async throws { try requireOK(await send(["operation": "unlink", "path": path])) }
    public func rename(_ oldPath: String, _ newPath: String) async throws {
        try requireOK(await send(["operation": "rename", "oldPath": oldPath, "newPath": newPath]))
    }
    public func link(_ existingPath: String, _ newPath: String) async throws {
        try requireOK(await send(["operation": "link", "existingPath": existingPath, "newPath": newPath]))
    }
    public func symlink(target: String, path: String) async throws {
        try requireOK(await send(["operation": "symlink", "target": target, "path": path]))
    }
    public func readlink(_ path: String) async throws -> String {
        try require("value", in: await send(["operation": "readlink", "path": path]), as: String.self)
    }
    public func chmod(_ path: String, mode: UInt32) async throws {
        try requireOK(await send(["operation": "chmod", "path": path, "mode": mode]))
    }
    public func chown(_ path: String, uid: UInt32, gid: UInt32) async throws {
        try requireOK(await send(["operation": "chown", "path": path, "uid": uid, "gid": gid]))
    }
    public func lchown(_ path: String, uid: UInt32, gid: UInt32) async throws {
        try requireOK(await send(["operation": "lchown", "path": path, "uid": uid, "gid": gid]))
    }
    public func truncate(_ path: String, length: UInt64) async throws {
        try requireOK(await send(["operation": "truncate", "path": path, "length": length]))
    }
    public func utimes(_ path: String, atimeMS: Int64, mtimeMS: Int64) async throws {
        try requireOK(await send(["operation": "utimes", "path": path, "atimeMs": atimeMS, "mtimeMs": mtimeMS]))
    }
    public func lutimes(_ path: String, atimeMS: Int64, mtimeMS: Int64) async throws {
        try requireOK(await send(["operation": "lutimes", "path": path, "atimeMs": atimeMS, "mtimeMs": mtimeMS]))
    }

    public func shutdown() async throws {
        _ = try await delegate.shutdown(requestID: allocateRequestID())
    }
}
