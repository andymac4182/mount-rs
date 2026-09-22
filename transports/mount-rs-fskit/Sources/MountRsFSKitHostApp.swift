import AppKit
import Foundation

/// Minimal containing application for the FSKit module.
///
/// FSKit modules are delivered as app extensions. The application intentionally
/// has no filesystem logic; it exists to embed and install the signed appex.
@main
final class MountRsFSKitHostApp: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}
