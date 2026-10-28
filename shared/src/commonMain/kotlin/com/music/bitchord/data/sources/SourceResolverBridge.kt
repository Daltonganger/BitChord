package com.music.bitchord.data.sources

import com.music.bitchord.data.DebugLog
import com.music.bitchord.data.model.Song
import com.music.bitchord.data.settings.AppSettings
import com.music.bitchord.data.settings.AudioQuality
import com.music.bitchord.data.sources.addon.AddonRefresh
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * Swift-facing seam over [SourceResolver].
 *
 * Deliberately shaped around the four decisions the host actually has to make at
 * a moment of playback, rather than exposing the resolver's internals:
 *
 *  - [canSubstituteForYouTube] — cheap, synchronous, and asked from the cache and
 *    the read-ahead *before* anyone has searched, so it has to be answerable from
 *    the source list alone.
 *  - [substituteForYouTube] — the latency-critical race, run before a queued YouTube
 *    track is resolved.
 *  - [prefetchSubstitute] — the same race, earlier, narrowed to the sources quick
 *    enough to be worth asking speculatively.
 *  - [upgradeFor] — the unhurried second look, run with sound already playing.
 *  - [forDownload] — the one whose answer becomes a file rather than a stream.
 *
 * [worthSwapping] and [sameRecordingAs] are exposed because the host has to apply
 * the *same* judgements when it adopts or refuses a swap. They used to be
 * reimplemented in Swift, which is how the two copies came to disagree: the
 * Swift one had no Dolby-Atmos rule and its `canSubstitute` merely checked whether
 * any source URL was configured, so a source ranked *last* counted as a reason to
 * substitute.
 */
object SourceResolverBridge {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val json = Json { ignoreUnknownKeys = true }

    fun interface StreamCallback {
        fun onResult(json: String?, message: String?)
    }

    fun interface AnswerCallback {
        fun onResult(answer: Boolean)
    }

    // ── Cheap, synchronous ────────────────────────────────────────────��─────

    /**
     * Whether anything outranks YouTube.
     *
     * The real check — the ranked list, not "is a URL configured". The old Swift
     * version returned true if *any* source was set at all, including one ranked
     * below YouTube, so every queued track paid a pointless race for a source that
     * could not have won it.
     */
    fun canSubstituteForYouTube(): Boolean = runCatching { SourceResolver.canSubstituteForYouTube() }.getOrDefault(false)

    /** A stream that genuinely satisfies the request, for a track already playing. */
    fun worthSwapping(candidateJson: String, playingJson: String?): Boolean {
        val candidate = readFormat(candidateJson) ?: return false
        val playing = playingJson?.let { readFormat(it) }
        return SourceResolver.worthSwapping(candidate, playing)
    }

    /** Whether two runtimes are close enough to be the same recording. */
    fun sameRecordingAs(candidateSec: Int?, playingSec: Int?): Boolean =
        SourceResolver.sameRecordingAs(candidateSec, playingSec)

    // ── The races ───────────────────────────────────────────────────────────

    /** The stream for a queued YouTube track from a source ranked above it. */
    fun substituteForYouTube(
        title: String,
        artist: String,
        durationSec: Int?,
        album: String?,
        isExplicit: Boolean?,
        isVideo: Boolean,
        callback: StreamCallback,
    ) = lookup(callback) {
        val target = TrackMatcher.Target(title, artist, durationSec, album, isExplicit, isVideo)
        SourceResolver.substituteForYouTube(target)
    }

    /**
     * The stream for one addon search row, by its `src:` key — the path a search
     * result takes when it is tapped.
     *
     * [SourceResolver.resolve] opens the row's own source first and may fall
     * through to another enabled one, so a row whose source has since died is
     * still a playable track rather than a dead click.
     */
    fun resolveAddonTrack(
        key: String,
        title: String,
        artist: String,
        durationSec: Int?,
        callback: StreamCallback,
    ) = lookup(callback) {
        val parsed = SourceRegistry.parseTrackKey(key) ?: return@lookup null
        val target = TrackMatcher.Target(title, artist, durationSec)
        SourceResolver.resolve(parsed.first, parsed.second, target)
    }

    /**
     * The unhurried second look for a track already playing.
     *
     * @param playingJson what the listener is hearing now, as a
     *   [StreamFormat] document or null when unmeasured. A null floor is treated as
     *   one nothing lossy clears: a swap that might be a downgrade is worse than no
     *   swap at all.
     * @param servedBy the config id already serving the track, so it is not asked
     *   again for a stream it would only reproduce.
     */
    fun upgradeFor(
        title: String,
        artist: String,
        durationSec: Int?,
        album: String?,
        isExplicit: Boolean?,
        isVideo: Boolean,
        playingJson: String?,
        servedBy: String?,
        callback: StreamCallback,
    ) = lookup(callback) {
        val target = TrackMatcher.Target(title, artist, durationSec, album, isExplicit, isVideo)
        val playing = playingJson?.let { readFormat(it) }
        SourceResolver.upgradeFor(target, playing, servedBy)
    }

    /**
     * The best copy within the next-track preparation window, including slow addons.
     *
     * The caller is expected to **pin** a returned stream before caching any of it,
     * or playback re-runs the race and may land on a different source — writing a
     * second file into the cache entry the warm one already half-filled.
     */
    fun prefetchSubstitute(
        title: String,
        artist: String,
        durationSec: Int?,
        album: String?,
        isExplicit: Boolean?,
        isVideo: Boolean,
        callback: StreamCallback,
    ) = lookup(callback) {
        val target = TrackMatcher.Target(title, artist, durationSec, album, isExplicit, isVideo)
        SourceResolver.prefetchSubstitute(target)
    }

    /** The copy worth keeping as a file over whatever YouTube's ladder would give. */
    fun forDownload(
        title: String,
        artist: String,
        durationSec: Int?,
        album: String?,
        isExplicit: Boolean?,
        isVideo: Boolean,
        quality: String,
        callback: StreamCallback,
    ) = launch(callback) {
        val target = TrackMatcher.Target(title, artist, durationSec, album, isExplicit, isVideo)
        val request = SourceResolver.requestForDownload(
            com.music.bitchord.data.settings.DownloadQuality.fromName(quality),
        )
        SourceResolver.forDownload(target, request)?.let { callback.onResult(it.encode(), null) }
            ?: callback.onResult(null, null)
    }

    /**
     * The ceiling in force on the connection as it is right now.
     *
     * Answered here rather than in the host because the question is not "what is
     * the setting" — it is "which of two settings applies to the connection that
     * is up, and is that connection metered". That is derived state, it lives in
     * [AppSettings], and a host that assembled it from the two settings itself
     * would be one refactor away from making a mobile-data choice follow someone
     * onto Wi-Fi.
     */
    fun activeCeiling(): String = ceiling(AppSettings.meteredConnection.value).name

    /**
     * Whether the connection up right now is the metered one.
     *
     * Asked alongside [activeCeiling] because the row's "not used on …" line has
     * to name *which* connection, and a ceiling on its own cannot say that. It is
     * derived from the OS rather than stored, so there is no key to read and this
     * is the only honest place to get it.
     */
    fun activeConnectionIsMetered(): Boolean = AppSettings.meteredConnection.value == true

    /** Whether a source of [kind] is worth asking on the connection as it is. */
    fun activeCeilingPermits(kind: String): Boolean {
        val name = ceiling(AppSettings.meteredConnection.value)
        val sourceKind = SourceKind.entries.firstOrNull { it.name == kind } ?: return true
        return name.permits(sourceKind)
    }

    private fun ceiling(metered: Boolean?): AudioQuality =
        if (metered == true) AppSettings.audioQualityCellular.value
        else AppSettings.audioQualityWifi.value

    /**
     * Whether a source of [kind] is worth asking at [quality].
     *
     * Exposed because the sources screen has to say "on, but not today" when the
     * connection's ceiling is below what a source is for. The policy — which
     * ceilings skip which kinds — is a property of the ceiling rather than of the
     * source, so it belongs in one place; a copy in the host would drift from it
     * and the screen would eventually claim a source is being used when it is
     * not.
     */
    fun permits(quality: String, kind: String): Boolean {
        val ceiling = when (quality) {
            "LOW" -> AudioQuality.LOW
            "MEDIUM" -> AudioQuality.MEDIUM
            "HIGH" -> AudioQuality.HIGH
            else -> AudioQuality.LOSSLESS
        }
        val sourceKind = SourceKind.entries.firstOrNull { it.name == kind } ?: return true
        return ceiling.permits(sourceKind)
    }

    // ── Registry, for the sources screen ────────────────────────────────────

    fun interface ConfigsCallback {
        fun onResult(json: String?)
    }

    fun interface HealthCallback {
        fun onResult(configId: String, health: String, detail: String)
    }

    fun interface ActionCallback {
        fun onResult(ok: Boolean, message: String?)
    }

    /** Every configured source, as a document the sources screen can read. */
    fun configs(callback: ConfigsCallback) {
        val document = runCatching { json.encodeToString(ConfigListing.serializer(), readConfigs()) }.getOrNull()
        callback.onResult(document)
    }

    /** Reachability for one configured source, or for a candidate not yet saved. */
    fun health(configId: String, callback: HealthCallback) {
        scope.launch {
            val health = try {
                withTimeoutOrNull(15_000) {
                    val config = SourceRegistry.config(configId)
                    if (config != null) SourceRegistry.probeCandidate(config)
                    else com.music.bitchord.data.sources.SourceHealth.Rejected("Unknown source")
                } ?: com.music.bitchord.data.sources.SourceHealth.Unreachable("The source did not answer in time. Try again.")
            } catch (e: kotlinx.coroutines.CancellationException) { throw e }
            catch (_: Exception) {
                com.music.bitchord.data.sources.SourceHealth.Unreachable("The source could not be checked. Try again.")
            }
            callback.onResult(configId, healthName(health), healthDetail(health))
        }
    }

    /**
     * Health for a config that has not been saved — the editor's Test button.
     *
     * Takes the kind as a *name* rather than a [SourceConfig], so the editor can
     * open for a kind it has no enum case for and a kind added to the registry
     * becomes editable without a second list kept in step on the other side of
     * the bridge.
     */
    fun probeCandidate(
        kind: String,
        baseUrl: String,
        label: String,
        callback: ActionCallback,
    ) = probeCandidateWithRefresh(kind, baseUrl, label, false, "", callback)

    fun probeCandidateWithRefresh(
        kind: String, baseUrl: String, label: String,
        refreshEnabled: Boolean, refreshUrl: String, callback: ActionCallback,
    ) {
        scope.launch {
            val resolved = SourceKind.entries.firstOrNull { it.name == kind }
            if (resolved == null) {
                callback.onResult(false, "Unknown kind \"$kind\"")
                return@launch
            }
            val health = SourceRegistry.probeCandidate(
                SourceConfig(
                    id = "candidate",
                    kind = resolved,
                    label = label,
                    baseUrl = baseUrl,
                    enabled = true,
                    refreshEnabled = refreshEnabled,
                    refreshUrl = refreshUrl,
                ),
            )
            // Reported through the action channel because it is one question with
            // one answer, and the editor wants a verdict rather than a name/detail
            // pair it has to re-join.
            callback.onResult(
                health is com.music.bitchord.data.sources.SourceHealth.Ok,
                healthDetail(health),
            )
        }
    }

    /**
     * Save a source: add it when the id is new, update it otherwise.
     *
     * The URL goes to the secret tier rather than the settings list, so an addon
     * whose address carries a token does not end up in a preferences dump.
     */
    /** Save a source: add it when the id is new, update it otherwise. */
    fun save(
        id: String,
        kind: String,
        label: String,
        baseUrl: String,
        enabled: Boolean,
        callback: ActionCallback,
    ) {
        val existing = try { SourceRegistry.config(id) } catch (_: Exception) {
            callback.onResult(false, "Sources could not be read. Unlock your device or Keychain and try again.")
            return
        }
        saveWithRefresh(id, kind, label, baseUrl, enabled,
            existing?.refreshEnabled ?: false, existing?.refreshUrl.orEmpty(), existing?.refreshUrlUnavailable ?: false, callback)
    }

    fun saveWithRefresh(
        id: String, kind: String, label: String, baseUrl: String, enabled: Boolean,
        refreshEnabled: Boolean, refreshUrl: String, preserveUnavailableRefreshUrl: Boolean = false, callback: ActionCallback,
    ) {
        scope.launch {
            try {
                val preserveLink = preserveUnavailableRefreshUrl && refreshUrl.isBlank()
                if (refreshEnabled && (kind != "ADDON" || (!preserveLink && !AddonRefresh.validAddress(refreshUrl.trim())))) {
                    callback.onResult(false, "Enter an HTTPS renewal link from the addon provider")
                    return@launch
                }
                val resolved = SourceKind.entries.firstOrNull { it.name == kind }
                if (resolved == null) {
                    callback.onResult(false, "Unknown kind \"$kind\"")
                    return@launch
                }
                val config = SourceConfig(
                    id = id,
                    kind = resolved,
                    label = label,
                    baseUrl = baseUrl,
                    enabled = enabled,
                    refreshEnabled = refreshEnabled,
                    refreshUrl = refreshUrl,
                    refreshUrlUnavailable = preserveLink,
                )
                if (SourceRegistry.config(id) == null) {
                    SourceRegistry.add(config)
                } else {
                    SourceRegistry.update(config)
                }
                callback.onResult(true, null)
            } catch (e: Exception) {
                DebugLog.e("saving a source failed", e)
                callback.onResult(false, e.message ?: e.toString())
            }
        }
    }

    /**
     * Where [kind] sits in the walk, low first.
     *
     * Asked by name so the sources screen does not have to hold a copy of the
     * order — which is exactly the copy that would go stale the first time a kind
     * was added.
     */
    fun rankOf(kind: String): Int =
        SourceKind.entries.firstOrNull { it.name == kind }?.rank ?: Int.MAX_VALUE

    /**
     * How many addons are enabled. Counted the same way [SourceBridge.searchAddons]
     * asks them — `AddonSource` only — so a tab that says "no addons yet" and a
     * search that finds none can never disagree.
     */
    fun addonCount(): Int = runCatching { SourceRegistry.active().count { it is AddonSource } }.getOrDefault(0)

    fun setEnabled(configId: String, enabled: Boolean, callback: ActionCallback) {
        sourceAction(callback) { SourceRegistry.setEnabled(configId, enabled) }
    }

    fun remove(configId: String, callback: ActionCallback) {
        sourceAction(callback) { SourceRegistry.remove(configId) }
    }

    /** Put the addons in the order the listener dragged them into. */
    fun reorderAddons(orderedIds: List<String>, callback: ActionCallback) {
        sourceAction(callback) { SourceRegistry.reorderAddons(orderedIds) }
    }

    /** Kotlin exceptions must not cross the Swift callback boundary. */
    internal fun sourceAction(callback: ActionCallback, action: () -> Unit) {
        val error = try {
            action()
            null
        } catch (_: Exception) {
            "Could not save source settings; unlock your device or Keychain and try again"
        }
        callback.onResult(error == null, error)
    }

    /**
     * Whether [url] is already configured, so the editor can refuse a second copy.
     *
     * Compared after normalisation, which is what makes it catch the cases worth
     * catching: a trailing slash, a `MANIFEST.JSON`, and a differently-cased host
     * are all the same source.
     */
    fun isDuplicate(url: String, exceptId: String?): Boolean =
        runCatching { SourceRegistry.duplicateOf(url, exceptId) != null }.getOrDefault(false)

    /** Recovery retries must ask enabled addons again rather than reuse an expired URL. */
    fun refreshPlaybackSources() {
        runCatching {
            SourceRegistry.activeForPlayback().filterIsInstance<AddonSource>().forEach { it.clearCompletedTrackCalls() }
        }
    }

    private fun lookup(callback: StreamCallback, block: suspend () -> SourceStream?) {
        scope.launch {
            val report = SourceResolutionReport()
            try {
                val stream = withContext(report) { block() }
                callback.onResult(stream?.encode(), report.summary())
            } catch (e: kotlinx.coroutines.CancellationException) { throw e }
            catch (_: Exception) { callback.onResult(null, "Preferred sources could not provide playable audio.") }
        }
    }

    /** The one place a quality decision is made, exposed so the host agrees with it. */
    private fun launch(callback: StreamCallback, block: suspend () -> Unit) {
        scope.launch {
            try {
                block()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                DebugLog.e("source resolution failed", e)
                callback.onResult(null, e.message ?: e.toString())
            }
        }
    }

    // ── Wire format ─────────────────────────────────────────────────────────

    private fun healthName(health: com.music.bitchord.data.sources.SourceHealth): String =
        when (health) {
            is com.music.bitchord.data.sources.SourceHealth.Ok -> "ok"
            is com.music.bitchord.data.sources.SourceHealth.Unreachable -> "unreachable"
            is com.music.bitchord.data.sources.SourceHealth.Rejected -> "rejected"
        }

    private fun healthDetail(health: com.music.bitchord.data.sources.SourceHealth): String =
        when (health) {
            is com.music.bitchord.data.sources.SourceHealth.Ok -> health.detail.orEmpty()
            is com.music.bitchord.data.sources.SourceHealth.Unreachable -> health.reason
            is com.music.bitchord.data.sources.SourceHealth.Rejected -> health.reason
        }

    private fun readFormat(document: String): StreamFormat? =
        runCatching { json.decodeFromString(FormatDocument.serializer(), document) }
            .getOrNull()
            ?.toFormat()

    private fun SourceStream.encode(): String = encodeStream(this)

    // Native callbacks must use the already initialized bridge's serializer,
    // rather than a file-level Json that can still be null on this entry path.
    internal fun encodeStream(stream: SourceStream): String = json.encodeToString(
        StreamDocument.serializer(),
        StreamDocument(
            url = stream.url,
            format = FormatDocument(
                codec = stream.format.codec,
                kbps = stream.format.kbps,
                sampleRateHz = stream.format.sampleRateHz,
                bitDepth = stream.format.bitDepth,
                verified = stream.format.verified,
                isLossless = stream.format.isLossless,
                isDolbyAtmos = stream.format.isDolbyAtmos,
                summary = stream.format.summary,
            ),
            headers = stream.headers,
            belowRequest = stream.belowRequest,
            durationSec = stream.durationSec,
            sourceConfigId = stream.sourceConfigId,
        ),
    )

    private fun readConfigs(): ConfigListing = ConfigListing(
            SourceRegistry.configs.value.map { config ->
                ConfigDocument(
                    id = config.id,
                    kind = config.kind.name,
                    label = config.label,
                    displayName = config.displayName,
                    // The real address comes from the secret tier; `SourceConfig`'s own
                    // field is a fallback for a config stored before the split.
                    baseUrl = SourceRegistry.baseUrlOf(config),
                    enabled = config.enabled,
                    isComplete = config.isComplete,
                    needsServer = config.kind.needsServer,
                    canServeLossless = config.kind.canServeLossless,
                    labels = config.kind.labels,
                    refreshEnabled = config.refreshEnabled,
                    refreshUrl = config.refreshUrl,
                    refreshUrlUnavailable = config.refreshUrlUnavailable,
                )
            },
        )
}

/**
 * The wire format between the source layer and the host.
 *
 * `isLossless` is carried as a real nullable rather than a defaulted `false`,
 * because "unknown" is a different answer from "no" and the host needs both: it
 * decides whether a swap is worth making, and a source that declined to describe
 * its stream must not read as having said it is lossy.
 */
@Serializable
data class FormatDocument(
    val codec: String? = null,
    val kbps: Int? = null,
    val sampleRateHz: Int? = null,
    val bitDepth: Int? = null,
    val verified: Boolean = false,
    val isLossless: Boolean? = null,
    val isDolbyAtmos: Boolean = false,
    val summary: String = "",
)

/** A [SourceStream] as the host receives it. */
@Serializable
data class StreamDocument(
    val url: String,
    val format: FormatDocument,
    val headers: Map<String, String> = emptyMap(),
    val belowRequest: Boolean = false,
    val durationSec: Int? = null,
    val sourceConfigId: String? = null,
)

/** One configured source, for the sources screen. */
@Serializable
data class ConfigDocument(
    val id: String,
    val kind: String,
    val label: String,
    val displayName: String,
    val baseUrl: String,
    val enabled: Boolean,
    val isComplete: Boolean,
    val needsServer: Boolean,
    val canServeLossless: Boolean,
    val labels: List<String>,
    val refreshEnabled: Boolean = false,
    val refreshUrl: String = "",
    val refreshUrlUnavailable: Boolean = false,
)

@Serializable
data class ConfigListing(val sources: List<ConfigDocument>)

private fun FormatDocument.toFormat(): StreamFormat = StreamFormat(
    codec = codec,
    kbps = kbps,
    sampleRateHz = sampleRateHz,
    bitDepth = bitDepth,
    verified = verified,
)
