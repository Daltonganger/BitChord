package com.music.bitchord.data.lyrics

import kotlin.test.Test
import kotlin.test.assertEquals

class SyllableRunsTest {

    @Test
    fun enhanced_lrc_joins_syllables_until_the_written_space() {
        val lines = EnhancedLrc.parse(
            "[00:00.00]<00:00.00>hel<00:00.30>lo <00:00.50>world"
        )

        assertEquals("hello world", lines.single().text)
        assertEquals(listOf("hello", "world"), lines.single().words.map { it.text })
        assertEquals(0L, lines.single().words[0].startMs)
        assertEquals(500L, lines.single().words[1].startMs)
    }

    @Test
    fun qq_qrc_joins_syllables_until_the_written_space() {
        val lines = KaraokeLrc.parse("[0,1200](0,200)hel(200,300)lo (500,400)world")

        assertEquals("hello world", lines.single().text)
        assertEquals(listOf("hello", "world"), lines.single().words.map { it.text })
        assertEquals(0L, lines.single().words[0].startMs)
        assertEquals(500L, lines.single().words[1].startMs)
    }

    @Test
    fun ttml_joins_adjacent_syllables_and_keeps_inter_span_spacing() {
        val lines = TtmlLyrics.parse(
            """<tt><body><p begin="0s" end="2s"><span begin="0s" end="0.2s">hel</span><span begin="0.2s" end="0.4s">lo </span><span begin="0.5s" end="1s">world</span></p></body></tt>"""
        )

        assertEquals("hello world", lines.single().text)
        assertEquals(listOf("hello", "world"), lines.single().words.map { it.text })
        assertEquals(400L, lines.single().words[0].endMs)
    }

    @Test
    fun runs_without_explicit_spacing_stay_separate_including_cjk() {
        val western = wordsFromRuns(
            listOf(TimedRun(0, 100, "hel"), TimedRun(100, 200, "lo"))
        )
        val cjk = wordsFromRuns(
            listOf(TimedRun(0, 100, "日"), TimedRun(100, 200, "本"))
        )

        assertEquals(listOf("hel", "lo"), western.map { it.text })
        assertEquals(listOf("日", "本"), cjk.map { it.text })
    }
}
