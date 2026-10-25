package com.music.bitchord.data.import

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The rules the Spotify importer runs on, without a network.
 *
 * The URL cases are the shapes a link actually arrives in — the share sheet,
 * the copy-link menu, a `spotify:` URI from another app, or a bare id pasted
 * out of one. The matching cases are the ones that used to be decided by eye:
 * a remaster, a live cut, a cover, and two uploads the same distance away.
 */
class SpotifyImportTest {

    // ---- Links -------------------------------------------------------------

    @Test
    fun readsEveryShapeOfPlaylistLink() {
        val id = "37i9dQZF1DXcBWIGoYBM5M"
        listOf(
            "https://open.spotify.com/playlist/$id",
            "https://open.spotify.com/playlist/$id?si=abc123&pt=def",
            "http://open.spotify.com/playlist/$id",
            "https://open.spotify.com/intl-de/playlist/$id",
            "https://open.spotify.com/intl-pt-BR/playlist/$id?si=1",
            "spotify:playlist:$id",
            "  $id  ",
        ).forEach { input ->
            assertEquals(id, SpotifyImport.playlistId(input), "could not read $input")
        }
    }

    @Test
    fun refusesWhatIsNotAPlaylist() {
        listOf(
            "",
            "   ",
            "https://open.spotify.com/album/4aawyAB9vmqN3uQ7FjRGTy",
            "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT",
            "https://open.spotify.com/artist/06HL4z0CvFAxyc27GXpf02",
            "spotify:album:4aawyAB9vmqN3uQ7FjRGTy",
            "not a link",
            "https://open.spotify.com/playlist/short",
        ).forEach { input ->
            assertNull(SpotifyImport.playlistId(input), "accepted $input")
        }
    }

    // ---- Matching ----------------------------------------------------------

    private fun track(
        title: String = "Yellow",
        artists: String = "Coldplay",
        durationMs: Long = 4 * 60_000 + 29_000,
    ) = SpotifyImport.Track(title, artists, durationMs, "Parachutes")

    private fun candidate(
        videoId: String = "vid",
        title: String = "Yellow",
        artist: String = "Coldplay",
        duration: String? = "4:29",
    ) = SpotifyImport.Candidate(videoId, title, artist, duration, null)

    @Test
    fun anExactRowMatches() {
        val match = SpotifyImport.bestMatch(track(), listOf(candidate()))
        assertEquals(SpotifyImport.Status.MATCHED, match.status)
        assertEquals("vid", match.videoId)
    }

    @Test
    fun aRemasterIsTheSameCut() {
        val match = SpotifyImport.bestMatch(
            track(),
            listOf(candidate(title = "Yellow (Remastered)", duration = "4:26")),
        )
        assertEquals(SpotifyImport.Status.MATCHED, match.status)
    }

    @Test
    fun aDifferentEditIsNotAMatch() {
        // Same words, different recording: the version words disagree and the
        // lengths are a minute apart. It stays on the list as a choice, but it
        // must never be taken silently as the match.
        val match = SpotifyImport.bestMatch(
            track(),
            listOf(candidate(title = "Yellow (Live at Glastonbury)", duration = "5:51")),
        )
        assertEquals(SpotifyImport.Status.AMBIGUOUS, match.status)
    }

    @Test
    fun anUnrelatedTitleIsMissing() {
        val match = SpotifyImport.bestMatch(
            track(),
            listOf(candidate(videoId = "other", title = "Blue Monday", artist = "New Order")),
        )
        assertEquals(SpotifyImport.Status.MISSING, match.status)
    }

    @Test
    fun theWrongArtistIsNotAMatch() {
        val match = SpotifyImport.bestMatch(
            track(),
            listOf(candidate(videoId = "cover", title = "Yellow", artist = "A Tribute Band")),
        )
        assertTrue(match.status != SpotifyImport.Status.MATCHED, "matched a cover band: ${match.status}")
    }

    @Test
    fun aMuchLongerVideoIsNotAMatch() {
        val match = SpotifyImport.bestMatch(
            track(durationMs = 200_000),
            listOf(candidate(duration = "3:20 (music video)")),
        )
        // An unreadable length is neutral, not evidence either way.
        assertEquals(SpotifyImport.Status.MATCHED, match.status)

        val video = SpotifyImport.bestMatch(
            track(durationMs = 200_000),
            listOf(candidate(duration = "4:45")),
        )
        assertTrue(video.status != SpotifyImport.Status.MATCHED, "matched a 4:45 video to a 3:20 track")
    }

    @Test
    fun twoEvenlyGoodRowsAreAmbiguous() {
        val match = SpotifyImport.bestMatch(
            track(),
            listOf(candidate(videoId = "a"), candidate(videoId = "b"))
        )
        assertEquals(SpotifyImport.Status.AMBIGUOUS, match.status)
    }

    @Test
    fun aFeaturedArtistIsEnough() {
        val match = SpotifyImport.bestMatch(
            track(title = "Stay", artists = "Kid LAROI, Justin Bieber", durationMs = 201_000),
            listOf(candidate(title = "Stay", artist = "The Kid LAROI, Justin Bieber", duration = "3:21")),
        )
        assertEquals(SpotifyImport.Status.MATCHED, match.status)
    }

    @Test
    fun anEmptySearchIsMissing() {
        val match = SpotifyImport.bestMatch(track(), emptyList())
        assertEquals(SpotifyImport.Status.MISSING, match.status)
        assertNull(match.videoId)
    }

    @Test
    fun accentsAndPunctuationDoNotBreakTitleMatching() {
        val match = SpotifyImport.bestMatch(
            track(title = "Déjà Vu", artists = "Beyoncé", durationMs = 213_000),
            listOf(candidate(title = "Deja Vu", artist = "Beyonce", duration = "3:33")),
        )
        assertEquals(SpotifyImport.Status.MATCHED, match.status)
    }

    @Test
    fun aPathfinderPageReadsTracksLengthsAndTheTotal() {
        val body = """
            {"data":{"playlistV2":{"content":{"totalCount":150,"items":[
              {"itemV2":{"data":{"name":"Lucid Dreams","trackDuration":{"totalMilliseconds":239835},
                "albumOfTrack":{"name":"Goodbye & Good Riddance"},
                "artists":{"items":[{"profile":{"name":"Juice WRLD"}}]}}}},
              {"itemV2":{"data":{"name":"  ","artists":{"items":[]}}}},
              {"itemV2":{"data":{"name":"Love Me Like You Do",
                "artists":{"items":[{"profile":{"name":"Ellie Goulding"}},{"profile":{"name":"Someone"}}]}}}}
            ]}}}}
        """.trimIndent()
        val (page, total) = SpotifyImport.parsePathfinderPage(body)
        assertEquals(150, total)
        // Three rows were read, even though one had no title, so paging advances by three.
        assertEquals(3, page.read)
        assertEquals(listOf("Lucid Dreams", "Love Me Like You Do"), page.tracks.map { it.title })
        assertEquals(239_835L, page.tracks[0].durationMs)
        assertEquals("Goodbye & Good Riddance", page.tracks[0].album)
        assertEquals("Ellie Goulding, Someone", page.tracks[1].artists)
    }

    @Test
    fun aRefusedPathfinderPageEndsPaging() {
        val (page, total) = SpotifyImport.parsePathfinderPage("""{"errors":[{"message":"denied"}]}""")
        assertEquals(0, page.read)
        assertNull(total)
    }
}
