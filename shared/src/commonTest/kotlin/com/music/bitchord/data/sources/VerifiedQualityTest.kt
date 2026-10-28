package com.music.bitchord.data.sources

import com.music.bitchord.data.model.Song
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import kotlin.test.*

class VerifiedQualityTest {
    @Test fun decoderEvidenceReplacesFalseLosslessAndBitrateClaims() = runTest {
        StreamVerification.register(object : StreamVerification.Verifier {
            override fun verify(url: String, headers: Map<String, String>, callback: StreamVerification.Callback) {
                assertEquals("secret-test-header", headers["Authorization"])
                callback.onResult("mp3", 128, 44_100, 16)
                callback.onResult("flac", 9999, 192_000, 24) // A duplicate callback must be ignored.
            }
        })
        try {
            val report = SourceResolutionReport()
            val result = withContext(report) { StreamVerification.verify(SourceStream("https://example.test/audio", StreamFormat("flac", 9999, 192_000, 24), mapOf("Authorization" to "secret-test-header"))) }
            assertNotNull(result)
            assertEquals("mp3", result.format.codec)
            assertEquals(128, result.format.kbps)
            assertTrue(result.format.verified)
            assertFalse(result.format.isLossless == true)
            assertTrue(report.summary()!!.contains("compressed"))
            assertEquals("secret-test-header", result.headers["Authorization"])
        } finally { StreamVerification.register(null) }
    }

    @Test fun unverifiableAndSilentCallbacksCannotWin() = runTest {
        StreamVerification.register(object : StreamVerification.Verifier {
            override fun verify(url: String, headers: Map<String, String>, callback: StreamVerification.Callback) { }
        })
        try {
            val report = SourceResolutionReport()
            assertNull(withContext(report) { StreamVerification.verify(SourceStream("https://example.test/stalled", StreamFormat("flac"))) })
            assertTrue(report.summary()!!.contains("verified"))
        } finally { StreamVerification.register(null) }
    }

    @Test fun invalidDecoderResultsAreRejected() = runTest {
        StreamVerification.register(object : StreamVerification.Verifier {
            override fun verify(url: String, headers: Map<String, String>, callback: StreamVerification.Callback) { callback.onResult("flac", 1400, 0, 24) }
        })
        try { assertNull(StreamVerification.verify(SourceStream("https://example.test/bad"))) }
        finally { StreamVerification.register(null) }
    }

    @Test fun crossCodecBitratesDoNotInventAnUpgrade() {
        val opus = StreamFormat("opus", 146)
        val aac = StreamFormat("aac", 320)
        assertFalse(SourceResolver.isBetter(aac, opus))
        assertFalse(SourceResolver.isBetter(opus, aac))
        assertFalse(SourceResolver.worthSwapping(aac, opus))
        assertFalse(SourceResolver.worthSwapping(StreamFormat(null, 1000), opus))
        assertTrue(SourceResolver.worthSwapping(StreamFormat("audio/aac", 320), StreamFormat("aac", 128)))
    }

    @Test fun losslessFidelityUsesDecodedPrecisionRatherThanCompressionRate() {
        val cd = StreamFormat("flac", 1600, 44_100, 16, verified = true)
        val hiRes = StreamFormat("flac", 1000, 96_000, 24, verified = true)
        assertTrue(SourceResolver.isBetter(hiRes, cd))
        assertFalse(SourceResolver.isBetter(cd, hiRes))
        assertTrue(SourceResolver.worthSwapping(hiRes, cd))
        assertFalse(SourceResolver.worthSwapping(cd, cd))
        assertFalse(SourceResolver.worthSwapping(StreamFormat("aac", 1000), cd))
        assertFalse(SourceResolver.isBetter(cd.copy(kbps = 9999), cd))
    }

    @Test fun verifiedLosslessWinsOverAProviderThatOnlyClaimsLossless() = runTest {
        StreamVerification.register(object : StreamVerification.Verifier {
            override fun verify(url: String, headers: Map<String, String>, callback: StreamVerification.Callback) {
                if (url.endsWith("liar")) callback.onResult("mp3", 128, 44_100, 16)
                else callback.onResult("flac", 900, 96_000, 24)
            }
        })
        try {
            val selected = SourceResolver.bestAcross(listOf(ClaimingProvider("liar", 10), ClaimingProvider("actual", 100)),
                TrackMatcher.Target("Example", "Artist", 300), StreamRequest.Lossless)
            assertEquals("actual", selected?.first?.configId)
            assertEquals(24, selected?.second?.format?.bitDepth)
            assertTrue(selected?.second?.format?.verified == true)
        } finally { StreamVerification.register(null) }
    }

    private class ClaimingProvider(override val configId: String, val latency: Long) : MusicSource {
        override val kind = SourceKind.ADDON
        override val displayName = configId
        override suspend fun health(): SourceHealth = SourceHealth.Ok()
        override suspend fun search(query: String, limit: Int, waitForAll: Boolean, request: StreamRequest?) =
            listOf(Song(videoId = configId, title = "Example", artist = "Artist", durationText = "5:00"))
        override suspend fun stream(trackId: String, request: StreamRequest): SourceStream {
            delay(latency)
            return SourceStream("https://example.test/$configId", StreamFormat("flac", 9999, 192_000, 24))
        }
    }

    @Test fun reportsStayLocalToTheirLookupAndContainNoStreamUrls() = runTest {
        val one = SourceResolutionReport()
        val two = SourceResolutionReport()
        withContext(one) { SourceResolutionReport.record("timeout") }
        withContext(two) { SourceResolutionReport.record("provider_lossy") }
        assertTrue(one.summary()!!.contains("window"))
        assertTrue(two.summary()!!.contains("compressed"))
        assertFalse(one.summary()!!.contains("https://"))
    }
}
