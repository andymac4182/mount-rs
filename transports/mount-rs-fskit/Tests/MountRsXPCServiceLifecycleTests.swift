import Dispatch
import Foundation
import XPC

@available(macOS 15.0, *)
@main
struct MountRsXPCServiceLifecycleTests {
    enum TestError: Error {
        case workerCreationFailed
    }

    static func runWorker(
        configuration: MountRsWorkerConfiguration,
        operation: (MountRsWorkerClient) async throws -> Void
    ) async throws {
        guard let worker = MountRsRustWorker(configuration: configuration) else {
            throw TestError.workerCreationFailed
        }
        let listener = XPCListener(
            targetQueue: DispatchQueue(label: "mount-rs.fskit.test"),
            options: .none
        ) { request in
            request.accept(
                incomingMessageHandler: { (message: MountRsXPCDataMessage) -> MountRsXPCDataMessage? in
                    guard let response = worker.dispatch(message.payload) else { return nil }
                    return MountRsXPCDataMessage(payload: response)
                },
                cancellationHandler: { _ in }
            )
        }
        let session = try XPCSession(endpoint: listener.endpoint)
        defer {
            session.cancel(reason: "test complete")
            listener.cancel()
        }
        let client = MountRsWorkerClient(
            transport: MountRsXPCSessionTransport(session: session)
        )
        do {
            try await operation(client)
            try await client.shutdown()
        } catch {
            try? await client.shutdown()
            throw error
        }
    }

    static func runMemoryLifecycle() async throws {
        try await runWorker(
            configuration: MountRsWorkerConfiguration(backend: .memory),
            operation: { client in
                let capabilities = try await client.handshake(readOnly: false)
                precondition(capabilities.handles && !capabilities.readOnly)
                let open = try await client.open("/xpc.txt", flags: "w+", mode: 0o644)
                let payload = Array(repeating: UInt8(0x78), count: 150_000)
                let written = try await client.write(handle: open.handle, offset: 0, data: payload)
                precondition(written == payload.count)
                let read = try await client.read(
                    handle: open.handle,
                    offset: 0,
                    length: UInt64(payload.count)
                )
                precondition(read == payload)
                try await client.close(open.handle)
            }
        )
    }

    static func runHostPathLifecycle(root: URL) async throws {
        let client = try MountRsWorkerClient(
            configuration: MountRsWorkerConfiguration(backend: .host(root: root.path))
        )
        let capabilities = try await client.handshake(readOnly: false)
        precondition(capabilities.handles && !capabilities.readOnly)
        let open = try await client.open("/shared.txt", flags: "w+", mode: 0o644)
        let payload = Array("FSKit and NFS share this rooted path".utf8)
        let written = try await client.write(handle: open.handle, offset: 0, data: payload)
        precondition(written == payload.count)
        try await client.synchronize()
        try await client.close(open.handle)
        try await client.shutdown()

        let hostBytes = try Data(contentsOf: root.appendingPathComponent("shared.txt"))
        precondition(Array(hostBytes) == payload)

        let reopened = try MountRsWorkerClient(
            configuration: MountRsWorkerConfiguration(backend: .host(root: root.path))
        )
        let readOpen = try await reopened.open("/shared.txt", flags: "r")
        let read = try await reopened.read(
            handle: readOpen.handle,
            offset: 0,
            length: UInt64(payload.count)
        )
        precondition(read == payload)
        try await reopened.close(readOpen.handle)
        try await reopened.shutdown()
    }

    static func runSQLiteReopenLifecycle(
        configuration: MountRsWorkerConfiguration
    ) async throws {
        let first = [UInt8(0), 1, 127, 128, 254, 255]
        let second = [UInt8(0), 128, 255, 0x42]
        var expected = [UInt8](repeating: 0, count: 4_096 + second.count)
        expected.replaceSubrange(3..<(3 + first.count), with: first)
        expected.replaceSubrange(4_096..<(4_096 + second.count), with: second)

        try await runWorker(configuration: configuration) { client in
            let capabilities = try await client.handshake(readOnly: false)
            precondition(capabilities.durableWrites)
            let open = try await client.open("/partial.bin", flags: "w+", mode: 0o600)
            let firstWritten = try await client.write(handle: open.handle, offset: 3, data: first)
            precondition(firstWritten == first.count)
            let secondWritten = try await client.write(handle: open.handle, offset: 4_096, data: second)
            precondition(secondWritten == second.count)
            try await client.synchronize()
            try await client.close(open.handle)
        }

        try await runWorker(configuration: configuration) { client in
            let capabilities = try await client.handshake(readOnly: false)
            precondition(capabilities.durableWrites)
            let open = try await client.open("/partial.bin", flags: "r", mode: 0)
            let stats = try await client.stat("/partial.bin")
            precondition(stats.size == UInt64(expected.count))
            let read = try await client.read(
                handle: open.handle,
                offset: 0,
                length: UInt64(expected.count)
            )
            precondition(read == expected)
            try await client.close(open.handle)
        }
    }

    static func runEnvironmentConfigurationTests() {
        guard let defaultConfiguration = MountRsWorkerConfiguration.fromEnvironment([:]) else {
            preconditionFailure("an omitted backend must default to memory")
        }
        guard case .memory = defaultConfiguration.backend else {
            preconditionFailure("the omitted backend did not select memory")
        }
        precondition(!defaultConfiguration.readOnly)

        let sqlitePath = "/tmp/mount-rs-fskit-config.sqlite"
        guard let sqliteConfiguration = MountRsWorkerConfiguration.fromEnvironment([
            "MOUNT_RS_FSKIT_BACKEND": "sqlite",
            "MOUNT_RS_FSKIT_DATABASE_PATH": sqlitePath,
            "MOUNT_RS_FSKIT_READ_ONLY": "1",
        ]) else {
            preconditionFailure("valid SQLite configuration was rejected")
        }
        guard case .sqlite(let databasePath) = sqliteConfiguration.backend else {
            preconditionFailure("SQLite configuration selected the wrong backend")
        }
        precondition(databasePath == sqlitePath)
        precondition(sqliteConfiguration.readOnly)

        let hostPath = "/tmp/mount-rs-fskit-host"
        guard let hostConfiguration = MountRsWorkerConfiguration.fromEnvironment([
            "MOUNT_RS_FSKIT_BACKEND": "host",
            "MOUNT_RS_FSKIT_ROOT": hostPath,
        ]) else {
            preconditionFailure("valid host configuration was rejected")
        }
        guard case .host(let root) = hostConfiguration.backend else {
            preconditionFailure("host configuration selected the wrong backend")
        }
        precondition(root == hostPath)

        let splitEnvironment = [
            "MOUNT_RS_FSKIT_BACKEND": "splitSqlite",
            "MOUNT_RS_FSKIT_METADATA_PATH": "/tmp/mount-rs-fskit-metadata.sqlite",
            "MOUNT_RS_FSKIT_BLOCKS_PATH": "/tmp/mount-rs-fskit-blocks.sqlite",
            "MOUNT_RS_FSKIT_CHUNK_SIZE": "4096",
        ]
        guard let splitConfiguration = MountRsWorkerConfiguration.fromEnvironment(splitEnvironment) else {
            preconditionFailure("valid split SQLite configuration was rejected")
        }
        guard case .splitSQLite(let metadataPath, let blocksPath, let chunkSize) = splitConfiguration.backend else {
            preconditionFailure("split SQLite configuration selected the wrong backend")
        }
        precondition(metadataPath != blocksPath)
        precondition(chunkSize == 4096)

        let invalidConfigurations = [
            ["MOUNT_RS_FSKIT_BACKEND": "unknown"],
            ["MOUNT_RS_FSKIT_BACKEND": "host"],
            ["MOUNT_RS_FSKIT_BACKEND": "sqlite"],
            [
                "MOUNT_RS_FSKIT_BACKEND": "splitSqlite",
                "MOUNT_RS_FSKIT_METADATA_PATH": "/tmp/same.sqlite",
                "MOUNT_RS_FSKIT_BLOCKS_PATH": "/tmp/same.sqlite",
            ],
            [
                "MOUNT_RS_FSKIT_BACKEND": "splitSqlite",
                "MOUNT_RS_FSKIT_METADATA_PATH": "/tmp/metadata.sqlite",
                "MOUNT_RS_FSKIT_BLOCKS_PATH": "/tmp/blocks.sqlite",
                "MOUNT_RS_FSKIT_CHUNK_SIZE": "0",
            ],
            [
                "MOUNT_RS_FSKIT_BACKEND": "splitSqlite",
                "MOUNT_RS_FSKIT_METADATA_PATH": "/tmp/metadata.sqlite",
                "MOUNT_RS_FSKIT_BLOCKS_PATH": "/tmp/blocks.sqlite",
                "MOUNT_RS_FSKIT_CHUNK_SIZE": "not-a-number",
            ],
        ]
        for environment in invalidConfigurations {
            precondition(
                MountRsWorkerConfiguration.fromEnvironment(environment) == nil,
                "invalid worker configuration was accepted: \(environment)"
            )
        }
    }

    static func runWorkerBoundaryTests() {
        guard let worker = MountRsRustWorker(
            configuration: MountRsWorkerConfiguration(backend: .memory)
        ) else {
            preconditionFailure("memory worker could not be created")
        }
        let oversized = Data(
            repeating: 0,
            count: MountRsXPCFrame.headerLength + MountRsXPCFrame.maximumBodyLength + 1
        )
        precondition(
            worker.dispatch(oversized) == nil,
            "oversized XPC payload crossed the Rust worker boundary"
        )
    }

    static func main() async {
        var temporaryRoots: [URL] = []
        do {
            runEnvironmentConfigurationTests()
            runWorkerBoundaryTests()
            try await runMemoryLifecycle()

            let hostRoot = FileManager.default.temporaryDirectory
                .appendingPathComponent("mount-rs-fskit-host-(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: hostRoot, withIntermediateDirectories: false)
            temporaryRoots.append(hostRoot)
            try await runHostPathLifecycle(root: hostRoot)

            let sqliteRoot = FileManager.default.temporaryDirectory
                .appendingPathComponent("mount-rs-fskit-sqlite-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: sqliteRoot, withIntermediateDirectories: false)
            temporaryRoots.append(sqliteRoot)
            try await runSQLiteReopenLifecycle(
                configuration: MountRsWorkerConfiguration(
                    backend: .sqlite(
                        databasePath: sqliteRoot.appendingPathComponent("filesystem.sqlite").path
                    )
                )
            )
            precondition(
                FileManager.default.fileExists(
                    atPath: sqliteRoot.appendingPathComponent("filesystem.sqlite").path
                ),
                "single-database SQLite backend did not create its database"
            )

            let splitRoot = FileManager.default.temporaryDirectory
                .appendingPathComponent("mount-rs-fskit-split-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: splitRoot, withIntermediateDirectories: false)
            temporaryRoots.append(splitRoot)
            try await runSQLiteReopenLifecycle(
                configuration: MountRsWorkerConfiguration(
                    backend: .splitSQLite(
                        metadataPath: splitRoot.appendingPathComponent("metadata.sqlite").path,
                        blocksPath: splitRoot.appendingPathComponent("blocks.sqlite").path,
                        chunkSize: 4_096
                    )
                )
            )
            precondition(
                FileManager.default.fileExists(
                    atPath: splitRoot.appendingPathComponent("metadata.sqlite").path
                ),
                "split SQLite backend did not create its metadata database"
            )
            precondition(
                FileManager.default.fileExists(
                    atPath: splitRoot.appendingPathComponent("blocks.sqlite").path
                ),
                "split SQLite backend did not create its block database"
            )

            print("MountRsXPCServiceLifecycleTests: PASS")
        } catch {
            fputs("MountRsXPCServiceLifecycleTests: FAIL: \(error)\n", stderr)
            exit(1)
        }
        for root in temporaryRoots {
            try? FileManager.default.removeItem(at: root)
        }
    }
}
