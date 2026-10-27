package com.music.bitchord.data.sources

import com.music.bitchord.data.sources.addon.AddonLease
import com.music.bitchord.data.sources.addon.AddonSession
import com.music.bitchord.data.sources.addon.AddonSessionProvider
import com.music.bitchord.data.sources.addon.AddonUnavailable
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AddonSessionTest {
    private class Fixture(val root: String = "https://another-addon.example") : AddonSessionProvider {
        override val secretKey = root
        var now = 100_000L
        var stored: String? = null
        var calls = 0
        var failure: Exception? = null
        var hang = false
        var unsafe = false
        override fun supports(base: String) = base == root
        override fun acceptsLease(base: String) = base.startsWith("$root/session/")
        override suspend fun renew(): AddonLease {
            calls++
            delay(1) // Ensure concurrent callers actually suspend behind one renewal.
            failure?.let { throw it }
            if (hang) awaitCancellation()
            return AddonLease(if (unsafe) "https://attacker.example/token" else "$root/session/$calls", now + 3600)
        }
        fun client() = AddonSession(this, { stored }, { stored = it }, { now })
    }

    @Test fun genericLifecycleSupportsIndependentProvidersAndPersistsTheirOwnLeases() = runTest {
        val a = Fixture(); val b = Fixture("https://second-addon.example")
        val ca = a.client(); val cb = b.client()
        val results = (1..20).map { async { ca.rewrite(a.root + "/search?q=a%2Fb", a.root) } }.awaitAll()
        assertEquals(1, a.calls)
        assertEquals(1, results.toSet().size)
        assertEquals("${a.root}/session/1/search?q=a%2Fb", results.first())
        assertEquals("${b.root}/session/1/manifest.json", cb.rewrite(b.root + "/manifest.json", b.root))
        assertEquals(results.first(), a.client().rewrite(a.root + "/search?q=a%2Fb", a.root))
        assertEquals(1, a.calls)
        assertTrue(a.stored != b.stored)
    }

    @Test fun failuresAreSanitizedAndCooldownDoesNotSlideWithEveryCaller() = runTest {
        val f = Fixture(); val client = f.client()
        f.failure = IllegalStateException("secret-token-in-provider-error")
        val error = assertFailsWith<AddonUnavailable> { client.rewrite(f.root + "/manifest.json", f.root) }
        assertTrue(!error.message.orEmpty().contains("secret-token"))
        assertNull(error.cause)
        assertNull(f.stored)
        f.now += 20
        assertFailsWith<AddonUnavailable> { client.rewrite(f.root + "/manifest.json", f.root) }
        assertEquals(1, f.calls)
        f.failure = null
        f.now += 11
        client.rewrite(f.root + "/manifest.json", f.root)
        assertEquals(2, f.calls)
    }

    @Test fun cancelledRenewalDoesNotPersistOrThrottleTheNextCaller() = runTest {
        val f = Fixture(); val client = f.client()
        f.failure = CancellationException("cancel")
        assertFailsWith<CancellationException> { client.rewrite(f.root + "/manifest.json", f.root) }
        assertNull(f.stored)
        f.failure = null
        client.rewrite(f.root + "/manifest.json", f.root)
        assertEquals(2, f.calls)
    }

    @Test fun timeoutIsBoundedAndThrottlesFurtherRenewals() = runTest {
        val f = Fixture(); val client = f.client(); f.hang = true
        assertFailsWith<AddonUnavailable> { client.rewrite(f.root + "/manifest.json", f.root) }
        assertFailsWith<AddonUnavailable> { client.rewrite(f.root + "/manifest.json", f.root) }
        assertEquals(1, f.calls)
        assertNull(f.stored)
    }

    @Test fun adapterMustApproveTheReturnedLeaseBeforeAnythingIsPersisted() = runTest {
        val f = Fixture(); f.unsafe = true
        assertFailsWith<AddonUnavailable> { f.client().rewrite(f.root + "/manifest.json", f.root) }
        assertNull(f.stored)
    }

    @Test fun unrelatedAddonDoesNotRenewOrReadStorage() = runTest {
        val f = Fixture()
        val client = AddonSession(f, { error("must not read") }, { error("must not write") }, { f.now })
        val url = "https://ordinary.example/private/manifest.json"
        assertEquals(url, client.rewrite(url, "https://ordinary.example/private"))
        assertEquals(0, f.calls)
    }
}
