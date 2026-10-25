package com.music.bitchord.data.lyrics

/**
 * One stamped text run from a provider. Adjacent runs without a space can be
 * syllables of the same word; whitespace is the provider's word boundary.
 */
internal data class TimedRun(val startMs: Long, val endMs: Long, val raw: String)

/**
 * Reassembles provider syllables into words while keeping the first and last
 * run timings. CJK, kana and Thai stay one word per run because those scripts
 * do not use spaces as word boundaries. If a source wrote no whitespace at all,
 * missing spaces are not evidence that all runs form one word.
 */
internal fun wordsFromRuns(runs: List<TimedRun>): List<LyricWordDto> {
    val hasExplicitSpacing = runs.any { run -> run.raw.any(Char::isWhitespace) }
    val words = ArrayList<LyricWordDto>()
    val current = StringBuilder()
    var start = 0L
    var end = 0L

    fun flush() {
        if (current.isNotEmpty()) {
            words += LyricWordDto(start, maxOf(start, end), current.toString())
        }
        current.setLength(0)
        end = 0L
    }

    for (run in runs) {
        val content = run.raw.trim()
        if (content.isEmpty()) {
            flush()
            continue
        }
        val joinsPrevious = hasExplicitSpacing && current.isNotEmpty() &&
            !run.raw.first().isWhitespace() &&
            !current.last().isUnspacedScript() &&
            !content.first().isUnspacedScript()
        if (!joinsPrevious) flush()
        if (current.isEmpty()) start = run.startMs
        current.append(content)
        end = maxOf(end, run.endMs)
        if (run.raw.last().isWhitespace()) flush()
    }
    flush()
    return words
}

private fun Char.isUnspacedScript(): Boolean =
    this in '\u3400'..'\u9FFF' || // Han
        this in '\u3040'..'\u30FF' || // hiragana and katakana
        this in '\u3000'..'\u303F' || // CJK punctuation
        this in '\uFF00'..'\uFFEF' || // full-width forms
        this in '\u0E00'..'\u0E7F' // Thai
