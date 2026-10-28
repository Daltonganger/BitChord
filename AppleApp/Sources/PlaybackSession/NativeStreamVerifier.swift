import Foundation
import BitChordShared

/// Bounded worker admission: slow providers cannot create an unbounded probe queue.
final class NativeStreamVerifier: StreamVerificationVerifier, @unchecked Sendable {
    static let shared = NativeStreamVerifier()
    private let slots = DispatchSemaphore(value: 3)
    private let probe = PlayerEngine()

    static func verifyCachedFile(_ path: String) -> VerifiedAudioSourceRec? {
        try? shared.probe.verifyAudioSource(source: path, headers: nil)
    }

    static func install() { StreamVerification.shared.register(verifier: shared) }

    func verify(url: String, headers: [String: String], callback: StreamVerificationCallback) {
        guard slots.wait(timeout: .now()) == .success else {
            callback.onResult(codec: nil, kbps: nil, sampleRateHz: nil, bitDepth: nil)
            return
        }
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            defer { slots.signal() }
            do {
                let result = try probe.verifyAudioSource(source: url, headers: headers)
                callback.onResult(
                    codec: result.format.codec,
                    kbps: result.estimatedKbps.map { KotlinInt(value: Int32(clamping: $0)) },
                    sampleRateHz: KotlinInt(value: Int32(clamping: result.format.sampleRate)),
                    bitDepth: KotlinInt(value: Int32(clamping: result.format.bitDepth))
                )
            } catch {
                callback.onResult(codec: nil, kbps: nil, sampleRateHz: nil, bitDepth: nil)
            }
        }
    }
}
