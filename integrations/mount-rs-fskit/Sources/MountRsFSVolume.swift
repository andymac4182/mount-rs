import Foundation
import FSKit

/// The FSKit V1 volume adapter.
///
/// This class owns only Apple-facing item identity and callback translation.
/// Every namespace, metadata, handle, and data operation is delegated to the
/// Rust `FsDriver` worker through `MountRsWorkerClient`.
final class MountRsFSVolume: FSVolume, FSVolume.Operations, FSVolume.OpenCloseOperations, FSVolume.ReadWriteOperations {
    private let worker: MountRsWorkerClient
    private let readOnly: Bool
    private let resourceURL: URL?
    private var rootItem: MountRsFSItem?
    private var cachedStatsFS: MountRsWorkerStatsFS?

    init(worker: MountRsWorkerClient, volumeName: String, readOnly: Bool, resourceURL: URL? = nil) {
        self.worker = worker
        self.readOnly = readOnly
        self.resourceURL = resourceURL
        super.init(
            volumeID: FSVolume.Identifier(),
            volumeName: FSFileName(string: volumeName)
        )
    }

    var supportedVolumeCapabilities: FSVolume.SupportedCapabilities {
        let capabilities = FSVolume.SupportedCapabilities()
        capabilities.supportsPersistentObjectIDs = true
        capabilities.supportsSymbolicLinks = true
        capabilities.supportsHardLinks = true
        capabilities.supports2TBFiles = true
        capabilities.supports64BitObjectIDs = true
        capabilities.supportsFastStatFS = true
        capabilities.doesNotSupportVolumeSizes = false
        capabilities.doesNotSupportImmutableFiles = true
        capabilities.doesNotSupportSettingFilePermissions = false
        capabilities.caseFormat = .sensitive
        return capabilities
    }

    var volumeStatistics: FSStatFSResult {
        let result = FSStatFSResult(fileSystemTypeName: "mount-rs")
        let stats = cachedStatsFS
        let blockSize = stats?.blockSize ?? 4096
        result.blockSize = Int(blockSize)
        result.ioSize = Int(blockSize)
        result.totalBlocks = stats?.blocks ?? 0
        result.availableBlocks = stats?.blocksAvailable ?? 0
        result.freeBlocks = stats?.blocksFree ?? 0
        result.usedBlocks = (stats?.blocks ?? 0).saturatingSubtract(stats?.blocksFree ?? 0)
        result.totalFiles = stats?.files ?? 0
        result.freeFiles = stats?.filesFree ?? 0
        result.totalBytes = result.totalBlocks.saturatingMultiply(blockSize)
        result.availableBytes = result.availableBlocks.saturatingMultiply(blockSize)
        result.freeBytes = result.freeBlocks.saturatingMultiply(blockSize)
        result.usedBytes = result.usedBlocks.saturatingMultiply(blockSize)
        return result
    }

    var maximumLinkCount: Int { Int(Int32.max) }
    var maximumNameLength: Int { 255 }
    var restrictsOwnershipChanges: Bool { false }
    var truncatesLongNames: Bool { false }

    func mount(options: FSTaskOptions, replyHandler: @escaping ((any Error)?) -> Void) {
        run(replyHandler: replyHandler) {
            try await self.worker.synchronize()
        }
    }

    func unmount(replyHandler: @escaping () -> Void) {
        Task {
            _ = try? await self.worker.synchronize()
            replyHandler()
        }
    }

    func synchronize(flags: FSSyncFlags, replyHandler: @escaping ((any Error)?) -> Void) {
        run(replyHandler: replyHandler) {
            try await self.worker.synchronize()
        }
    }

    func getAttributes(
        _ desiredAttributes: FSItem.GetAttributesRequest,
        of item: FSItem,
        replyHandler: @escaping (FSItem.Attributes?, (any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            let stats = try await self.worker.stat(self.path(of: item))
            return self.attributes(from: stats, wanted: desiredAttributes.wantedAttributes)
        }
    }

    func setAttributes(
        _ newAttributes: FSItem.SetAttributesRequest,
        on item: FSItem,
        replyHandler: @escaping (FSItem.Attributes?, (any Error)?) -> Void
    ) {
        let request = newAttributes
        run(replyHandler: replyHandler) {
            let path = try self.path(of: item)
            try await self.apply(attributes: request, to: path)
            let stats = try await self.worker.stat(path)
            return self.attributes(from: stats)
        }
    }

    func lookupItem(
        named name: FSFileName,
        inDirectory directory: FSItem,
        replyHandler: @escaping (FSItem?, FSFileName?, (any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            guard let component = name.string, !component.isEmpty,
                  !component.contains("/"), !component.contains("\0")
            else { throw self.posixError(22, "lookup name is not valid UTF-8") }
            let path = try self.join(self.path(of: directory), component)
            let stats = try await self.worker.lstat(path)
            return (
                MountRsFSItem(path: path, stats: stats) as FSItem,
                FSFileName(string: component)
            )
        }
    }

    func reclaimItem(_ item: FSItem, replyHandler: @escaping ((any Error)?) -> Void) {
        // FSKit owns the item object lifetime. Rust handles are closed by the
        // explicit OpenClose callback or by worker shutdown, never by reclaim.
        replyHandler(nil)
    }

    func readSymbolicLink(
        _ item: FSItem,
        replyHandler: @escaping (FSFileName?, (any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            let target = try await self.worker.readlink(self.path(of: item))
            return FSFileName(string: target)
        }
    }

    func createItem(
        named name: FSFileName,
        type: FSItem.ItemType,
        inDirectory directory: FSItem,
        attributes newAttributes: FSItem.SetAttributesRequest,
        replyHandler: @escaping (FSItem?, FSFileName?, (any Error)?) -> Void
    ) {
        let request = newAttributes
        run(replyHandler: replyHandler) {
            guard let component = name.string, !component.isEmpty,
                  !component.contains("/"), !component.contains("\0")
            else { throw self.posixError(22, "item name is not valid UTF-8") }
            let path = try self.join(self.path(of: directory), component)
            switch type {
            case .file:
                let opened = try await self.worker.open(path, flags: "wx", mode: request.mode & 0o7777)
                try await self.worker.close(opened.handle)
            case .directory:
                try await self.worker.mkdir(path, mode: request.mode & 0o7777)
            default:
                throw self.posixError(22, "createItem only accepts files and directories")
            }
            try await self.apply(attributes: request, to: path)
            let stats = try await self.worker.lstat(path)
            return (
                MountRsFSItem(path: path, stats: stats) as FSItem,
                FSFileName(string: component)
            )
        }
    }

    func createSymbolicLink(
        named name: FSFileName,
        inDirectory directory: FSItem,
        attributes newAttributes: FSItem.SetAttributesRequest,
        linkContents contents: FSFileName,
        replyHandler: @escaping (FSItem?, FSFileName?, (any Error)?) -> Void
    ) {
        let request = newAttributes
        run(replyHandler: replyHandler) {
            guard let component = name.string, let target = contents.string,
                  !component.isEmpty, !component.contains("/"), !component.contains("\0")
            else { throw self.posixError(22, "symbolic-link name or contents is not UTF-8") }
            let path = try self.join(self.path(of: directory), component)
            try await self.worker.symlink(target: target, path: path)
            try await self.apply(attributes: request, to: path)
            let stats = try await self.worker.lstat(path)
            return (
                MountRsFSItem(path: path, stats: stats) as FSItem,
                FSFileName(string: component)
            )
        }
    }

    func createLink(
        to item: FSItem,
        named name: FSFileName,
        inDirectory directory: FSItem,
        replyHandler: @escaping (FSFileName?, (any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            guard let component = name.string, !component.isEmpty,
                  !component.contains("/"), !component.contains("\0")
            else { throw self.posixError(22, "link name is not valid UTF-8") }
            let newPath = try self.join(self.path(of: directory), component)
            try await self.worker.link(try self.path(of: item), newPath)
            return FSFileName(string: component)
        }
    }

    func removeItem(
        _ item: FSItem,
        named name: FSFileName,
        fromDirectory directory: FSItem,
        replyHandler: @escaping ((any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            let path = try self.join(self.path(of: directory), self.name(name))
            let stats = try await self.worker.lstat(path)
            if stats.isDirectory {
                try await self.worker.rmdir(path)
            } else {
                try await self.worker.unlink(path)
            }
        }
    }

    func renameItem(
        _ item: FSItem,
        inDirectory sourceDirectory: FSItem,
        named sourceName: FSFileName,
        to destinationName: FSFileName,
        inDirectory destinationDirectory: FSItem,
        overItem: FSItem?,
        replyHandler: @escaping (FSFileName?, (any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            let source = try self.join(self.path(of: sourceDirectory), self.name(sourceName))
            let destinationComponent = try self.name(destinationName)
            let destination = try self.join(self.path(of: destinationDirectory), destinationComponent)
            try await self.worker.rename(source, destination)
            if let moved = item as? MountRsFSItem { moved.path = destination }
            return FSFileName(string: destinationComponent)
        }
    }

    func enumerateDirectory(
        _ directory: FSItem,
        startingAt cookie: FSDirectoryCookie,
        verifier: FSDirectoryVerifier,
        attributes: FSItem.GetAttributesRequest?,
        packer: FSDirectoryEntryPacker,
        replyHandler: @escaping (FSDirectoryVerifier, (any Error)?) -> Void
    ) {
        runVerifier(replyHandler: replyHandler) {
            let path = try self.path(of: directory)
            var entries = try await self.worker.readdir(path)
            if attributes == nil {
                let directoryStats = try await self.worker.stat(path)
                entries.insert(
                    MountRsWorkerDirectoryEntry(
                        name: ".", parentPath: path, fileType: "Directory", stats: directoryStats
                    ), at: 0
                )
                entries.insert(
                    MountRsWorkerDirectoryEntry(
                        name: "..", parentPath: path, fileType: "Directory", stats: directoryStats
                    ), at: 1
                )
            }
            guard cookie.rawValue <= UInt64(entries.count) else {
                throw self.posixError(22, "invalid directory cookie")
            }
            let start = Int(cookie.rawValue)
            for (offset, entry) in entries.dropFirst(start).enumerated() {
                let itemName = FSFileName(string: entry.name)
                let itemID = FSItem.Identifier(rawValue: entry.stats.ino) ?? .invalid
                let packedAttributes = attributes.map {
                    self.attributes(from: entry.stats, wanted: $0.wantedAttributes)
                }
                let shouldContinue = packer.packEntry(
                    name: itemName,
                    itemType: self.itemType(from: entry.stats.mode),
                    itemID: itemID,
                    nextCookie: FSDirectoryCookie(rawValue: cookie.rawValue + UInt64(offset) + 1),
                    attributes: packedAttributes
                )
                if !shouldContinue { break }
            }
            return FSDirectoryVerifier(1)
        }
    }

    func activate(
        options: FSTaskOptions,
        replyHandler: @escaping (FSItem?, (any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            let capabilities = try await self.worker.handshake(readOnly: self.readOnly)
            guard capabilities.readOnly == self.readOnly || !self.readOnly else {
                throw self.posixError(71, "worker read-only policy mismatch")
            }
            let stats = try await self.worker.stat("/")
            self.cachedStatsFS = try? await self.worker.statfs("/")
            let root = MountRsFSItem(path: "/", stats: stats)
            self.rootItem = root
            return root as FSItem
        }
    }

    func deactivate(
        options: FSDeactivateOptions,
        replyHandler: @escaping ((any Error)?) -> Void
    ) {
        Task {
            if let resourceURL = self.resourceURL {
                defer { resourceURL.stopAccessingSecurityScopedResource() }
            }
            do {
                try await self.worker.shutdown()
                replyHandler(nil)
            } catch {
                replyHandler(self.nsError(error))
            }
        }
    }

    func openItem(
        _ item: FSItem,
        modes: FSVolume.OpenModes,
        replyHandler: @escaping ((any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            guard let rustItem = item as? MountRsFSItem else { throw self.posixError(22, "unknown FSItem") }
            if rustItem.handle == nil {
                let flags = modes.contains(.write) ? "r+" : "r"
                rustItem.handle = try await self.worker.open(rustItem.path, flags: flags).handle
                rustItem.openModes = modes
            } else if modes.contains(.write) && !rustItem.openModes.contains(.write) {
                // A prior read-only open cannot satisfy a later write open.
                // Replace the opaque Rust handle before recording the union of
                // access modes retained by FSKit.
                var retainedModes = rustItem.openModes
                retainedModes.formUnion(modes)
                if let handle = rustItem.handle { try await self.worker.close(handle) }
                rustItem.handle = nil
                rustItem.openModes = []
                do {
                    rustItem.handle = try await self.worker.open(rustItem.path, flags: "r+").handle
                    rustItem.openModes = retainedModes
                } catch {
                    rustItem.handle = nil
                    rustItem.openModes = []
                    throw error
                }
            } else {
                rustItem.openModes.formUnion(modes)
            }
        }
    }

    func closeItem(
        _ item: FSItem,
        modes: FSVolume.OpenModes,
        replyHandler: @escaping ((any Error)?) -> Void
    ) {
        run(replyHandler: replyHandler) {
            guard let rustItem = item as? MountRsFSItem else { throw self.posixError(22, "unknown FSItem") }
            guard !modes.contains(.read) && !modes.contains(.write), let handle = rustItem.handle else {
                rustItem.openModes = modes
                return
            }
            try await self.worker.close(handle)
            rustItem.handle = nil
            rustItem.openModes = []
        }
    }

    func read(
        from item: FSItem,
        at offset: off_t,
        length: Int,
        into buffer: FSMutableFileDataBuffer,
        replyHandler: @escaping (Int, (any Error)?) -> Void
    ) {
        var temporaryHandle: UInt64?
        Task {
            do {
                guard offset >= 0, length >= 0 else { throw posixError(22, "negative read range") }
                let (handle, temporary) = try await self.handle(for: item, flags: "r")
                temporaryHandle = temporary ? handle : nil
                let bytes = try await self.worker.read(handle: handle, offset: UInt64(offset), length: UInt64(length))
                buffer.withUnsafeMutableBytes { destination in
                    bytes.withUnsafeBytes { source in
                        destination.copyBytes(from: source)
                    }
                }
                if temporary { try? await self.worker.close(handle); temporaryHandle = nil }
                replyHandler(bytes.count, nil)
            } catch {
                if let temporaryHandle { try? await self.worker.close(temporaryHandle) }
                replyHandler(0, self.nsError(error))
            }
        }
    }

    func write(
        contents: Data,
        to item: FSItem,
        at offset: off_t,
        replyHandler: @escaping (Int, (any Error)?) -> Void
    ) {
        var temporaryHandle: UInt64?
        Task {
            do {
                guard offset >= 0 else { throw posixError(22, "negative write offset") }
                let (handle, temporary) = try await self.handle(for: item, flags: "r+")
                temporaryHandle = temporary ? handle : nil
                let count = try await self.worker.write(
                    handle: handle,
                    offset: UInt64(offset),
                    data: Array(contents)
                )
                if temporary { try? await self.worker.close(handle); temporaryHandle = nil }
                replyHandler(count, nil)
            } catch {
                if let temporaryHandle { try? await self.worker.close(temporaryHandle) }
                replyHandler(0, self.nsError(error))
            }
        }
    }

    private func handle(for item: FSItem, flags: String) async throws -> (UInt64, Bool) {
        guard let rustItem = item as? MountRsFSItem else { throw posixError(22, "unknown FSItem") }
        if let handle = rustItem.handle {
            if flags == "r+" && !rustItem.openModes.contains(.write) {
                var retainedModes = rustItem.openModes
                retainedModes.insert(.write)
                try await worker.close(handle)
                rustItem.handle = nil
                rustItem.openModes = []
                do {
                    let reopened = try await worker.open(rustItem.path, flags: flags)
                    rustItem.handle = reopened.handle
                    rustItem.openModes = retainedModes
                    return (reopened.handle, false)
                } catch {
                    rustItem.handle = nil
                    rustItem.openModes = []
                    throw error
                }
            }
            return (handle, false)
        }
        return (try await worker.open(rustItem.path, flags: flags).handle, true)
    }

    private func apply(attributes: FSItem.SetAttributesRequest, to path: String) async throws {
        if attributes.wasAttributeConsumed(.mode) == false && attributes.isValid(.mode) {
            try await worker.chmod(path, mode: attributes.mode)
            attributes.consumedAttributes.insert(.mode)
        }
        if attributes.isValid(.uid) || attributes.isValid(.gid) {
            try await worker.chown(path, uid: attributes.isValid(.uid) ? attributes.uid : UInt32.max, gid: attributes.isValid(.gid) ? attributes.gid : UInt32.max)
            if attributes.isValid(.uid) { attributes.consumedAttributes.insert(.uid) }
            if attributes.isValid(.gid) { attributes.consumedAttributes.insert(.gid) }
        }
        if attributes.isValid(.size) {
            try await worker.truncate(path, length: attributes.size)
            attributes.consumedAttributes.insert(.size)
        }
        if attributes.isValid(.accessTime) || attributes.isValid(.modifyTime) {
            // utimes updates both values atomically. Preserve the value that
            // the caller did not request instead of turning an omitted field
            // into the Unix epoch.
            let current = try await worker.stat(path)
            let atime = attributes.isValid(.accessTime)
                ? milliseconds(attributes.accessTime)
                : current.atimeMS
            let mtime = attributes.isValid(.modifyTime)
                ? milliseconds(attributes.modifyTime)
                : current.mtimeMS
            try await worker.utimes(path, atimeMS: atime, mtimeMS: mtime)
            if attributes.isValid(.accessTime) { attributes.consumedAttributes.insert(.accessTime) }
            if attributes.isValid(.modifyTime) { attributes.consumedAttributes.insert(.modifyTime) }
        }
    }

    private func attributes(from stats: MountRsWorkerStats, wanted: FSItem.Attribute? = nil) -> FSItem.Attributes {
        let result = FSItem.Attributes()
        result.type = itemType(from: stats.mode)
        result.mode = stats.mode & 0o7777
        result.linkCount = UInt32(min(stats.nlink, UInt64(UInt32.max)))
        result.uid = stats.uid
        result.gid = stats.gid
        result.size = stats.size
        result.allocSize = stats.blocks.saturatingMultiply(stats.blksize)
        result.fileID = FSItem.Identifier(rawValue: stats.ino) ?? .invalid
        result.parentID = .parentOfRoot
        result.accessTime = timespec(milliseconds: stats.atimeMS)
        result.modifyTime = timespec(milliseconds: stats.mtimeMS)
        result.changeTime = timespec(milliseconds: stats.ctimeMS)
        result.birthTime = timespec(milliseconds: stats.birthtimeMS)
        return result
    }

    private func itemType(from mode: UInt32) -> FSItem.ItemType {
        switch mode & 0o170000 {
        case 0o040000: return .directory
        case 0o120000: return .symlink
        case 0o010000: return .fifo
        case 0o020000: return .charDevice
        case 0o060000: return .blockDevice
        case 0o140000: return .socket
        default: return .file
        }
    }

    private func path(of item: FSItem) throws -> String {
        guard let item = item as? MountRsFSItem else { throw posixError(22, "unknown FSItem") }
        return item.path
    }

    private func name(_ name: FSFileName) throws -> String {
        guard let value = name.string, !value.isEmpty, !value.contains("/"), !value.contains("\0") else {
            throw posixError(22, "filename is not valid UTF-8")
        }
        return value
    }

    private func join(_ parent: String, _ component: String) throws -> String {
        let component = try name(FSFileName(string: component))
        return parent == "/" ? "/" + component : parent + "/" + component
    }

    private func nsError(_ error: Error) -> NSError {
        if let error = error as? MountRsWorkerError { return error.asNSError() }
        return error as NSError
    }

    private func posixError(_ code: Int32, _ message: String) -> NSError {
        NSError(domain: NSPOSIXErrorDomain, code: Int(code), userInfo: [NSLocalizedDescriptionKey: message])
    }

    private func run<T>(
        replyHandler: @escaping (T?, (any Error)?) -> Void,
        operation: @escaping () async throws -> T
    ) {
        Task {
            do { replyHandler(try await operation(), nil) }
            catch { replyHandler(nil, self.nsError(error)) }
        }
    }

    private func run(
        replyHandler: @escaping ((any Error)?) -> Void,
        operation: @escaping () async throws -> Void
    ) {
        Task {
            do { try await operation(); replyHandler(nil) }
            catch { replyHandler(self.nsError(error)) }
        }
    }

    private func run<T, U>(
        replyHandler: @escaping (T?, U?, (any Error)?) -> Void,
        operation: @escaping () async throws -> (T, U)
    ) {
        Task {
            do {
                let result = try await operation()
                replyHandler(result.0, result.1, nil)
            } catch {
                replyHandler(nil, nil, self.nsError(error))
            }
        }
    }

    private func runVerifier(
        replyHandler: @escaping (FSDirectoryVerifier, (any Error)?) -> Void,
        operation: @escaping () async throws -> FSDirectoryVerifier
    ) {
        Task {
            do { replyHandler(try await operation(), nil) }
            catch { replyHandler(FSDirectoryVerifier(0), self.nsError(error)) }
        }
    }
}

private extension UInt64 {
    func saturatingSubtract(_ value: UInt64) -> UInt64 { self >= value ? self - value : 0 }
    func saturatingMultiply(_ value: UInt64) -> UInt64 {
        let (result, overflow) = multipliedReportingOverflow(by: value)
        return overflow ? UInt64.max : result
    }
}

private func timespec(milliseconds: Int64) -> timespec {
    timespec(
        tv_sec: Int(milliseconds / 1000),
        tv_nsec: Int((milliseconds % 1000) * 1_000_000)
    )
}

private func milliseconds(_ value: timespec) -> Int64 {
    Int64(value.tv_sec) * 1000 + Int64(value.tv_nsec) / 1_000_000
}
