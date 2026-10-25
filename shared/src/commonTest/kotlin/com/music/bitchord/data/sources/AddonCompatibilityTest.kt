package com.music.bitchord.data.sources

import com.music.bitchord.data.sources.addon.AddonManifest
import com.music.bitchord.data.sources.addon.AddonStream
import com.music.bitchord.data.sources.addon.AddonTrack
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AddonCompatibilityTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test fun resourcesOnlyManifestIsPlayable() {
        val manifest = json.decodeFromString<AddonManifest>("""{
            "id":"dev.akira.bitchord-hires",
            "resources":["search","stream"],
            "checkValidLossless":1,"allowDownloads":1
        }""")
        assertTrue(manifest.isPlayable)
        assertTrue(manifest.downloadsAllowed)
        assertTrue(manifest.requiresLosslessOutput)
    }

    @Test fun resourceManifestRetainsDownloadAndOutputPolicies() {
        for (value in listOf("false", "0", "\"false\"", "\"0\"")) {
            val manifest = json.decodeFromString<AddonManifest>("""{
                "id":"dev.playback-only", "resources":["search","stream"],
                "allowDownloads":$value, "checkValidLossless":"true"
            }""")
            assertTrue(manifest.isPlayable)
            assertFalse(manifest.downloadsAllowed)
            assertTrue(manifest.requiresLosslessOutput)
        }
        val legacy = json.decodeFromString<AddonManifest>(
            """{"id":"dev.legacy","search":{},"stream":{}}""")
        assertTrue(legacy.downloadsAllowed)
        assertFalse(legacy.requiresLosslessOutput)
    }

    @Test fun absentStreamIsRejected() {
        val manifest = json.decodeFromString<AddonManifest>(
            """{"id":"dev.test","resources":["search"]}""")
        assertFalse(manifest.isPlayable)
    }

    @Test fun explicitEndpointObjectsStillWork() {
        val manifest = json.decodeFromString<AddonManifest>(
            """{"id":"dev.legacy","search":{},"stream":{}}""")
        assertTrue(manifest.isPlayable)
    }

    @Test fun upstreamTrackAliasesDecode() {
        val track = json.decodeFromString<AddonTrack>("""{
            "id":"qb:123", "title":"Song", "artist":"Artist",
            "duration":242, "artworkURL":"https://example.org/art.jpg",
            "format":"flac", "audioQuality":"HI_RES_LOSSLESS"
        }""")
        assertEquals(242, track.effectiveDurationSec)
        assertEquals("https://example.org/art.jpg", track.effectiveArtworkUrl)
        assertEquals("flac", track.effectiveCodec)
        assertTrue(track.claimsLossless)
    }

    @Test fun qualityAliasCountsAsLosslessOnlyWithoutContradictingCodec() {
        val hires = json.decodeFromString<AddonTrack>(
            """{"audioQuality":"HI_RES_LOSSLESS"}""")
        assertTrue(hires.claimsLossless)
        val mp3 = json.decodeFromString<AddonTrack>(
            """{"audioQuality":"HI_RES_LOSSLESS", "codec":"mp3"}""")
        assertFalse(mp3.claimsLossless)
    }

    @Test fun streamUnitsAndEncryptionAreDecoded() {
        val stream = json.decodeFromString<AddonStream>("""{
            "url":"https://example.org/music.flac", "codec":"flac",
            "sampleRate":192000,"bitDepth":24,"bitrate":4610250,
            "encrypted":true,"manifest":"none"
        }""")
        assertEquals(192000, stream.effectiveSampleRateHz)
        assertEquals(4610, stream.effectiveBitrateKbps)
        assertTrue(stream.isEncrypted)
    }

    @Test fun aliasesReachTheDisplayedSong() {
        val track = json.decodeFromString<AddonTrack>("""{
            "id":"test", "title":"Song", "duration":242.8,
            "albumArtworkURL":"https://example.org/album.jpg", "format":"flac"
        }""")
        val song = track.asRow().toSong("addon")
        assertEquals("4:02", song.durationText)
        assertEquals("https://example.org/album.jpg", song.thumbnailUrl)
        assertEquals("FLAC", song.sourceQuality)
    }

    @Test fun normalizedStreamMetadataReachesTheResolver() {
        val answer = json.decodeFromString<AddonStream>("""{
            "url":"https://example.org/music", "format":"flac",
            "sampleRate":44.1, "bitrate":1411, "bitDepth":16,
            "headers":{"Authorization":"test"}
        }""")
        val stream = assertNotNull(answer.toSourceStream("addon", StreamRequest.Lossless))
        assertEquals("flac", stream.format.codec)
        assertEquals(44100, stream.format.sampleRateHz)
        assertEquals(1411, stream.format.kbps)
        assertEquals(16, stream.format.bitDepth)
        assertEquals(mapOf("Authorization" to "test"), stream.headers)
        assertFalse(stream.belowRequest)
        assertTrue(assertNotNull(answer.toSourceStream("addon", StreamRequest.Capped(320))).belowRequest)
    }

    @Test fun canonicalFieldsTakePrecedenceOverAliases() {
        val stream = json.decodeFromString<AddonStream>("""{
            "codec":"aac", "format":"flac", "sampleRateHz":48000, "sampleRate":192000,
            "bitrateKbps":256, "bitrate":4610250
        }""")
        assertEquals("aac", stream.effectiveCodec)
        assertEquals(48000, stream.effectiveSampleRateHz)
        assertEquals(256, stream.effectiveBitrateKbps)
        val track = json.decodeFromString<AddonTrack>("""{
            "durationSec":60, "duration":242.8, "artworkUrl":"primary", "artworkURL":"alias",
            "codec":"mp3", "format":"flac", "lossless":true
        }""")
        assertEquals(60, track.effectiveDurationSec)
        assertEquals("primary", track.effectiveArtworkUrl)
        assertFalse(track.claimsLossless)
        assertTrue(track.asRow().rank(StreamRequest.Lossless) < 10000)
    }

    @Test fun encryptedAnswersNeverBecomePlayableStreams() {
        for (value in listOf("true", "\"widevine\"", "\"aes-128\"", "{}")) {
            val answer = json.decodeFromString<AddonStream>(
                """{"url":"https://example.org/audio", "encrypted":$value}""")
            assertTrue(answer.isEncrypted)
            assertNull(answer.toSourceStream("addon", StreamRequest.Best))
        }
    }

    @Test fun unencryptedAndLegacyAnswersRemainPlayable() {
        for (value in listOf("false", "null", "\"false\"", "\"none\"", "\"\"")) {
            val answer = json.decodeFromString<AddonStream>(
                """{"url":"https://example.org/audio", "encrypted":$value}""")
            assertFalse(answer.isEncrypted)
            assertNotNull(answer.toSourceStream("addon", StreamRequest.Best))
        }
        assertNotNull(AddonStream(url = "https://example.org/audio")
            .toSourceStream("addon", StreamRequest.Best))
        assertNull(AddonStream().toSourceStream("addon", StreamRequest.Best))
    }

    @Test fun numericAliasesAcceptDecimalsAndRejectInvalidValues() {
        val answer = json.decodeFromString<AddonStream>("""{
            "sampleRate":192000.0, "bitrate":320000.0
        }""")
        assertEquals(192000, answer.effectiveSampleRateHz)
        assertEquals(320, answer.effectiveBitrateKbps)
        assertEquals(9216, AddonStream(bitrate = 9216.0).effectiveBitrateKbps)
        assertNull(AddonStream(sampleRate = -1.0, bitrate = 0.0).effectiveSampleRateHz)
        assertNull(AddonStream(bitrate = 0.0).effectiveBitrateKbps)
    }

    @Test fun mixedCapabilityDeclarationsAreAccepted() {
        val manifest = json.decodeFromString<AddonManifest>(
            """{"resources":["SEARCH"],"stream":{}}""")
        assertTrue(manifest.isPlayable)
        assertFalse(AddonManifest().isPlayable)
    }
}
