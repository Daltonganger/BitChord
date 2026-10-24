package com.music.bitchord.data.innertube

import com.music.bitchord.data.model.SearchFilter
import com.music.bitchord.data.model.SearchResult
import com.music.bitchord.data.model.Song
import com.music.bitchord.data.sources.TrackMatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * The music-video upload of a catalogue song, for the player's video toggle.
 *
 * Most songs on YouTube Music are art tracks: the upload's picture is the
 * square sleeve, and the music video is a different videoId. Upstream answers
 * the same question in `YtMusicRepository.resolveVideo` — search the Videos
 * filter with the matcher's queries and take [TrackMatcher.best]. That alone
 * picks "Official Audio" uploads: a 16:9 file showing the cover for the whole
 * song, with the song's exact runtime, so it wins on duration. Here the
 * matcher's ranking is kept but filtered by what YouTube Music says the upload
 * *is* — its `musicVideoType`, official music videos first — and by the
 * title's own markers for audio, visualiser and lyric uploads. Swift gets the
 * ranked list and still checks each picture, because a label is not a picture.
 */
object VideoVersionBridge {

    private val bridgeScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val json = Json { encodeDefaults = true }

    /** Song videoId → its ranked music-video candidates (empty: none found). */
    private val cache = mutableMapOf<String, List<Candidate>>()
    private val cacheLock = Mutex()

    @Serializable
    data class Candidate(
        val videoId: String,
        val title: String,
        val durationText: String? = null,
        /** `MUSIC_VIDEO_TYPE_OMV`, `…_UGC`, … or null when the result said nothing. */
        val type: String? = null,
    )

    fun interface VideoVersionCallback {
        /** [json] is the ranked candidate list (possibly empty), null on failure. */
        fun onResult(json: String?, message: String?)
    }

    /** Title words that say the upload is the audio, not a music video. */
    private val notAVideo = Regex(
        """\b(official\s+audio|audio\s+only|\(audio\)|\[audio\]|visuali[sz]er|lyrics?\s+video|\(lyrics?\)|\[lyrics?\]|lyric\s+visualizer|static\s+video)\b""",
        RegexOption.IGNORE_CASE,
    )

    fun find(videoId: String, title: String, artist: String, durationText: String?, callback: VideoVersionCallback) {
        val generation = Innertube.sessionGeneration
        bridgeScope.launch {
            val known = cacheLock.withLock { cache[videoId] }
            if (known != null) {
                callback.onResult(json.encodeToString(ListSerializer(Candidate.serializer()), known), null)
                return@launch
            }
            try {
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
                        .filter { it.videoId != videoId && !notAVideo.containsMatchIn(it.title) }
                    val ranked = TrackMatcher.ranked(songs, target)
                        .filter { types[it.videoId] != "MUSIC_VIDEO_TYPE_ATV" }
                        // Stable: the matcher's order within each type.
                        .sortedBy { if (types[it.videoId] == "MUSIC_VIDEO_TYPE_OMV") 0 else 1 }
                    for (song in ranked) {
                        if (found.none { it.videoId == song.videoId }) {
                            found += Candidate(song.videoId, song.title, song.durationText, types[song.videoId])
                        }
                    }
                    if (found.any { it.type == "MUSIC_VIDEO_TYPE_OMV" }) break
                }
                val result = found.take(4)
                Innertube.checkSession(generation)
                cacheLock.withLock { cache[videoId] = result }
                callback.onResult(json.encodeToString(ListSerializer(Candidate.serializer()), result), null)
            } catch (e: Throwable) {
                callback.onResult(null, e.message ?: e.toString())
            }
        }
    }

    /**
     * Every `watchEndpoint` in a response that names its music video type:
     * `{videoId, watchEndpointMusicSupportedConfigs.watchEndpointMusicConfig.musicVideoType}`.
     */
    private fun collectVideoTypes(element: JsonElement, into: MutableMap<String, String>) {
        when (element) {
            is JsonObject -> {
                val id = (element["videoId"] as? JsonPrimitive)?.takeIf { it.isString }?.content
                val type = (element["watchEndpointMusicSupportedConfigs"] as? JsonObject)
                    ?.get("watchEndpointMusicConfig")?.let { it as? JsonObject }
                    ?.get("musicVideoType")?.let { it as? JsonPrimitive }?.content
                if (id != null && type != null && id !in into) into[id] = type
                element.values.forEach { collectVideoTypes(it, into) }
            }
            is JsonArray -> element.forEach { collectVideoTypes(it, into) }
            else -> Unit
        }
    }
}
