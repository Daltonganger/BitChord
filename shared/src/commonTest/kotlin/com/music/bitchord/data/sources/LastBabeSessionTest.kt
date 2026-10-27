package com.music.bitchord.data.sources

import com.music.bitchord.data.crypto.Sha256
import com.music.bitchord.data.sources.addon.AddonException
import com.music.bitchord.data.sources.addon.LastBabeSessionProvider
import com.music.bitchord.data.sources.addon.AddonSession
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

// The real proof solver runs on Dispatchers.Default; use real deadlines here.
class LastBabeSessionTest {
    private class Fixture {
        var now = 100_000L
        var stored: String? = null
        var minted = 0
        var badHost = false
        var unavailable = false
        var rickroll = false
        val root = LastBabeSessionProvider.ORIGIN
        fun client() = AddonSession(
            LastBabeSessionProvider(
                get = {
                    assertEquals("$root/challenge", it)
                    """{"challenge":"test-challenge","difficulty":1,"rickroll":$rickroll}"""
                },
                post = { url, body ->
                    assertEquals("$root/mint", url)
                    val proof = Json.parseToJsonElement(body).jsonObject
                    val digest = Sha256.digest((proof.getValue("challenge").jsonPrimitive.content +
                        proof.getValue("nonce").jsonPrimitive.content).encodeToByteArray())
                    assertEquals(0, digest[0].toInt() and 0xf0)
                    if (unavailable) error("temporary outage")
                    minted++
                    val host = if (badHost) "https://elsewhere.example" else root
                    """{"manifestUrl":"$host/e/${now + 172800}/token$minted/manifest.json","expiresAt":${now + 172800}}"""
                },
            ),
            read = { stored }, write = { stored = it }, now = { now },
        )
    }

    @Test fun onlyTheKnownProviderAndTokenPathsAreManaged() {
        val root = LastBabeSessionProvider.ORIGIN
        assertTrue(LastBabeSessionProvider.supports(root))
        assertTrue(LastBabeSessionProvider.supports("$root:443"))
        assertTrue(LastBabeSessionProvider.supports("$root/e/123456/token"))
        for (url in listOf("$root.evil.example", "$root/other-config", "$root/e/1/t?secret=x",
            "https://user@lastbabe.velixir.run/e/1/t", "http://lastbabe.velixir.run", "$root:444/e/1/t")) {
            assertFalse(LastBabeSessionProvider.supports(url), url)
        }
    }

    @Test fun rootAndPreviouslyMintedAddressesRenewAndPreserveEncodedEndpoints() = runBlocking {
        val f = Fixture(); val client = f.client()
        val suffix = "/stream/id%2Fpart?quality=LOSSLESS"
        val result = client.rewrite(f.root + suffix, f.root)
        assertEquals("${f.root}/e/272800/token1$suffix", result)
        val old = "${f.root}/e/1/expired"
        assertEquals(result, client.rewrite(old + suffix, old))
        assertEquals(1, f.minted)
    }

    @Test fun storedLeaseSurvivesAnotherClientAndCorruptStorageCanRecover() = runBlocking {
        val f = Fixture()
        val first = f.client().rewrite(f.root + "/manifest.json", f.root)
        assertEquals(first, f.client().rewrite(f.root + "/manifest.json", f.root))
        assertEquals(1, f.minted)
        f.stored = "not json"
        assertNotEquals(first, f.client().rewrite(f.root + "/manifest.json", f.root))
        assertEquals(2, f.minted)
    }

    @Test fun concurrentFirstRequestsShareOneMint() = runBlocking {
        val f = Fixture(); val client = f.client()
        val answers = (1..20).map { async { client.rewrite(f.root + "/search?q=test", f.root) } }.awaitAll()
        assertEquals(1, answers.toSet().size)
        assertEquals(1, f.minted)
    }

    @Test fun renewsBeforeExpiryWithoutChangingTheSavedConfiguration() = runBlocking {
        val f = Fixture(); val client = f.client()
        val before = client.rewrite(f.root + "/manifest.json", f.root)
        f.now += 172800 - 599
        val after = client.rewrite(f.root + "/manifest.json", f.root)
        assertNotEquals(before, after)
        assertEquals(2, f.minted)
        assertTrue(after.startsWith(f.root))
    }

    @Test fun rejectedTokenRenewsOnceAndConcurrentOldRejectionsReuseTheNewToken() = runBlocking {
        val f = Fixture(); val client = f.client()
        val old = client.rewrite(f.root + "/manifest.json", f.root)
        val fresh = client.rewrite(f.root + "/manifest.json", f.root, old)
        assertNotEquals(old, fresh)
        assertEquals(fresh, client.rewrite(f.root + "/manifest.json", f.root, old))
        assertEquals(2, f.minted)
    }

    @Test fun failuresDoNotPersistOrPoisonTheNextAttempt() = runBlocking {
        val f = Fixture(); val client = f.client(); f.unavailable = true
        assertTrue(runCatching { client.rewrite(f.root + "/manifest.json", f.root) }.isFailure)
        assertEquals(null, f.stored)
        f.unavailable = false
        f.now += 31
        assertTrue(client.rewrite(f.root + "/manifest.json", f.root).endsWith("/manifest.json"))
        assertEquals(1, f.minted)
    }

    @Test fun unsafeMintedOriginIsRejectedWithoutStoringCredentials() = runBlocking {
        val f = Fixture(); f.badHost = true
        assertFailsWith<com.music.bitchord.data.sources.addon.AddonUnavailable> { f.client().rewrite(f.root + "/manifest.json", f.root) }
        assertEquals(null, f.stored)
    }

    @Test fun providerNonMusicModeDoesNotMintOrPretendToServeTheRequestedTrack() = runBlocking {
        val f = Fixture(); f.rickroll = true
        assertFailsWith<com.music.bitchord.data.sources.addon.AddonUnavailable> { f.client().rewrite(f.root + "/manifest.json", f.root) }
        assertEquals(0, f.minted)
        assertEquals(null, f.stored)
    }

    @Test fun ordinaryConfiguredPathsAreNeverRewrittenOrMinted() = runBlocking {
        val f = Fixture()
        val configured = "https://example.org/u/private-config"
        val url = "$configured/stream/track"
        assertEquals(url, f.client().rewrite(url, configured))
        assertEquals(0, f.minted)
    }

    @Test fun oddAndEvenProofDifficultiesAreSatisfiedAndInvalidWorkIsBounded() = runBlocking {
        for (difficulty in 1..2) {
            val nonce = LastBabeSessionProvider.solve("fixture", difficulty)
            val digest = Sha256.digest(("fixture" + nonce).encodeToByteArray())
            assertEquals(0, if (difficulty == 1) digest[0].toInt() and 0xf0 else digest[0].toInt())
        }
        assertFailsWith<AddonException> { LastBabeSessionProvider.solve("fixture", 8) }
        Unit
    }
}
