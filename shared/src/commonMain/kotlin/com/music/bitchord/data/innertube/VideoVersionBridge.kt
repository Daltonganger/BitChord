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

/**
 * The music-video upload of a catalogue song, for the player's video toggle.
 *
 * Most songs on YouTube Music are art tracks: the upload's picture is the
 * square sleeve, and the music video is a different videoId. Upstream answers
 * the same question in `YtMusicRepository.resolveVideo` — search the Videos
 * filter with the matcher's queries and take [TrackMatcher.best] — and this is
 * that lookup, shaped for Swift. The matcher already demands the title core,
 * the version markers and a shared artist, and lets a music video's longer
 * runtime through only when the artist is credited.
 */
object VideoVersionBridge {

    private val bridgeScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /** Song videoId → the video version, or null once a search found none. */
    private val cache = mutableMapOf<String, Song?>()
    private val cacheLock = Mutex()

    fun interface VideoVersionCallback {
        /** [videoId] is null when there is no music video for this song. */
        fun onResult(videoId: String?, durationText: String?, message: String?)
    }

    fun find(videoId: String, title: String, artist: String, durationText: String?, callback: VideoVersionCallback) {
        val generation = Innertube.sessionGeneration
        bridgeScope.launch {
            val known = cacheLock.withLock { if (cache.containsKey(videoId)) Result.success(cache[videoId]) else null }
            if (known != null) {
                val song = known.getOrNull()
                callback.onResult(song?.videoId, song?.durationText, null)
                return@launch
            }
            try {
                val target = TrackMatcher.targetOf(
                    Song(videoId = videoId, title = title, artist = artist, durationText = durationText),
                )
                var match: Song? = null
                for (query in TrackMatcher.queries(target)) {
                    Innertube.checkSession(generation)
                    val response = Innertube.search(query, SearchFilter.VIDEOS.params)
                    val candidates = InnertubeParser.parseSearchPage(response, true)
                        .filterIsInstance<SearchResult.Track>()
                        .map { it.song }
                        .filter { it.videoId != videoId }
                    match = TrackMatcher.best(candidates, target)
                    if (match != null) break
                }
                Innertube.checkSession(generation)
                cacheLock.withLock { cache[videoId] = match }
                callback.onResult(match?.videoId, match?.durationText, null)
            } catch (e: Throwable) {
                callback.onResult(null, null, e.message ?: e.toString())
            }
        }
    }
}
