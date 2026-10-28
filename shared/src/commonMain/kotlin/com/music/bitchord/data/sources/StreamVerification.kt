package com.music.bitchord.data.sources

import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.concurrent.atomics.AtomicReference
import kotlin.concurrent.atomics.ExperimentalAtomicApi
import kotlin.coroutines.resume

/** Host decoder evidence, collected before a provider answer participates in ranking. */
@OptIn(ExperimentalAtomicApi::class)
object StreamVerification {
    interface Verifier {
        fun verify(url: String, headers: Map<String, String>, callback: Callback)
    }
    fun interface Callback {
        fun onResult(codec: String?, kbps: Int?, sampleRateHz: Int?, bitDepth: Int?)
    }
    private val native = AtomicReference<Verifier?>(null)
    fun register(verifier: Verifier?) { native.store(verifier) }

    internal suspend fun verify(stream: SourceStream): SourceStream? {
        val verifier = native.load() ?: return stream // Other hosts retain their own playback verification.
        if (PlaybackCodecCapabilities.canRenderDolby(stream.format.codec, stream.headers)) return stream
        val format = withTimeoutOrNull(1_800) {
            suspendCancellableCoroutine { continuation ->
                val answered = AtomicReference(false)
                verifier.verify(stream.url, stream.headers) { codec, kbps, rate, depth ->
                    if (answered.compareAndSet(false, true) && continuation.isActive) {
                        continuation.resume(if (codec.isNullOrBlank() || rate == null || rate <= 0) null else
                            StreamFormat(codec, kbps?.takeIf { it > 0 }, rate, depth?.takeIf { it > 0 }, verified = true))
                    }
                }
            }
        }
        if (format == null) {
            SourceResolutionReport.record("verification_failed")
            return null
        }
        if (stream.format.isLossless == true && format.isLossless != true) {
            SourceResolutionReport.record("provider_lossy")
        }
        return stream.copy(format = format)
    }
}
