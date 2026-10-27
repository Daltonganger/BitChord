package com.music.bitchord.data.sources.addon

import com.music.bitchord.data.crypto.Sha256
import com.music.bitchord.data.http.Http
import com.music.bitchord.data.settings.PlatformSettings
import io.ktor.http.Url
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.yield
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlin.time.Clock
import kotlin.time.TimeSource
import kotlin.time.Duration.Companion.seconds

/** The provider's normal challenge/mint protocol, renewed on use rather than by a background timer. */
internal object LastBabeAddon {
    private const val SECRET_KEY = "addon_lastbabe_session_v1"
    private val session = LastBabeSession(
        get = { Http.getText(it, timeoutMillis = 15_000) },
        post = { url, body -> Http.postJson(url, body, timeoutMillis = 15_000) },
        read = { PlatformSettings.getSecret(SECRET_KEY) },
        write = { PlatformSettings.putSecret(SECRET_KEY, it) },
        now = { Clock.System.now().epochSeconds },
    )

    suspend fun rewrite(url: String, configuredBase: String, rejectedUrl: String? = null): String =
        session.rewrite(url, configuredBase, rejectedUrl)
}

internal class LastBabeSession(
    private val get: suspend (String) -> String,
    private val post: suspend (String, String) -> String,
    private val read: () -> String?,
    private val write: (String) -> Unit,
    private val now: () -> Long,
) {
    private val json = Json { ignoreUnknownKeys = true }
    private val lock = Mutex()
    private var current: Lease? = null

    suspend fun rewrite(url: String, configuredBase: String, rejectedUrl: String? = null): String {
        if (!supports(configuredBase)) return url
        val suffix = url.removePrefix(configuredBase)
        if (suffix == url || !suffix.startsWith('/')) throw AddonException("Invalid Last Babe endpoint")
        val lease = lock.withLock {
            try {
                val cached = current ?: read()?.let { runCatching { json.decodeFromString<Lease>(it) }.getOrNull() }
                val rejected = cached != null && rejectedUrl?.startsWith(cached.baseUrl + "/") == true
                if (cached != null && valid(cached) && !rejected) {
                    current = cached
                    cached
                } else {
                    mint().also {
                        write(json.encodeToString(Lease.serializer(), it))
                        current = it
                    }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: AddonException) {
                throw e
            } catch (_: Exception) {
                // Neither a minted token nor a persisted credential belongs in an error log.
                throw AddonUnavailable("Last Babe could not renew its addon URL; try again later")
            }
        }
        return lease.baseUrl + suffix
    }

    private fun valid(lease: Lease): Boolean =
        lease.expiresAt > now() + RENEW_EARLY_SECONDS && isLeaseBase(lease.baseUrl)

    private suspend fun mint(): Lease {
        val challenge = json.decodeFromString<Challenge>(get("$ORIGIN/challenge"))
        if (challenge.rickroll) throw AddonException("Last Babe is not currently offering the requested music")
        val nonce = solve(challenge.challenge, challenge.difficulty)
        val request = json.encodeToString(Proof.serializer(), Proof(challenge.challenge, nonce))
        val response = json.decodeFromString<Minted>(post("$ORIGIN/mint", request))
        val base = AddonClient.normalizeBase(response.manifestUrl ?: response.addonUrl.orEmpty())
        val lease = Lease(base, response.expiresAt)
        if (!valid(lease)) throw AddonException("Last Babe returned an invalid or expired addon URL")
        return lease
    }

    @Serializable private data class Challenge(val challenge: String, val difficulty: Int = 4, val rickroll: Boolean = false)
    @Serializable private data class Proof(val challenge: String, val nonce: String)
    @Serializable private data class Minted(val manifestUrl: String? = null, val addonUrl: String? = null, val expiresAt: Long)
    @Serializable private data class Lease(val baseUrl: String, val expiresAt: Long)

    companion object {
        const val ORIGIN = "https://lastbabe.velixir.run"
        private const val RENEW_EARLY_SECONDS = 600L
        private val leasePath = Regex("/e/[0-9]+/[A-Za-z0-9_-]+")

        fun supports(base: String): Boolean =
            cleanUrl(base)?.let { it.encodedPath in listOf("", "/") || leasePath.matches(it.encodedPath) } == true

        private fun isLeaseBase(base: String): Boolean = cleanUrl(base)?.let { leasePath.matches(it.encodedPath) } == true

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
