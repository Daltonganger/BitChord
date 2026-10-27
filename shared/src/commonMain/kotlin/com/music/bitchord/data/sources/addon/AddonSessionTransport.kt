package com.music.bitchord.data.sources.addon

import com.music.bitchord.data.http.Http
import io.ktor.client.HttpClient
import io.ktor.client.plugins.timeout
import io.ktor.client.request.prepareRequest
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsChannel
import io.ktor.http.ContentType
import io.ktor.http.HttpMethod
import io.ktor.http.contentType
import io.ktor.util.AttributeKey
import io.ktor.utils.io.cancel
import io.ktor.utils.io.readRemaining
import kotlinx.io.readByteArray

/** Renewal documents are small JSON messages, never executable provider code. */
internal object AddonSessionTransport {
    // Consulted by the shared transport before following any redirect.
    val noRedirects = AttributeKey<Boolean>("AddonSessionNoRedirects")
    private const val MAX_BYTES = 16_384

    suspend fun request(url: String, body: String? = null, client: HttpClient = Http.client): String =
        client.prepareRequest(url) {
            attributes.put(noRedirects, true)
            method = if (body == null) HttpMethod.Get else HttpMethod.Post
            timeout {
                requestTimeoutMillis = 15_000
                connectTimeoutMillis = 15_000
                socketTimeoutMillis = 15_000
            }
            if (body != null) {
                contentType(ContentType.Application.Json)
                setBody(body)
            }
        }.execute { response ->
            val channel = response.bodyAsChannel()
            try {
                if (response.status.value !in 200..299) throw AddonUnavailable("Addon renewal request failed")
                val bytes = channel.readRemaining(MAX_BYTES.toLong() + 1).readByteArray()
                if (bytes.size > MAX_BYTES) throw AddonException("Addon renewal response is too large")
                bytes.decodeToString()
            } finally {
                channel.cancel(null)
            }
        }
}
