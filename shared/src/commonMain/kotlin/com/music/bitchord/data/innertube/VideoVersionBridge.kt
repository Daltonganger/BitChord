package com.music.bitchord.data.innertube

import com.music.bitchord.data.model.SearchFilter
import com.music.bitchord.data.model.SearchResult
import com.music.bitchord.data.model.Song
import com.music.bitchord.data.settings.PlatformSettings
import com.music.bitchord.data.sources.TrackMatcher
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlin.time.Clock

/** The catalogue music-video counterpart for a song's video toggle. */
object VideoVersionBridge {
    private const val CACHE_KEY = "video_version_bridge_cache_v2"
    private const val CACHE_CAPACITY = 128
    private const val POSITIVE_TTL_MS = 14L * 24 * 60 * 60 * 1000
    private const val NEGATIVE_TTL_MS = 24L * 60 * 60 * 1000

    private val bridgeScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val json = Json { encodeDefaults = true; ignoreUnknownKeys = true }
    private val cacheLock = Mutex()
    private val cache = linkedMapOf<String, CacheEntry>()
    private var cacheLoaded = false

    @Serializable
    data class Candidate(
        val videoId: String,
        val title: String,
        val durationText: String? = null,
        /** `MUSIC_VIDEO_TYPE_OMV`, `…_UGC`, … or null when the response omitted it. */
        val type: String? = null,
        /** `counterpart` is YouTube Music's explicit Song/Video pairing; otherwise `search`. */
        val source: String = "search",
    )

    @Serializable
    private data class CacheEntry(val savedAtMillis: Long, val candidates: List<Candidate>)

    fun interface VideoVersionCallback {
        /** [json] is the ranked candidate list (possibly empty), null on failure. */
        fun onResult(json: String?, message: String?)
    }

    /** Whole-word markers for uploads that are not a music video. */
    private val notAVideo = Regex(
        """\b(lyrics?|letras?|paroles|visuali[sz]ers?|official\s+audio|audio|karaoke|instrumental|cover|nightcore|slowed|sped\s+up|reverb|8d|fan[ -]+made|amv)\b""",
        RegexOption.IGNORE_CASE,
    )
    private val liveMarker = Regex("\\blive\\b", RegexOption.IGNORE_CASE)

    fun find(
        videoId: String,
        title: String,
        artist: String,
        durationText: String?,
        callback: VideoVersionCallback,
    ) {
        val generation = Innertube.sessionGeneration
        bridgeScope.launch {
            val known = cacheLock.withLock { cached(videoId, Clock.System.now().toEpochMilliseconds()) }
            if (known != null) {
                callback.onResult(encode(known), null)
                return@launch
            }
            try {
                Innertube.checkSession(generation)
                val counterpart = try {
                    parseCounterpart(Innertube.next(videoId), videoId)
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Throwable) {
                    null
                }
                val found = if (counterpart != null) {
                    listOf(counterpart)
                } else {
                    searchOfficialVideos(videoId, title, artist, durationText, generation)
                }
                Innertube.checkSession(generation)
                cacheLock.withLock { store(videoId, found, Clock.System.now().toEpochMilliseconds()) }
                callback.onResult(encode(found), null)
            } catch (e: Throwable) {
                callback.onResult(null, e.message ?: e.toString())
            }
        }
    }

    private suspend fun searchOfficialVideos(
        videoId: String,
        title: String,
        artist: String,
        durationText: String?,
        generation: Long,
    ): List<Candidate> {
        val target = TrackMatcher.targetOf(
            Song(videoId = videoId, title = title, artist = artist, durationText = durationText),
        )
        val found = mutableListOf<Candidate>()
        for (query in TrackMatcher.queries(target)) {
            Innertube.checkSession(generation)
            val response = Innertube.search(query, SearchFilter.VIDEOS.params)
            val types = mutableMapOf<String, String>()
            collectVideoTypes(response, types)
            val songs = InnertubeParser.parseSearchPage(response, true)
                .filterIsInstance<SearchResult.Track>()
                .map { it.song }
                .filter { it.videoId != videoId }
                .filter { types[it.videoId] == "MUSIC_VIDEO_TYPE_OMV" }
                .filterNot { isExcludedSearchTitle(it.title, title) }
            for (song in TrackMatcher.rankedOfficialVideos(songs, target)) {
                if (found.none { it.videoId == song.videoId }) {
                    found += Candidate(
                        videoId = song.videoId,
                        title = song.title,
                        durationText = song.durationText,
                        type = types[song.videoId],
                        source = "search",
                    )
                }
            }
            if (found.isNotEmpty()) break
        }
        return found.take(4)
    }

    private fun cached(videoId: String, now: Long): List<Candidate>? {
        loadCache()
        val entry = cache[videoId] ?: return null
        val ttl = if (entry.candidates.isEmpty()) NEGATIVE_TTL_MS else POSITIVE_TTL_MS
        if (now - entry.savedAtMillis in 0..ttl) return entry.candidates
        cache.remove(videoId)
        persistCache()
        return null
    }

    private fun store(videoId: String, candidates: List<Candidate>, now: Long) {
        loadCache()
        cache[videoId] = CacheEntry(now, candidates)
        val expired = cache.filter { (_, entry) ->
            val ttl = if (entry.candidates.isEmpty()) NEGATIVE_TTL_MS else POSITIVE_TTL_MS
            now - entry.savedAtMillis !in 0..ttl
        }.keys
        expired.forEach { cache.remove(it) }
        while (cache.size > CACHE_CAPACITY) {
            val oldest = cache.minByOrNull { it.value.savedAtMillis }?.key ?: break
            cache.remove(oldest)
        }
        persistCache()
    }

    private fun loadCache() {
        if (cacheLoaded) return
        cacheLoaded = true
        val encoded = PlatformSettings.getString(CACHE_KEY, "")
        if (encoded.isBlank()) return
        runCatching {
            json.decodeFromString(MapSerializer(String.serializer(), CacheEntry.serializer()), encoded)
        }.getOrNull()?.let(cache::putAll)
    }

    private fun persistCache() {
        PlatformSettings.putString(
            CACHE_KEY,
            json.encodeToString(MapSerializer(String.serializer(), CacheEntry.serializer()), cache),
        )
    }

    private fun encode(candidates: List<Candidate>): String =
        json.encodeToString(ListSerializer(Candidate.serializer()), candidates)

    internal fun isExcludedSearchTitle(candidateTitle: String, songTitle: String): Boolean =
        notAVideo.containsMatchIn(candidateTitle)
            || (!liveMarker.containsMatchIn(songTitle) && liveMarker.containsMatchIn(candidateTitle))

    /**
     * Reads YouTube Music's own queue-row Song/Video switch. Only the counterpart
     * on the row whose primary renderer is [sourceVideoId] can be authoritative.
     */
    internal fun parseCounterpart(next: JsonObject, sourceVideoId: String): Candidate? {
        for (wrapper in collectNamedObjects(next, "playlistPanelVideoWrapperRenderer")) {
            val primary = wrapper["primaryRenderer"] ?: wrapper["primary"] ?: continue
            val primaryVideo = collectNamedObjects(primary, "playlistPanelVideoRenderer")
                .firstOrNull()?.get("videoId")?.primitiveText()
                ?: (primary as? JsonObject)?.get("videoId")?.primitiveText()
            if (primaryVideo != sourceVideoId) continue
            val counterparts = wrapper["counterpart"] as? JsonArray ?: continue
            for (entry in counterparts) {
                val counterpartRenderer = (entry as? JsonObject)?.get("counterpartRenderer") ?: continue
                val renderer = collectNamedObjects(counterpartRenderer, "playlistPanelVideoRenderer").firstOrNull()
                    ?: continue
                val id = renderer["videoId"].primitiveText()?.takeIf { it.isNotBlank() } ?: continue
                if (id == sourceVideoId) continue
                val videoTypes = mutableMapOf<String, String>()
                collectVideoTypes(renderer, videoTypes)
                return Candidate(
                    videoId = id,
                    title = textOf(renderer["title"]).ifBlank { id },
                    durationText = textOf(renderer["lengthText"]).takeIf { it.isNotBlank() },
                    type = videoTypes[id],
                    source = "counterpart",
                )
            }
        }
        return null
    }

    private fun collectNamedObjects(root: JsonElement, name: String): List<JsonObject> = buildList {
        fun visit(element: JsonElement) {
            when (element) {
                is JsonObject -> {
                    (element[name] as? JsonObject)?.let { add(it) }
                    element.values.forEach(::visit)
                }
                is JsonArray -> element.forEach(::visit)
                else -> Unit
            }
        }
        visit(root)
    }

    private fun textOf(element: JsonElement?): String = when (element) {
        is JsonPrimitive -> element.contentOrNull.orEmpty()
        is JsonObject -> element["simpleText"]?.primitiveText()
            ?: (element["runs"] as? JsonArray)?.joinToString("") { run ->
                (run as? JsonObject)?.get("text").primitiveText().orEmpty()
            }.orEmpty()
        else -> ""
    }

    private fun JsonElement?.primitiveText(): String? = (this as? JsonPrimitive)?.contentOrNull

    /** Collects `{videoId, ...musicVideoType}` pairs wherever Innertube nested them. */
    private fun collectVideoTypes(element: JsonElement, into: MutableMap<String, String>) {
        when (element) {
            is JsonObject -> {
                val id = element["videoId"].primitiveText()
                val type = ((element["watchEndpointMusicSupportedConfigs"] as? JsonObject)
                    ?.get("watchEndpointMusicConfig") as? JsonObject)
                    ?.get("musicVideoType").primitiveText()
                if (id != null && type != null && id !in into) into[id] = type
                element.values.forEach { collectVideoTypes(it, into) }
            }
            is JsonArray -> element.forEach { collectVideoTypes(it, into) }
            else -> Unit
        }
    }
}
