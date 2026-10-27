package com.music.bitchord.data.sources.addon

import com.music.bitchord.data.crypto.Sha256
import io.ktor.http.Url
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.yield
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlin.time.TimeSource
import kotlin.time.Duration.Companion.seconds

/** Only the provider-specific wire protocol and URL ownership rules live here. */
internal class LastBabeSessionProvider(
    private val get: suspend (String) -> String = { AddonSessionTransport.request(it) },
    private val post: suspend (String, String) -> String = { url, body -> AddonSessionTransport.request(url, body) },
) : AddonSessionProvider {
    override val secretKey = "addon_lastbabe_session_v1"
    override fun supports(base: String): Boolean = Companion.supports(base)
    override fun acceptsLease(base: String): Boolean = isLeaseBase(base)
    private val json = Json { ignoreUnknownKeys = true }

    override suspend fun renew(): AddonLease {
        val challenge = json.decodeFromString<Challenge>(get("$ORIGIN/challenge"))
        if (challenge.rickroll) throw AddonException("Last Babe is not currently offering the requested music")
        val nonce = solve(challenge.challenge, challenge.difficulty)
        val request = json.encodeToString(Proof.serializer(), Proof(challenge.challenge, nonce))
        val response = json.decodeFromString<Minted>(post("$ORIGIN/mint", request))
        val base = AddonClient.normalizeBase(response.manifestUrl ?: response.addonUrl.orEmpty())
        return AddonLease(base, response.expiresAt)
    }

    @Serializable private data class Challenge(val challenge: String, val difficulty: Int = 4, val rickroll: Boolean = false)
    @Serializable private data class Proof(val challenge: String, val nonce: String)
    @Serializable private data class Minted(val manifestUrl: String? = null, val addonUrl: String? = null, val expiresAt: Long)

    companion object {
        const val ORIGIN = "https://lastbabe.velixir.run"
        private val leasePath = Regex("/e/[0-9]+/[A-Za-z0-9_-]+")

        fun supports(base: String): Boolean =
            cleanUrl(base)?.let { it.encodedPath in listOf("", "/") || leasePath.matches(it.encodedPath) } == true

        fun isLeaseBase(base: String): Boolean = cleanUrl(base)?.let { leasePath.matches(it.encodedPath) } == true

        private fun cleanUrl(base: String): Url? = runCatching {
            val url = Url(base)
            url.takeIf { url.protocol.name == "https" && url.host == "lastbabe.velixir.run" && url.port == 443 &&
                url.user.isNullOrEmpty() && url.password.isNullOrEmpty() && url.parameters.isEmpty() &&
                url.fragment.isEmpty() }
        }.getOrNull()

        internal suspend fun solve(challenge: String, difficulty: Int): String = withContext(Dispatchers.Default) {
            if (challenge.isBlank() || challenge.length > 4096 || difficulty !in 1..5) {
                throw AddonException("Last Babe returned an unsupported challenge")
            }
            val started = TimeSource.Monotonic.markNow()
            for (nonce in 1..2_000_000) {
                val digest = Sha256.digest((challenge + nonce).encodeToByteArray())
                val wholeBytes = difficulty / 2
                if ((0 until wholeBytes).all { digest[it] == 0.toByte() } &&
                    (difficulty % 2 == 0 || (digest[wholeBytes].toInt() and 0xf0) == 0)) {
                    return@withContext nonce.toString()
                }
                if (nonce % 256 == 0) {
                    yield()
                    if (started.elapsedNow() > 8.seconds) break
                }
            }
            throw AddonUnavailable("Last Babe renewal took too long; try again later")
        }
    }
}
