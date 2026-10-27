package com.music.bitchord.data.sources

import com.music.bitchord.data.crypto.Sha256
import com.music.bitchord.data.sources.addon.AddonException
import com.music.bitchord.data.sources.addon.AddonRefresh
import com.music.bitchord.data.sources.addon.AddonRefreshSession
import com.music.bitchord.data.sources.module.SharedCalls
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.minutes

class AddonRefreshTest {
    private class Fixture(val host: String = "https://provider.example") {
        var now = 100_000L
        var stored: String? = null
        var gets = 0
        var attempts = 0
        var cancelled = false
        var posts = 0
        var unavailable = false
        var document: String? = null
        var lifetime: Long? = 172800
        var target = "https://catalogue.example/u/config.v2%2Fopaque"
        val refresh = "$host/account/renew?key=private"
        fun response(): String = document ?: """{"manifestUrl":"$target/$gets/manifest.json"${lifetime?.let { ",\"expiresIn\":$it" }.orEmpty()}}"""
        fun client() = AddonRefreshSession(
            refreshUrl = refresh,
            get = {
                attempts++
                if (cancelled) throw CancellationException("cancelled")
                if (unavailable) error("outage with private response data")
                if (it == refresh) { gets++; response() }
                else {
                    assertEquals("$host/puzzle", it)
                    """{"challenge":"fixture","difficulty":1}"""
                }
            },
            post = { address, body ->
                assertEquals("$host/issue", address)
                val proof = Json.parseToJsonElement(body).jsonObject
                val digest = Sha256.digest((proof.getValue("challenge").jsonPrimitive.content +
                    proof.getValue("nonce").jsonPrimitive.content).encodeToByteArray())
                assertEquals(0, digest[0].toInt() and 0xf0)
                posts++
                """{"addonUrl":"$target/issued$posts","expiresAt":${now + 172800}}"""
            },
            read = { stored }, write = { stored = it }, now = { now },
        )
    }

    @Test fun freshInstallDoesNotIncludeAnyAddonOrProviderDirectory() {
        val sources = SourceRegistry.sourcesForInit(emptyList(), forceJioSaavnOff = false)
        assertFalse(sources.any { it.kind == SourceKind.ADDON })
        assertTrue(sources.all { !it.refreshEnabled && it.refreshUrl.isEmpty() })
    }

    @Test fun refreshIsOffForExistingAndNewSourcesByDefault() {
        val config = Json.decodeFromString<SourceConfig>("""{"id":"old","kind":"ADDON","baseUrl":"https://example.org/u/keep"}""")
        assertFalse(config.refreshEnabled)
        assertEquals("", config.refreshUrl)
    }

    @Test fun noConfiguredRefreshLinkLeavesNormalAddonRequestsUntouched() = runTest {
        val url = "https://any.example/u/original/stream/id"
        assertEquals(url, AddonRefresh.rewrite(url, "https://any.example/u/original", null))
    }

    @Test fun jsonRenewalWorksAcrossUnrelatedProvidersAndKeepsOpaqueConfigurationPaths() = runTest {
        for (origin in listOf("https://first.example", "https://second.example:8443")) {
            val f = Fixture(origin)
            val original = "https://old.example/u/previous"
            val suffix = "/stream/track%2Fpart?quality=LOSSLESS"
            val result = f.client().rewrite(original + suffix, original)
            assertEquals("${f.target}/1$suffix", result)
            assertEquals(1, f.gets)
        }
    }

    @Test fun leaseSurvivesAnotherClientAndConcurrentRequestsShareOneRenewal() = runTest {
        val f = Fixture(); val client = f.client(); val base = "https://old.example"
        val answers = (1..20).map { async { client.rewrite("$base/search?q=test", base) } }.awaitAll()
        assertEquals(1, answers.toSet().size)
        assertEquals(1, f.gets)
        assertEquals(answers.first(), f.client().rewrite("$base/search?q=test", base))
        assertEquals(1, f.gets)
    }

    @Test fun shortAndLongLifetimesRefreshBeforeTheirOwnExpiry() = runTest {
        for (lifetime in listOf(30L, 172800L)) {
            val f = Fixture(); f.lifetime = lifetime; val client = f.client(); val base = "https://old.example"
            val before = client.rewrite("$base/manifest.json", base)
            f.now += lifetime - minOf(600, lifetime / 10) + 1
            assertNotEquals(before, client.rewrite("$base/manifest.json", base))
            assertEquals(2, f.gets)
        }
    }

    @Test fun renewedLeaseInvalidatesCachedAnswersEvenWhenItsBaseUrlStaysTheSame() = runTest {
        val f = Fixture()
        f.document = """{"baseUrl":"${f.target}","expiresIn":30}"""
        val client = f.client()
        val base = "https://old.example"
        val cache = SharedCalls<Int>(5.minutes, backgroundScope)
        var fetches = 0
        suspend fun answer(): Int = cache.get(client.cacheIdentity(base), { "test stream" }) {
            Result.success(++fetches)
        }.getOrThrow()

        val identity = client.cacheIdentity(base)
        assertEquals(identity, f.client().cacheIdentity(base))
        assertEquals(1, answer())
        assertEquals(1, answer())
        f.now += 28
        assertEquals(2, answer())
        assertEquals(2, answer())
        assertNotEquals(identity, client.cacheIdentity(base))
        assertEquals(2, f.gets)
    }

    @Test fun leasesWithoutExpiryAreReusedUntilRejectedRatherThanUsingAnInventedSchedule() = runTest {
        val f = Fixture(); f.lifetime = null; val client = f.client(); val base = "https://old.example"
        val old = client.rewrite("$base/manifest.json", base)
        f.now += 999999
        assertEquals(old, client.rewrite("$base/manifest.json", base))
        val fresh = client.rewrite("$base/manifest.json", base, old)
        assertNotEquals(old, fresh)
        assertEquals(fresh, client.rewrite("$base/manifest.json", base, old))
        assertEquals(2, f.gets)
    }

    @Test fun temporaryRenewalFailureKeepsAnUnexpiredLeaseButNotARejectedOne() = runTest {
        val f = Fixture(); val client = f.client(); val base = "https://old.example"
        val old = client.rewrite("$base/manifest.json", base)
        f.now += 172800 - 500; f.unavailable = true
        assertEquals(old, client.rewrite("$base/manifest.json", base))
        assertTrue(runCatching { client.rewrite("$base/manifest.json", base, old) }.isFailure)
        f.now += 600
        assertTrue(runCatching { client.rewrite("$base/manifest.json", base) }.isFailure)
    }

    @Test fun malformedStorageRecoversAndFailedRenewalDoesNotPoisonAnotherAttempt() = runTest {
        val f = Fixture(); f.stored = "not JSON"; val client = f.client(); val base = "https://old.example"
        f.unavailable = true
        val error = runCatching { client.rewrite("$base/manifest.json", base) }.exceptionOrNull()!!
        assertFalse(error.message.orEmpty().contains("private response"))
        f.unavailable = false
        f.now += 31
        assertTrue(client.rewrite("$base/manifest.json", base).startsWith(f.target))
    }

    @Test fun epochMillisecondsAndIsoExpiryAreAccepted() = runTest {
        for (expiry in listOf("1793290468000", "\"2026-10-29T12:00:00Z\"")) {
            val f = Fixture(); f.document = """{"baseUrl":"${f.target}","expiresAt":$expiry}"""
            assertEquals(f.target + "/manifest.json", f.client().rewrite("https://old.example/manifest.json", "https://old.example"))
        }
    }

    @Test fun expiredOrUnsafeResponsesAreNotPersisted() = runTest {
        for (response in listOf(
            """{"addonUrl":"http://unsafe.example/u/token"}""",
            """{"addonUrl":"https://user:secret@unsafe.example/u/token"}""",
            """{"addonUrl":"https://safe.example/u/token","expiresAt":1}""",
            """{"addonUrl":"https://safe.example/u/token","expiresIn":-1}""",
        )) {
            val f = Fixture(); f.document = response
            assertFailsWith<AddonException> { f.client().rewrite("https://old.example/manifest.json", "https://old.example") }
            assertEquals(null, f.stored)
        }
    }

    @Test fun descriptorCanAdvertiseArbitraryChallengeAndIssuingPaths() = runTest {
        val f = Fixture(); f.document = """{"challengeUrl":"/puzzle","refreshUrl":"/issue"}"""
        assertTrue(f.client().rewrite("https://old.example/manifest.json", "https://old.example").contains("/issued1/"))
        assertEquals(1, f.posts)
    }

    @Test fun renewalPageIsReadWithoutExecutingItsJavascript() = runTest {
        val f = Fixture(); f.document = """<script>
            crypto.subtle.digest('SHA-256', bytes);
            const c = await (await fetch('/puzzle')).json();
            const m = await fetch('/issue', {method:'POST',body:JSON.stringify({challenge:c.challenge,nonce:found})});
            throw new Error('this must never run');
        </script>"""
        assertTrue(f.client().rewrite("https://old.example/manifest.json", "https://old.example").contains("/issued1/"))
        assertEquals(1, f.posts)
    }

    @Test fun unsupportedLoginPagesAndCrossOriginChallengeDeclarationsRequireManualRenewal() = runTest {
        for (body in listOf("<html>Sign in to continue</html>",
            """{"challengeUrl":"https://other.example/puzzle","refreshUrl":"/issue"}""")) {
            val f = Fixture(); f.document = body
            assertFailsWith<AddonException> { f.client().rewrite("https://old.example/manifest.json", "https://old.example") }
            assertEquals(0, f.posts)
        }
    }

    @Test fun proofVerificationSupportsOddAndEvenDifficultiesAndRejectsUnboundedWork() = runTest {
        for (difficulty in 1..2) {
            val nonce = AddonRefreshSession.solve("fixture", difficulty)
            val digest = Sha256.digest(("fixture" + nonce).encodeToByteArray())
            assertEquals(0, if (difficulty == 1) digest[0].toInt() and 0xf0 else digest[0].toInt())
        }
        assertFailsWith<AddonException> { AddonRefreshSession.solve("fixture", 8) }
    }

    @Test fun earlyRenewalFailureReusesValidLeaseWithoutRepeatedNetworkWork() = runTest {
        val f = Fixture(); f.lifetime = 30; val client = f.client(); val base = "https://old.example"
        val old = client.rewrite("$base/manifest.json", base)
        f.now += 28; f.unavailable = true
        repeat(20) { assertEquals(old, client.rewrite("$base/manifest.json", base)) }
        assertEquals(2, f.attempts)
        f.now += 31; f.unavailable = false
        assertNotEquals(old, client.rewrite("$base/manifest.json", base))
        assertEquals(3, f.attempts)
    }

    @Test fun expiredLeaseFailuresShareANonSlidingCooldownAcrossConcurrentCallers() = runTest {
        val f = Fixture(); f.lifetime = 30; val client = f.client(); val base = "https://old.example"
        client.rewrite("$base/manifest.json", base)
        f.now += 31; f.unavailable = true
        (1..20).map { async {
            assertTrue(runCatching { client.rewrite("$base/manifest.json", base) }.isFailure)
        } }.awaitAll()
        assertEquals(2, f.attempts)
        f.now += 20
        assertTrue(runCatching { client.rewrite("$base/manifest.json", base) }.isFailure)
        assertEquals(2, f.attempts)
        f.now += 11; f.unavailable = false
        client.rewrite("$base/manifest.json", base)
        assertEquals(3, f.attempts)
    }

    @Test fun rejectedLeaseCannotBeReturnedByTheNextCallerDuringCooldown() = runTest {
        val f = Fixture(); f.lifetime = null; val client = f.client(); val base = "https://old.example"
        val old = client.rewrite("$base/manifest.json", base)
        f.unavailable = true
        assertTrue(runCatching { client.rewrite("$base/manifest.json", base, old) }.isFailure)
        assertTrue(runCatching { client.rewrite("$base/manifest.json", base) }.isFailure)
        assertEquals(2, f.attempts)
        f.now += 31; f.unavailable = false
        assertNotEquals(old, client.rewrite("$base/manifest.json", base))
    }

    @Test fun cancellationDoesNotPersistALeaseOrThrottleTheNextCaller() = runTest {
        val f = Fixture(); val client = f.client(); val base = "https://old.example"
        f.cancelled = true
        assertFailsWith<CancellationException> { client.rewrite("$base/manifest.json", base) }
        assertEquals(null, f.stored)
        f.cancelled = false
        client.rewrite("$base/manifest.json", base)
        assertEquals(2, f.attempts)
    }

}
