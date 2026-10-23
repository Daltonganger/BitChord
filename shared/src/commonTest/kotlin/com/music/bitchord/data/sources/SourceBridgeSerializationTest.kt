package com.music.bitchord.data.sources

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SourceBridgeSerializationTest {
    @Test
    fun firstBackgroundStreamAnswerIsEncodedByTheInitializedBridge() = runTest {
        val expected = SourceStream(
            url = "https://media.invalid/track.flac",
            format = StreamFormat(codec = "flac", kbps = 1411, sampleRateHz = 44100, bitDepth = 16),
            headers = mapOf("Authorization" to "synthetic"),
            durationSec = 180,
            sourceConfigId = "first-addon",
        )
        val encoded = withContext(Dispatchers.Default) { SourceResolverBridge.encodeStream(expected) }
        val document = Json.decodeFromString<StreamDocument>(encoded)
        assertEquals(expected.url, document.url)
        assertEquals(expected.headers, document.headers)
        assertEquals(180, document.durationSec)
        assertEquals("first-addon", document.sourceConfigId)
        assertEquals(44100, document.format.sampleRateHz)
        assertTrue(document.format.isLossless == true)
        assertFalse(document.belowRequest)
    }

    @Test
    fun threeAddonsCanReturnConcurrentPlaybackAnswersRepeatedly() = runTest {
        val answers = (0 until 300).map { index ->
            async(Dispatchers.Default) {
                val addon = "addon-${index % 3}"
                val url = "https://media.invalid/$index.flac"
                val encoded = SourceResolverBridge.encodeStream(SourceStream(
                    url = url, format = StreamFormat(codec = "flac"), sourceConfigId = addon,
                ))
                val answer = Json.decodeFromString<StreamDocument>(encoded)
                assertEquals(url, answer.url)
                assertEquals(addon, answer.sourceConfigId)
                assertTrue(answer.format.isLossless == true)
                answer
            }
        }.awaitAll()
        assertEquals(300, answers.size)
    }

    @Test
    fun unknownMetadataAndBelowRequestSurviveTheBridge() {
        val encoded = SourceResolverBridge.encodeStream(SourceStream(
            url = "https://media.invalid/unknown", belowRequest = true,
        ))
        val answer = Json.decodeFromString<StreamDocument>(encoded)
        assertEquals(null, answer.format.codec)
        assertEquals(null, answer.format.isLossless)
        assertTrue(answer.belowRequest)
        assertEquals(emptyMap(), answer.headers)
    }
}
