package com.music.bitchord.data.sources

import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext

/** Request-local, URL-free reasons shared by concurrent providers. */
internal class SourceResolutionReport : AbstractCoroutineContextElement(Key) {
    companion object Key : CoroutineContext.Key<SourceResolutionReport> {
        suspend fun record(reason: String) { currentCoroutineContext()[Key]?.reasons?.update { it + reason } }
    }
    private val reasons = MutableStateFlow<Set<String>>(emptySet())
    fun summary(): String? = when {
        "provider_lossy" in reasons.value -> "The provider supplied compressed audio instead of Lossless."
        "verification_failed" in reasons.value -> "A preferred source could not be verified by the audio decoder in time."
        "timeout" in reasons.value -> "Preferred sources did not answer within the lookup window."
        "no_match" in reasons.value -> "No matching recording was found on the preferred sources."
        else -> null
    }
}
