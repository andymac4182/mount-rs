import Foundation

private final class RecordingTransport: MountRsXPCTransport {
    private let response: Data
    private(set) var lastRequest: Data?

    init(response: Data) {
        self.response = response
    }

    func send(_ request: Data) async throws -> Data {
        lastRequest = request
        return response
    }
}

@main
struct MountRsXPCDelegateTests {
    static func main() async throws {
        let requestID: UInt64 = 0x0102_0304_0506_0708
        let hello = try MountRsXPCFrame(kind: .hello, requestID: requestID)
        let expectedWire: [UInt8] = [
            0x4D, 0x52, 0x46, 0x53, 0x01, 0x00, 0x01, 0x00,
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x00, 0x00, 0x00, 0x00,
        ]
        precondition(Array(hello.encoded()) == expectedWire)
        let decodedHello = try MountRsXPCFrame.decode(hello.encoded())
        precondition(decodedHello == hello)

        let reply = try MountRsXPCFrame(
            kind: .reply,
            requestID: requestID,
            body: Data([0x01, 0x00])
        )
        let transport = RecordingTransport(response: reply.encoded())
        let delegate = MountRsXPCDelegate(transport: transport)
        let received = try await delegate.hello(requestID: requestID)
        precondition(received == reply)
        precondition(transport.lastRequest == hello.encoded())

        do {
            _ = try MountRsXPCFrame.decode(Data(expectedWire.dropLast()))
            preconditionFailure("truncated frame unexpectedly decoded")
        } catch MountRsXPCFrameError.tooShort(actual: MountRsXPCFrame.headerLength - 1) {
            // Expected malformed-frame rejection.
        }

        do {
            _ = try MountRsXPCFrame(
                kind: .operation,
                requestID: requestID,
                body: Data(repeating: 0, count: MountRsXPCFrame.maximumBodyLength + 1)
            )
            preconditionFailure("oversized frame unexpectedly constructed")
        } catch MountRsXPCFrameError.bodyTooLarge(let count) {
            precondition(count == MountRsXPCFrame.maximumBodyLength + 1)
        }

        let mismatchedReply = try MountRsXPCFrame(
            kind: .reply,
            requestID: requestID + 1
        )
        let mismatchedDelegate = MountRsXPCDelegate(
            transport: RecordingTransport(response: mismatchedReply.encoded())
        )
        do {
            _ = try await mismatchedDelegate.hello(requestID: requestID)
            preconditionFailure("mismatched response unexpectedly accepted")
        } catch MountRsXPCFrameError.requestIDMismatch(
            expected: requestID,
            actual: requestID + 1
        ) {
            // Expected request correlation rejection.
        }
    }
}
