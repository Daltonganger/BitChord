package com.music.bitchord.data.sources

import com.music.bitchord.data.model.Song
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class SourceSelectionWindowTest {
    private val target = TrackMatcher.Target("Get Lucky", "Daft Punk", 300)

    @Test fun oneThreeAndSixAddonsKeepPlayingWhenAnotherProviderFails() = runTest {
        for (count in listOf(1, 3, 6)) {
            val preferred = Provider("preferred-$count", 100, StreamFormat(codec = "flac"))
            val others = (1 until count).map { index ->
                Provider("other-$index", 10, StreamFormat(codec = "mp3", kbps = 128), fail = index % 2 == 1)
            }
            val result = SourceResolver.bestAcross(listOf(preferred) + others, target, StreamRequest.Lossless)
            assertEquals(preferred.configId, result?.first?.configId)
        }
    }

    @Test fun slowerLosslessBeatsFastTranscode() = runTest {
        val lower = Provider("lower", 10, StreamFormat(codec = "mp3", kbps = 128))
        val fast = Provider("fast", 20, StreamFormat(codec = "mp3", kbps = 320))
        val lossless = Provider("lossless", 900, StreamFormat(codec = "flac"))
        val result = SourceResolver.bestAcross(listOf(lower, fast, lossless), target, StreamRequest.Lossless)
        assertEquals("lossless", result?.first?.configId)
    }

    @Test fun deadlineReturnsBestCompletedAnswerAndCancelsHangingProvider() = runTest {
        val fast = Provider("fast", 20, StreamFormat(codec = "mp3", kbps = 320))
        val stuck = Provider("stuck", null, StreamFormat(codec = "flac"))
        val result = SourceResolver.bestAcross(listOf(fast, stuck), target, StreamRequest.Lossless)
        assertEquals("fast", result?.first?.configId)
        assertTrue(stuck.cancelled)
    }

    @Test fun fastModeStillReturnsWithoutWaitingForSlowProvider() = runTest {
        val fast = Provider("fast", 20, StreamFormat(codec = "aac", kbps = 256))
        val slow = Provider("slow", 900, StreamFormat(codec = "flac"))
        assertEquals("fast", SourceResolver.bestAcross(listOf(fast, slow), target, StreamRequest.Best)?.first?.configId)
        assertTrue(slow.cancelled)
        assertFalse(slow.completed)
    }

    @Test fun preparationWaitsForAddonOutsideStartupBudget() = runTest {
        val fast = Provider("fast", 20, StreamFormat(codec = "mp3", kbps = 320))
        val addon = Provider("addon", 7_000, StreamFormat(codec = "flac"))
        val result = SourceResolver.bestAcross(listOf(fast, addon), target, StreamRequest.Lossless,
            waitForAll = true, selectionWindowMs = 12_000)
        assertEquals("addon", result?.first?.configId)
    }

    @Test fun equalQualityRespectsConfiguredOrderEvenWhenFavouriteIsSlower() = runTest {
        val preferred = Provider("preferred", 900, StreamFormat(codec = "flac"))
        val fast = Provider("fast", 20, StreamFormat(codec = "flac"))
        assertEquals("preferred", SourceResolver.bestAcross(listOf(preferred, fast), target, StreamRequest.Lossless)?.first?.configId)
    }

    @Test fun cancellationStopsEveryOutstandingLookup() = runTest {
        val providers = listOf(Provider("one", null, StreamFormat()), Provider("two", null, StreamFormat()))
        val job = async { SourceResolver.bestAcross(providers, target, StreamRequest.Lossless) }
        delay(10)
        job.cancel()
        job.join()
        assertTrue(job.isCancelled)
        assertTrue(providers.all { it.cancelled })
    }

    @Test fun emptyAndUnansweredWindowsDoNotInventAStream() = runTest {
        assertNull(SourceResolver.bestAcross(emptyList(), target, StreamRequest.Lossless))
        val stuck = Provider("stuck", null, StreamFormat())
        assertNull(SourceResolver.bestAcross(listOf(stuck), target, StreamRequest.Lossless))
        assertTrue(stuck.cancelled)
    }

    private class Provider(
        override val configId: String,
        private val latencyMs: Long?,
        private val format: StreamFormat,
        private val fail: Boolean = false,
    ) : MusicSource {
        override val kind = SourceKind.ADDON
        override val displayName = configId
        var cancelled = false
        var completed = false
        override suspend fun health(): SourceHealth = SourceHealth.Ok()
        override suspend fun search(query: String, limit: Int, waitForAll: Boolean, request: StreamRequest?): List<Song> =
            listOf(Song(videoId = configId, title = "Get Lucky", artist = "Daft Punk", durationText = "5:00"))
        override suspend fun stream(trackId: String, request: StreamRequest): SourceStream {
            try {
                if (latencyMs == null) awaitCancellation() else delay(latencyMs)
                check(!fail) { "Provider unavailable" }
                completed = true
                return SourceStream(url = "https://example.test/$configId", format = format)
            } finally {
                if (!completed) cancelled = true
            }
        }
    }
}
