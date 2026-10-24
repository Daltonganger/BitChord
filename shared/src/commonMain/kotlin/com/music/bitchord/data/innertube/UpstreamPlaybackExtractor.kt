package com.music.bitchord.data.innertube

import com.metrolist.innertubex.InnerTube
import com.metrolist.innertubex.InnerTubeLogger
import com.metrolist.innertubex.cipher.PlayerConfigRepository
import com.metrolist.innertubex.cipher.RemotePlayerConfigStore
import com.metrolist.innertubex.cipher.YouTubeCipherService
import com.metrolist.innertubex.extraction.AudioQuality
import com.metrolist.innertubex.extraction.ContentHints
import com.metrolist.innertubex.extraction.ExtractedStream
import com.metrolist.innertubex.extraction.InnerTubeExtractor
import com.metrolist.innertubex.extraction.generateClientPlaybackNonce
import com.music.bitchord.data.DebugLog
import com.music.bitchord.data.http.Http
import kotlin.concurrent.atomics.AtomicReference
import kotlin.concurrent.atomics.ExperimentalAtomicApi
import kotlin.time.TimeSource
import kotlin.time.TimeMark
import kotlin.time.Duration.Companion.minutes
import kotlinx.coroutines.*
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * What one video probe found. Every field is the extractor's own, not a
 * judgement; the caller decides what a picture means.
 *
 * `hasNParameter` is carried because the extractor silently refuses a URL that
 * has one (`InnerTubeExtractor.hasReadyVideoUrl`), so without it "no video" and
 * "video YouTube will throttle" look identical from outside.
 */
internal data class VideoProbe(
    val videoUrl: String?,
    val width: Int?,
    val height: Int?,
    val itag: Int?,
    val mimeType: String?,
    val codecs: String?,
    val bitrate: Int?,
    val availableVideoHeights: List<Int>,
    val hasNParameter: Boolean,
    val clientName: String,
    val profileId: String,
    val musicVideoType: String?,
    val selection: String,
)

/** Same maintained extraction stack as Android upstream, on the shared Darwin transport. */
@OptIn(ExperimentalAtomicApi::class)
internal object UpstreamPlaybackExtractor {
    /** Upstream's deck caps a music video at 1080p (videoDeck.ts `maxHeight`). */
    private const val MAX_VIDEO_HEIGHT = 1080

    /**
     * The visitor-backed profile the audio path actually resolves through.
     * Asking it for video needs an override because the video selection
     * refuses it (the catalog's note: "legacy direct playback verified for
     * normal audio only"), which is exactly the constraint the spike is about.
     */
    private const val AUDIO_DIRECT_CLIENT = "VISIONOS_0_1"

    /** The extractor's own n-parameter test, which is private there. */
    private val N_PARAMETER_REGEX = Regex("(?:[?&]|%26)n(?:=|%3[dD])", RegexOption.IGNORE_CASE)

    private class Session(val generation: Long, val innerTube: InnerTube, val extractor: InnerTubeExtractor, val cipher: YouTubeCipherService)
    private data class Issued(val id: String, val profile: String, val generation: Long)
    private val refreshScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val issued = AtomicReference<Map<String, Issued>>(emptyMap())
    private val excluded = AtomicReference<Map<String, TimeMark>>(emptyMap())
    fun onRefused(url: String, excludeClient: Boolean = false) {
        val origin = issued.load()[url] ?: return
        if (origin.generation != Innertube.sessionGeneration) return
        if (excludeClient) while (true) {
            val old = excluded.load()
            val next = (if (old.size >= 64) emptyMap() else old) + ("${origin.generation}:${origin.id}:${origin.profile}" to TimeSource.Monotonic.markNow())
            if (excluded.compareAndSet(old, next)) break
        }
        refreshScope.launch {
            lock.withLock { session?.takeIf { it.generation == origin.generation }?.cipher }
                ?.let { runCatching { it.refreshAfterStreamRejection() } }
        }
    }
    private val lock = Mutex()
    private var session: Session? = null
    private val logger = InnerTubeLogger { event ->
        // Library diagnostics may contain signed URLs. The shared redactor is
        // applied before anything reaches the system log; never print details maps.
        DebugLog.d("ITX ${event.tag}: ${event.message}")
    }
    private suspend fun current(): Session {
        val generation = Innertube.sessionGeneration
        if (Innertube.cookie != null) Innertube.ensureSessionScope()
        Innertube.checkSession(generation)
        return lock.withLock {
            session?.takeIf { it.generation == generation }?.let { return@withLock it }
            session?.let { it.innerTube.close(); it.cipher.dispose() }
            val snapshot = Innertube.requestSession()
            val tube = InnerTube(Http.client, logger = logger)
            tube.replaceSession(snapshot.cookie, snapshot.visitor, snapshot.dataSyncId,
                snapshot.authUser ?: "0", useLoginForBrowse = snapshot.cookie != null)
            val repository = object : PlayerConfigRepository {
                override val enabled = true
                override val sourceUrl = "https://raw.githubusercontent.com/ZemerTeam/zemer-cipher/master/library/src/main/assets/player_configs.json"
                override val defaultSourceUrl = sourceUrl
                override var cachedJson = ""
                override var cachedAtMs = 0L
                override var cachedSourceUrl = ""
                override var cachedEtag = ""
            }
            val store = RemotePlayerConfigStore(Http.client, repository, logger)
            val cipher = YouTubeCipherService(Http.client, store, logger)
            val extractor = InnerTubeExtractor(
                configParser = com.metrolist.innertubex.extraction.YtConfigParserImpl(Http.client, tube, store, cipherService = cipher),
                cipherService = cipher, innerTube = tube, tokenProvider = PlaybackTokenBridge.tokenProvider(), logger = logger)
            Session(generation, tube, extractor, cipher).also { session = it }
        }
    }
    suspend fun extract(videoId: String, maxKbps: Int): StreamResolver.ResolvedStream? {
        val stream = extractStream(videoId, maxKbps, wantVideo = false) ?: return null
        check(stream.sabrBootstrap == null) { "This audio engine requires a progressive stream" }
        while (true) {
            val old = issued.load()
            val next = (if (old.size >= 64) emptyMap() else old) + (stream.audioUrl to Issued(videoId, stream.profileId, Innertube.sessionGeneration))
            if (issued.compareAndSet(old, next)) break
        }
        DebugLog.d("$videoId: upstream ${stream.profileId} selected ${(stream.bitrate ?: 0) / 1000}kbps")
        return StreamResolver.ResolvedStream(stream.audioUrl, (stream.bitrate ?: 0) / 1000,
            stream.mimeType.orEmpty() + (stream.codecs?.let { "; codecs=\"$it\"" } ?: ""),
            stream.headers, stream.loudnessDb, stream.mediaMetadata?.durationSeconds?.takeIf { it > 0 })
    }

    /**
     * Ask for the video side of a track without changing what playback asks for.
     *
     * The profile the audio path actually plays through is asked first, by
     * override: the automatic video selection prefers SABR profiles, and this
     * engine needs a progressive stream, so the automatic answer would say
     * "no video" about a path playback cannot use anyway. Only if that direct
     * profile answers without video is the automatic selection tried, for the
     * report. A null answer means neither delivered a video-only stream.
     */
    suspend fun probeVideo(videoId: String): VideoProbe? {
        val direct = probeAttempt(videoId, AUDIO_DIRECT_CLIENT)
        if (direct?.videoUrl != null) return direct
        val automatic = probeAttempt(videoId, clientOverride = null)
        return automatic ?: direct
    }

    private suspend fun probeAttempt(videoId: String, clientOverride: String?): VideoProbe? =
        try {
            extractStream(videoId, Int.MAX_VALUE, wantVideo = true, clientOverride = clientOverride)?.let { stream ->
                val videoUrl = stream.videoUrl
                VideoProbe(
                    videoUrl = videoUrl,
                    width = stream.videoWidth,
                    height = stream.videoHeight,
                    itag = stream.videoItag,
                    mimeType = stream.videoMimeType,
                    codecs = stream.videoCodecs,
                    bitrate = stream.videoBitrate,
                    availableVideoHeights = stream.availableVideoHeights,
                    hasNParameter = videoUrl?.let { N_PARAMETER_REGEX.containsMatchIn(it) } == true,
                    clientName = stream.clientName,
                    profileId = stream.profileId,
                    musicVideoType = stream.mediaMetadata?.musicVideoType,
                    selection = clientOverride?.let { "direct profile $it (override)" } ?: "automatic selection",
                )
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            DebugLog.d("$videoId: video probe via ${clientOverride ?: "automatic selection"} failed: ${e.message}")
            null
        }

    private suspend fun extractStream(
        videoId: String,
        maxKbps: Int,
        wantVideo: Boolean,
        clientOverride: String? = null,
    ): ExtractedStream? {
        val session = current()
        val stream = session.extractor.extract(
            videoId = videoId,
            hints = ContentHints(
                wantVideo = wantVideo,
                playbackClientOverrideId = clientOverride,
                maxVideoHeight = if (wantVideo) MAX_VIDEO_HEIGHT else null,
            ).withStreamCapabilities(allowHls = false, allowSabr = false, allowBoundedRange = true),
            excludedClients = excluded.load().filter { (key, at) ->
                key.startsWith("${session.generation}:$videoId:") && at.elapsedNow() < 5.minutes
            }.keys.map { it.substringAfterLast(":") }.toSet(),
            audioQuality = if (maxKbps <= 64) AudioQuality.LOW else AudioQuality.AUTO,
            clientPlaybackNonce = generateClientPlaybackNonce()) ?: return null
        Innertube.checkSession(session.generation)
        return stream
    }
}
