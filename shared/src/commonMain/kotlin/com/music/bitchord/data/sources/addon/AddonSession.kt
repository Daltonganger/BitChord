package com.music.bitchord.data.sources.addon

import com.music.bitchord.data.settings.PlatformSettings
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlin.time.Clock

/** Provider adapters own the handshake and the exact URLs they are allowed to renew. */
internal interface AddonSessionProvider {
    val secretKey: String
    fun supports(base: String): Boolean
    fun acceptsLease(base: String): Boolean
    suspend fun renew(): AddonLease
}

@Serializable
internal data class AddonLease(val baseUrl: String, val expiresAt: Long)

/** Shared lifecycle; no assumptions about a provider's challenge or authentication protocol. */
internal class AddonSession(
    private val provider: AddonSessionProvider,
    private val read: () -> String?,
    private val write: (String) -> Unit,
    private val now: () -> Long,
) {
    private val json = Json { ignoreUnknownKeys = true }
    private val lock = Mutex()
    private var current: AddonLease? = null
    private var retryAfter = 0L

    suspend fun rewrite(url: String, configuredBase: String, rejectedUrl: String? = null): String {
        if (!provider.supports(configuredBase)) return url
        val suffix = url.removePrefix(configuredBase)
        if (suffix == url || !suffix.startsWith('/')) throw AddonException("Invalid addon endpoint")
        val lease = lock.withLock {
            try {
                val cached = current ?: read()?.let { runCatching { json.decodeFromString<AddonLease>(it) }.getOrNull() }
                val rejected = cached != null && rejectedUrl?.startsWith(cached.baseUrl + "/") == true
                if (cached != null && valid(cached) && !rejected) {
                    current = cached
                    cached
                } else {
                    if (now() < retryAfter) throw AddonUnavailable("Addon renewal is temporarily unavailable; try again later")
                    val fresh = withTimeoutOrNull(40_000) { provider.renew() }
                        ?: throw AddonUnavailable("Addon renewal timed out")
                    if (!valid(fresh)) throw AddonException("Addon returned an invalid or expired session URL")
                    write(json.encodeToString(AddonLease.serializer(), fresh))
                    current = fresh
                    retryAfter = 0
                    fresh
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                if (now() >= retryAfter) retryAfter = now() + 30
                // Never propagate provider responses, signed URLs or storage errors to logs.
                throw AddonUnavailable("Addon could not renew its session; try again later")
            }
        }
        return lease.baseUrl + suffix
    }

    private fun valid(lease: AddonLease): Boolean =
        lease.expiresAt > now() + 600 && provider.acceptsLease(lease.baseUrl)
}

/** Ordinary addons pass through; supporting another renewal protocol adds one adapter here. */
internal object AddonSessions {
    private val providers: List<AddonSessionProvider> = listOf(LastBabeSessionProvider())
    private val sessions = providers.map { provider ->
        provider to AddonSession(
            provider,
            read = { PlatformSettings.getSecret(provider.secretKey) },
            write = { PlatformSettings.putSecret(provider.secretKey, it) },
            now = { Clock.System.now().epochSeconds },
        )
    }

    fun supports(base: String): Boolean = providers.any { it.supports(base) }

    suspend fun rewrite(url: String, configuredBase: String, rejectedUrl: String? = null): String =
        sessions.firstOrNull { it.first.supports(configuredBase) }?.second
            ?.rewrite(url, configuredBase, rejectedUrl) ?: url
}
