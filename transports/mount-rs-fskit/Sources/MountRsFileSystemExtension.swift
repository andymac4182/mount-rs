import ExtensionFoundation
import Foundation
import FSKit

@main
struct MountRsFileSystemExtension: UnaryFileSystemExtension {
    var fileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations {
        MountRsFileSystem()
    }
}
