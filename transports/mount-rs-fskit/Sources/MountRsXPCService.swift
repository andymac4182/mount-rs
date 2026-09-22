import Dispatch
import Foundation
import XPC

@available(macOS 14.0, *)
@main
struct MountRsXPCService {
    static func main() {
        guard let configuration = MountRsWorkerConfiguration.fromEnvironment() else {
            fputs("mount-rs XPC service has an invalid backend configuration\n", stderr)
            exit(2)
        }
        guard let worker = MountRsRustWorker(configuration: configuration) else {
            fputs("mount-rs XPC service could not initialize its backend\n", stderr)
            exit(2)
        }
        let queue = DispatchQueue(label: "com.andymac4182.mount-rs.worker")

        do {
            let listener = try XPCListener(
                service: MountRsXPCServiceName,
                targetQueue: queue,
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
            // A listener created with `.none` is active immediately. Calling
            // `activate()` here would be an API misuse; an actual containing
            // app/XPC host owns launch and lifetime of this service process.
            withExtendedLifetime(listener) {
                dispatchMain()
            }
        } catch {
            fputs("mount-rs XPC service failed to start: \(error)\n", stderr)
            exit(1)
        }
    }
}
