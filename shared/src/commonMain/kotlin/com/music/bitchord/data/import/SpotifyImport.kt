package com.music.bitchord.data.import

import com.music.bitchord.data.canvas.SpotifyToken
import com.music.bitchord.data.http.Http
import com.music.bitchord.data.innertube.Innertube
import com.music.bitchord.data.innertube.InnertubeParser
import com.music.bitchord.data.model.SearchResult
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlin.math.abs

/**
 * Importing a Spotify playlist into BitChord.
 *
 * Spotify's share links are the way in, its Web API is the source, and YouTube
 * Music's own search is how each track is found again. The session is the
 * `sp_dc` cookie the Canvas feature already stores — the same cookie mints the
 * bearer token, so nothing new has to be signed into.
 *
 * The import is deliberately two halves. [loadPlaylist] reads the playlist, and
 * [matchTrack] finds one track's counterpart. The host calls the second half a
 * row at a time, which is what lets it show per-row progress and lets the search
 * run beside the reading instead of after it.
 *
 * Everything that decides a match ([bestMatch]) is pure, so the rules are
 * testable without a network.
 */
object SpotifyImport {

    private const val API = "https://api.spotify.com/v1"
    private const val PAGE = 100
    private const val PLAYLIST_PAGE = 50
    private const val UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"
    private const val BROWSER_UA =
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15"

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    /** One row of a Spotify playlist, as the API describes it. */
    data class Track(
        val title: String,
        val artists: String,
        val durationMs: Long,
        val album: String?,
    )

    /** A loaded playlist: what it is called and everything in it. */
    data class Playlist(val id: String, val name: String, val tracks: List<Track>)

    /** The playlists in the signed-in Spotify account, for the picker. */
    data class Summary(val id: String, val name: String, val trackCount: Int)

    /** A YouTube Music search hit, stripped to what matching needs. */
    data class Candidate(
        val videoId: String,
        val title: String,
        val artist: String,
        val durationText: String?,
        val thumbnailUrl: String?,
    )

    enum class Status { MATCHED, AMBIGUOUS, MISSING }

    data class Match(
        val status: Status,
        val videoId: String? = null,
        val title: String? = null,
        val artist: String? = null,
        val durationText: String? = null,
        val thumbnailUrl: String? = null,
        val confidence: Double = 0.0,
    )

    // ---- Reading ------------------------------------------------------------

    /**
     * The playlist id in a share link, a `spotify:` URI or a bare id.
     *
     * Returns null for anything that is not a playlist — an album or a track
     * link is a different thing, and importing one as a playlist would produce
     * a one-row playlist nobody asked for.
     */
    fun playlistId(input: String): String? {
        val value = input.trim()
        if (value.isEmpty()) return null
        URI.find(value)?.let { return it.groupValues[1] }
        LINK.find(value)?.let { return it.groupValues[1] }
        return value.takeIf { BARE.matches(it) }
    }

    private val URI = Regex("""spotify:playlist:([A-Za-z0-9]{22})""", RegexOption.IGNORE_CASE)
    private val LINK = Regex(
        """open\.spotify\.com/(?:intl-[a-z]{2}(?:-[a-z]{2})?/)?playlist/([A-Za-z0-9]{22})""",
        RegexOption.IGNORE_CASE,
    )
    private val BARE = Regex("""[A-Za-z0-9]{22}""")

    /** The account's playlists. Empty when the cookie can't mint a token. */
    suspend fun loadPlaylists(): List<Summary> {
        val out = mutableListOf<Summary>()
        var offset = 0
        while (true) {
            val page = api("$API/me/playlists", mapOf("limit" to "$PLAYLIST_PAGE", "offset" to "$offset"))
            val items = page["items"]?.jsonArray ?: break
            if (items.isEmpty()) break
            items.mapNotNull { it as? JsonObject }.forEach { entry ->
                val id = entry["id"]?.jsonPrimitive?.contentOrNull ?: return@forEach
                val name = entry["name"]?.jsonPrimitive?.contentOrNull ?: return@forEach
                val count = (entry["tracks"] as? JsonObject)
                    ?.get("total")?.jsonPrimitive?.contentOrNull?.toIntOrNull() ?: 0
                out += Summary(id, name, count)
            }
            offset += items.size
            if (items.size < PLAYLIST_PAGE) break
        }
        return out
    }

    /**
     * Every track in a playlist, not the first page of it.
     *
     * With a session this walks the Web API's pages of 100, so a playlist of
     * 400 tracks is four requests and the count the host shows is the real one.
     * Without one the public web player's own page is read instead — a share
     * link has to work for someone who never set up a cookie, and what that page
     * carries is the whole list Spotify itself shows.
     */
    suspend fun loadPlaylist(id: String): Playlist =
        if (SpotifyToken.accessToken() != null) loadPlaylistFromApi(id) else loadPublicPlaylist(id)

    private suspend fun loadPlaylistFromApi(id: String): Playlist {
        val meta = api("$API/playlists/$id")
        val name = meta["name"]?.jsonPrimitive?.contentOrNull?.takeIf { it.isNotBlank() } ?: "Spotify playlist"
        val tracks = mutableListOf<Track>()
        var offset = 0
        while (true) {
            val page = api("$API/playlists/$id/tracks", mapOf("limit" to "$PAGE", "offset" to "$offset"))
            val items = page["items"]?.jsonArray ?: break
            if (items.isEmpty()) break
            items.forEach { item ->
                val track = (item as? JsonObject)?.get("track") as? JsonObject ?: return@forEach
                parseTrack(track)?.let(tracks::add)
            }
            offset += items.size
            if (items.size < PAGE) break
        }
        return Playlist(id, name, tracks)
    }

    private fun parseTrack(track: JsonObject): Track? {
        // A podcast episode lives in a playlist next to songs and is not
        // something YouTube Music's song search can (or should) match.
        if (track["type"]?.jsonPrimitive?.contentOrNull == "episode") return null
        if (track["is_local"]?.jsonPrimitive?.booleanOrNull == true) {
            // A local file has no catalogue entry on either side; the name and
            // artist are all it can offer, and they are still worth a search.
            if (track["name"]?.jsonPrimitive?.contentOrNull.isNullOrBlank()) return null
        }
        val title = track["name"]?.jsonPrimitive?.contentOrNull?.trim().orEmpty()
        if (title.isEmpty()) return null
        val artists = track["artists"]?.jsonArray
            ?.mapNotNull { (it as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull }
            ?.joinToString(", ")
            .orEmpty()
        val durationMs = track["duration_ms"]?.jsonPrimitive?.longOrNull ?: 0L
        val album = (track["album"] as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
        return Track(title, artists, durationMs, album)
    }

    /**
     * A playlist read from its public web-player page.
     *
     * `__NEXT_DATA__` carries the page's own track list — title, artist and
     * length — which is enough to match against YouTube Music. Private playlists
     * are not on that page at all, which is one of the things the cookie buys.
     */
    private suspend fun loadPublicPlaylist(id: String): Playlist {
        val response = Http.getRaw(
            "https://open.spotify.com/embed/playlist/$id",
            headers = mapOf("User-Agent" to BROWSER_UA, "Accept" to "text/html"),
        )
        if (response.status !in 200..299) {
            error("That playlist could not be read. Add the sp_dc cookie in Settings to import private playlists.")
        }
        val html = response.body ?: error("Spotify sent an empty page.")
        val payload = Regex(
            """<script id="__NEXT_DATA__" type="application/json">(.*?)</script>""",
            RegexOption.DOT_MATCHES_ALL,
        ).find(html)?.groupValues?.get(1)
            ?: error("Spotify's page did not contain a track list. Add the sp_dc cookie in Settings and try again.")
        val entity = findEntity(json.parseToJsonElement(payload))
            ?: error("That link is not a public Spotify playlist.")
        val name = entity["name"]?.jsonPrimitive?.contentOrNull?.takeIf { it.isNotBlank() } ?: "Spotify playlist"
        val tracks = (entity["trackList"] as? kotlinx.serialization.json.JsonArray)
            ?.mapNotNull { item ->
                val track = item as? JsonObject ?: return@mapNotNull null
                val title = track["title"]?.jsonPrimitive?.contentOrNull?.trim().orEmpty()
                if (title.isEmpty()) return@mapNotNull null
                Track(
                    title = title,
                    artists = track["subtitle"]?.jsonPrimitive?.contentOrNull.orEmpty(),
                    durationMs = track["duration"]?.jsonPrimitive?.longOrNull ?: 0L,
                    album = null,
                )
            }
            .orEmpty()
        return Playlist(id, name, tracks)
    }

    /** The object anywhere in a page's JSON that carries a `trackList`. */
    private fun findEntity(element: kotlinx.serialization.json.JsonElement): JsonObject? {
        val obj = element as? JsonObject
        if (obj == null) {
            (element as? kotlinx.serialization.json.JsonArray)?.forEach { child ->
                findEntity(child)?.let { return it }
            }
            return null
        }
        if (obj["trackList"] != null) return obj
        obj.values.forEach { child -> findEntity(child)?.let { return it } }
        return null
    }

    private suspend fun api(url: String, query: Map<String, String> = emptyMap()): JsonObject {
        val token = SpotifyToken.accessToken()
            ?: error("Add your Spotify sp_dc cookie in Settings › Spotify Canvas to import from Spotify.")
        val response = Http.getRaw(
            url,
            headers = mapOf(
                "Authorization" to "Bearer $token",
                "Accept" to "application/json",
                "User-Agent" to UA,
            ),
            query = query,
        )
        if (response.status == 401 || response.status == 403) {
            error("Spotify refused the session. Update the sp_dc cookie in Settings and try again.")
        }
        if (response.status !in 200..299) error("Spotify answered ${response.status}.")
        val body = response.body ?: error("Spotify sent an empty answer.")
        return json.parseToJsonElement(body).jsonObject
    }

    // ---- Matching -----------------------------------------------------------

    /** Searches YouTube Music for one track and picks its counterpart. */
    suspend fun matchTrack(track: Track): Match = bestMatch(track, searchCandidates(track))

    suspend fun searchCandidates(track: Track): List<Candidate> {
        val query = listOf(track.title, track.artists)
            .filter { it.isNotBlank() }
            .joinToString(" ")
        val response = Innertube.search(query, InnertubeParser.paramsFor("songs"))
        return InnertubeParser.parseSearchPage(response)
            .mapNotNull { result ->
                when (result) {
                    is SearchResult.Track -> result.song
                    is SearchResult.TopTrack -> result.song
                    is SearchResult.Browse -> null
                }
            }
            .distinctBy { it.videoId }
            .map { Candidate(it.videoId, it.title, it.artist, it.durationText, it.thumbnailUrl) }
    }

    /**
     * The best counterpart among [candidates], and how sure it is.
     *
     * A Spotify row and a YouTube row describe the same recording from
     * different catalogues, so the evidence is the same three things a person
     * would check: the title (ignoring "(Remastered 2011)" and the like), the
     * artist, and the length. Title carries the most weight because a cover
     * band can match a title exactly and an artist exactly and still be a
     * different recording — the length is what separates those.
     */
    fun bestMatch(source: Track, candidates: List<Candidate>): Match {
        if (candidates.isEmpty()) return Match(Status.MISSING)

        val scored = candidates.map { candidate ->
            val title = titleScore(source.title, candidate.title)
            val artist = artistScore(source.artists, candidate.artist)
            val duration = durationScore(source.durationMs, seconds(candidate.durationText))
            Scored(candidate, title, 0.55 * title + 0.30 * artist + 0.15 * duration, artist, duration)
        }.sortedByDescending { it.confidence }

        fun matchOf(scored: Scored, status: Status) = Match(
            status = status,
            videoId = scored.candidate.videoId,
            title = scored.candidate.title,
            artist = scored.candidate.artist,
            durationText = scored.candidate.durationText,
            thumbnailUrl = scored.candidate.thumbnailUrl,
            confidence = scored.confidence,
        )

        val best = scored.first()
        val runnerUp = scored.getOrNull(1)
        // Two candidates the same distance away are not a match to guess at:
        // they may be two editions, two uploads, or two different songs.
        val tied = runnerUp != null &&
            best.confidence - runnerUp.confidence < TIE_MARGIN &&
            runnerUp.confidence >= MATCH_CONFIDENCE

        // A known length that disagrees by more than the tolerance is the one
        // piece of evidence that can overrule a perfect title and artist: a
        // cover, a live cut or a different edit all look identical otherwise.
        val durationsDisagree = source.durationMs > 0 &&
            seconds(best.candidate.durationText) != null &&
            best.durationScore <= 0.0

        return when {
            best.confidence >= MATCH_CONFIDENCE && best.titleScore >= TITLE_CONFIDENCE &&
                !tied && !durationsDisagree ->
                matchOf(best, Status.MATCHED)
            best.confidence >= PLAUSIBLE_CONFIDENCE || tied || durationsDisagree ->
                matchOf(best, Status.AMBIGUOUS)
            else -> Match(Status.MISSING)
        }
    }

    private data class Scored(
        val candidate: Candidate,
        val titleScore: Double,
        val confidence: Double,
        val artistScore: Double,
        val durationScore: Double,
    )

    private const val MATCH_CONFIDENCE = 0.78
    private const val PLAUSIBLE_CONFIDENCE = 0.50
    private const val TITLE_CONFIDENCE = 0.80
    private const val TIE_MARGIN = 0.04
    private const val DURATION_TOLERANCE_SECONDS = 10.0

    private fun titleScore(wanted: String, candidate: String): Double {
        val a = core(wanted)
        val b = core(candidate)
        if (a.isEmpty() || b.isEmpty()) return 0.0
        if (a == b) {
            // "Song (Live)" and "Song" are the same words and different
            // recordings; the version words have to agree before it is a match.
            return if (version(wanted) == version(candidate)) 1.0 else 0.6
        }
        val at = a.split(' ').filter { it.isNotEmpty() }.toSet()
        val bt = b.split(' ').filter { it.isNotEmpty() }.toSet()
        if (at.isEmpty() || bt.isEmpty()) return 0.0
        val common = at.intersect(bt).size
        return common.toDouble() / maxOf(at.size, bt.size) * 0.9
    }

    private fun artistScore(wanted: String, candidate: String): Double {
        val wantedParts = artistParts(wanted)
        if (wantedParts.isEmpty()) return 0.6   // nothing to judge on: stay neutral
        val candidateParts = artistParts(candidate)
        if (candidateParts.isEmpty()) return 0.0
        // A Spotify row lists every credit; YouTube often lists the first. One
        // shared credit is enough — that is how a feature is credited on both.
        return if (wantedParts.any { it in candidateParts }) 1.0 else 0.0
    }

    private fun durationScore(wantedMs: Long, candidateSeconds: Double?): Double {
        if (wantedMs <= 0 || candidateSeconds == null || candidateSeconds <= 0) return 0.6
        val delta = abs(wantedMs / 1000.0 - candidateSeconds)
        return when {
            delta <= 2 -> 1.0
            delta <= 5 -> 0.7
            delta <= DURATION_TOLERANCE_SECONDS -> 0.4
            else -> 0.0
        }
    }

    /** "3:45" → 225.0, "1:02:03" → 3723.0. */
    private fun seconds(text: String?): Double? {
        if (text.isNullOrBlank()) return null
        val parts = text.split(':').map { it.trim().toIntOrNull() ?: return null }
        return when (parts.size) {
            2 -> parts[0] * 60.0 + parts[1]
            3 -> parts[0] * 3600.0 + parts[1] * 60.0 + parts[2]
            else -> null
        }
    }

    private val BRACKETED = Regex("""[\(\[\{][^\)\]\}]*[\)\]\}]""")
    private val VERSION_WORDS = setOf(
        "live", "remix", "remixed", "acoustic", "instrumental", "demo", "edit",
        "mix", "radio", "extended", "mono", "stereo", "sped", "slowed",
    )
    private val ARTIST_SEPARATORS = Regex("""[,&/]|\bfeat\.?\b|\bft\.?\b|\bwith\b""", RegexOption.IGNORE_CASE)
    private val ACCENTS = mapOf(
        'à' to 'a', 'á' to 'a', 'â' to 'a', 'ã' to 'a', 'ä' to 'a', 'å' to 'a',
        'è' to 'e', 'é' to 'e', 'ê' to 'e', 'ë' to 'e',
        'ì' to 'i', 'í' to 'i', 'î' to 'i', 'ï' to 'i',
        'ò' to 'o', 'ó' to 'o', 'ô' to 'o', 'õ' to 'o', 'ö' to 'o', 'ø' to 'o',
        'ù' to 'u', 'ú' to 'u', 'û' to 'u', 'ü' to 'u',
        'ñ' to 'n', 'ç' to 'c', 'ß' to 's', 'æ' to 'a', 'œ' to 'o',
    )

    /** "Song (Remastered 2011) - Live" → "song". */
    private fun core(title: String): String =
        normalized(title.replace(BRACKETED, " ").substringBefore(" - ")).ifEmpty { normalized(title) }

    /** The edition words a title names, which have to agree to be the same cut. */
    private fun version(title: String): Set<String> {
        val extras = BRACKETED.findAll(title).joinToString(" ") { it.value } +
            " " + title.substringAfter(" - ", "")
        return normalized(extras).split(' ').filter { it in VERSION_WORDS }.toSet()
    }

    private fun artistParts(value: String): Set<String> =
        value.split(ARTIST_SEPARATORS)
            .map { normalized(it) }
            .filter { it.length > 1 }
            .toSet()

    /** Lower case, accents folded, only letters and digits, single-spaced. */
    private fun normalized(value: String): String =
        value.lowercase()
            .map { ACCENTS[it] ?: it }
            .map { if (it.isLetterOrDigit()) it else ' ' }
            .joinToString("")
            .split(' ')
            .filter { it.isNotEmpty() }
            .joinToString(" ")
}
