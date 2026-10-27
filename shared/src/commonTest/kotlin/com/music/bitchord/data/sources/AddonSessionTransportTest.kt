package com.music.bitchord.data.sources

import com.music.bitchord.data.sources.addon.AddonException
import com.music.bitchord.data.sources.addon.AddonSessionTransport
import com.music.bitchord.data.sources.addon.AddonUnavailable
import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.engine.mock.respondOk
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class AddonSessionTransportTest {
    @Test fun readsSmallDocumentsAndRejectsOversizedResponses() = runTest {
        for (size in listOf(16_384, 16_385, 100_000)) {
            val client = HttpClient(MockEngine { request ->
                assertTrue(request.attributes[AddonSessionTransport.noRedirects])
                respondOk("x".repeat(size))
            }) { followRedirects = false }
            try {
                if (size <= 16_384) {
                    assertEquals(size, AddonSessionTransport.request("https://addon.example/challenge", client = client).length)
                } else {
                    assertFailsWith<AddonException> { AddonSessionTransport.request("https://addon.example/challenge", client = client) }
                }
            } finally { client.close() }
        }
    }

    @Test fun redirectAndErrorBodiesAreNeverAcceptedAsRenewalDocuments() = runTest {
        for (status in listOf(HttpStatusCode.Found, HttpStatusCode.TemporaryRedirect, HttpStatusCode.Forbidden)) {
            var calls = 0
            val client = HttpClient(MockEngine {
                calls++
                respond("secret-provider-body", status, headersOf("Location", "https://attacker.example/collect"))
            }) { followRedirects = false }
            try {
                val error = assertFailsWith<AddonUnavailable> {
                    AddonSessionTransport.request("https://addon.example/mint", body = "{}", client = client)
                }
                assertEquals(1, calls)
                assertTrue(!error.message.orEmpty().contains("secret-provider-body"))
            } finally { client.close() }
        }
    }
}
