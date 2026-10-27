package com.music.bitchord.data.sources.addon

import com.music.bitchord.data.crypto.Sha256
import com.music.bitchord.data.settings.PlatformSettings
import io.ktor.http.URLBuilder
import io.ktor.http.Url
import io.ktor.http.takeFrom
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.yield
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.longOrNull
import kotlin.time.Clock
import kotlin.time.Duration.Companion.seconds
import kotlin.time.Instant
import kotlin.time.TimeSource
import kotlin.random.Random

/** Sessions exist only for renewal links explicitly configured by the listener. */
internal object AddonRefresh {
    private val lock = Mutex()
    private val sessions = mutableMapOf<String, AddonRefreshSession>()

    suspend fun rewrite(url: String, base: String, refreshUrl: String?, rejectedUrl: String? = null): String {
        if (refreshUrl.isNullOrBlank()) return url
        return session(base, refreshUrl).rewrite(url, base, rejectedUrl)
    }

    suspend fun cacheIdentity(base: String, refreshUrl: String?): String =
        if (refreshUrl.isNullOrBlank()) base else session(base, refreshUrl).cacheIdentity(base)

    private suspend fun session(base: String, refreshUrl: String): AddonRefreshSession {
        val key = "addon_refresh_v1." + Sha256.digest("${base.length}:$base$refreshUrl".encodeToByteArray())
            .joinToString("") { (it.toInt() and 255).toString(16).padStart(2, '0') }
        return lock.withLock {
            sessions.getOrPut(key) {
                AddonRefreshSession(
                    refreshUrl = refreshUrl,
                    get = { AddonSessionTransport.request(it) },
                    post = { address, body -> AddonSessionTransport.request(address, body) },
                    read = { PlatformSettings.getSecret(key) },
                    write = { check(PlatformSettings.writeSecret(key, it)) { "Could not securely save the renewed addon session" } },
                    now = { Clock.System.now().epochSeconds },
                )
            }
        }
    }

    fun validAddress(value: String): Boolean = runCatching {
        val url = Url(value)
        url.protocol.name == "https" && url.host.isNotBlank() &&
            url.user.isNullOrEmpty() && url.password.isNullOrEmpty() && url.fragment.isEmpty()
    }.getOrDefault(false)
}

internal class AddonRefreshSession(
    private val refreshUrl: String,
    private val get: suspend (String) -> String,
    private val post: suspend (String, String) -> String,
    private val read: () -> String?,
    private val write: (String) -> Unit,
    private val now: () -> Long,
) {
    private val json = Json { ignoreUnknownKeys = true }
    private val lock = Mutex()
    private var current: Lease? = null
    private var retryAfter = 0L
    private var rejectedLeaseId: String? = null

    suspend fun cacheIdentity(base: String): String {
        rewrite(AddonClient.manifestUrl(base), base)
        return lock.withLock { checkNotNull(current).cacheId }
    }

    suspend fun rewrite(url: String, configuredBase: String, rejectedUrl: String? = null): String {
        if (!AddonRefresh.validAddress(refreshUrl)) throw AddonException("Enter an HTTPS renewal link from the addon provider")
        val suffix = url.removePrefix(configuredBase)
        if (suffix == url || !suffix.startsWith('/')) throw AddonException("Invalid addon endpoint")
        val lease = lock.withLock {
            val cached = current ?: read()?.let { runCatching { json.decodeFromString<Lease>(it) }.getOrNull() }
            val safe = cached?.takeIf { validBase(it.baseUrl) }
            val rejected = safe != null && rejectedUrl?.startsWith(safe.baseUrl + "/") == true
            if (rejected) rejectedLeaseId = safe?.cacheId
            val usable = safe != null && safe.cacheId != rejectedLeaseId &&
                (safe.expiresAt == null || safe.expiresAt > now())
            if (usable && (safe!!.refreshAt == null || safe.refreshAt > now())) {
                current = safe
                checkNotNull(safe)
            } else {
                if (now() < retryAfter) {
                    if (usable) {
                        current = safe
                        return@withLock checkNotNull(safe)
                    }
                    throw AddonUnavailable("Addon renewal is temporarily unavailable; try again later")
                }
                try {
                    val fresh = withContext(Dispatchers.Default) {
                        withTimeoutOrNull(40_000) { renew() }
                    }
                        ?: throw AddonUnavailable("Addon renewal timed out")
                    fresh.also {
                        write(json.encodeToString(Lease.serializer(), it))
                        current = it
                        retryAfter = 0
                        rejectedLeaseId = null
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    retryAfter = now() + 30
                    // A temporarily unavailable renewal service need not interrupt a still-valid lease.
                    if (usable) {
                        current = safe
                        checkNotNull(safe)
                    } else if (e is AddonException) {
                        throw e
                    } else {
                        // Provider replies and signed URLs must not leak into diagnostic errors.
                        throw AddonUnavailable("Could not renew the addon URL; try again later")
                    }
                }
            }
        }
        return lease.baseUrl + suffix
    }

    private suspend fun renew(): Lease {
        val body = get(refreshUrl)
        val document = runCatching { json.parseToJsonElement(body) as? JsonObject }.getOrNull()
        if (document != null && listOf("manifestUrl", "addonUrl", "baseUrl").any { document.text(it) != null }) {
            return lease(document)
        }
        val endpoints = if (document != null) {
            val challenge = document.text("challengeUrl")
            val refresh = document.text("refreshUrl")
            if (challenge == null || refresh == null) throw unsupported()
            challenge to refresh
        } else {
            // Read only literal endpoint declarations. Never run a page's JavaScript.
            if (!body.contains("SHA-256", ignoreCase = true)) throw unsupported()
            val calls = Regex("""fetch\s*\(\s*["']([^"']+)["']\s*(,\s*\{[^)]{0,2000})?""")
                .findAll(body).toList()
            val post = calls.firstOrNull { Regex("""method\s*:\s*["']POST["']""", RegexOption.IGNORE_CASE)
                .containsMatchIn(it.groupValues[2]) } ?: throw unsupported()
            val challenge = calls.firstOrNull { it.groupValues[2].isBlank() } ?: throw unsupported()
            challenge.groupValues[1] to post.groupValues[1]
        }
        val challengeUrl = endpoint(endpoints.first)
        val mintUrl = endpoint(endpoints.second)
        val challenge = json.decodeFromString<Challenge>(get(challengeUrl))
        val nonce = solve(challenge.challenge, challenge.difficulty)
        val proof = json.encodeToString(Proof.serializer(), Proof(challenge.challenge, nonce))
        val minted = json.parseToJsonElement(post(mintUrl, proof)) as? JsonObject ?: throw unsupported()
        return lease(minted)
    }

    private fun lease(document: JsonObject): Lease {
        val address = document.text("manifestUrl") ?: document.text("addonUrl") ?: document.text("baseUrl")
            ?: throw AddonException("The renewal link did not return an addon URL")
        val base = AddonClient.normalizeBase(address)
        if (!validBase(base)) throw AddonException("The renewal link returned an unsupported addon URL")
        val expiresAt = document["expiresAt"]?.takeUnless { it is JsonNull }?.let { value ->
            val text = (value as? JsonPrimitive)?.contentOrNull ?: throw AddonException("Invalid addon expiry")
            text.toLongOrNull()?.let { if (it > 100_000_000_000) it / 1000 else it }
                ?: runCatching { Instant.parse(text).epochSeconds }.getOrElse { throw AddonException("Invalid addon expiry") }
        } ?: document["expiresIn"]?.takeUnless { it is JsonNull }?.let { value ->
            val seconds = (value as? JsonPrimitive)?.longOrNull ?: throw AddonException("Invalid addon expiry")
            if (seconds !in 1..31_536_000) throw AddonException("Invalid addon expiry")
            now() + seconds
        }
        if (expiresAt != null && expiresAt <= now() + 2) throw AddonException("The renewed addon URL has already expired")
        val refreshAt = expiresAt?.let { it - minOf(600, maxOf(1, (it - now()) / 10)) }
        return Lease(base, expiresAt, refreshAt)
    }

    private fun endpoint(path: String): String {
        val value = URLBuilder().takeFrom(refreshUrl).apply {
            parameters.clear()
            fragment = ""
        }.takeFrom(path).buildString()
        val actual = Url(value); val origin = Url(refreshUrl)
        if (!AddonRefresh.validAddress(value) || actual.host != origin.host || actual.port != origin.port) {
            throw AddonException("The renewal page declares an endpoint outside its own origin")
        }
        return value
    }

    private fun validBase(value: String): Boolean = AddonRefresh.validAddress(value) && Url(value).parameters.isEmpty()
    private fun JsonObject.text(key: String): String? = (this[key] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }
    private fun unsupported() = AddonException("This renewal link needs a supported API; renew manually if it requires sign-in or a CAPTCHA")

    @Serializable private data class Challenge(val challenge: String, val difficulty: Int = 4)
    @Serializable private data class Proof(val challenge: String, val nonce: String)
    @Serializable private data class Lease(
        val baseUrl: String, val expiresAt: Long? = null, val refreshAt: Long? = null,
        val cacheId: String = Random.nextLong().toString(),
    )

    companion object {
        internal suspend fun solve(challenge: String, difficulty: Int): String = withContext(Dispatchers.Default) {
            if (challenge.isBlank() || challenge.length > 4096 || difficulty !in 1..5) {
                throw AddonException("Unsupported renewal challenge")
            }
            val started = TimeSource.Monotonic.markNow()
            val wholeBytes = difficulty / 2
            for (nonce in 1..2_000_000) {
                val digest = Sha256.digest((challenge + nonce).encodeToByteArray())
                if ((0 until wholeBytes).all { digest[it] == 0.toByte() } &&
                    (difficulty % 2 == 0 || (digest[wholeBytes].toInt() and 0xf0) == 0)) return@withContext nonce.toString()
                if (nonce % 256 == 0) {
                    yield()
                    if (started.elapsedNow() > 8.seconds) break
                }
            }
            throw AddonUnavailable("Addon renewal took too long; try again later")
        }
    }
}
