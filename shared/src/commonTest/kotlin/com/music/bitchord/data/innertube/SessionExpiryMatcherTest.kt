package com.music.bitchord.data.innertube

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The host flips to signed-out only when a bridge failure is the store's
 * "session is gone" refusal. Matching it with a hand-copied substring in Swift
 * is exactly the kind of claim that goes stale the first time the exception's
 * copy changes, so the match lives in [AuthBridge] and is pinned here.
 */
class SessionExpiryMatcherTest {

    @Test
    fun theStoresOwnRefusalMatches() {
        val refusal = runCatching { throw Innertube.AuthenticationException() }
            .exceptionOrNull()?.message
        assertTrue(AuthBridge.isSessionExpired(refusal), "the exception's own message must match")
        assertTrue(
            AuthBridge.isSessionExpired("Your YouTube Music session has expired. Sign in again."),
            "the exact text the host receives must match",
        )
    }

    @Test
    fun caseAndSurroundingsDoNotMatter() {
        assertTrue(
            AuthBridge.isSessionExpired("YOUR YOUTUBE MUSIC SESSION HAS EXPIRED. SIGN IN AGAIN."),
            "the host must not depend on the exception's casing",
        )
    }

    @Test
    fun ordinaryFailuresDoNotMatch() {
        assertFalse(AuthBridge.isSessionExpired(null))
        assertFalse(AuthBridge.isSessionExpired(""))
        assertFalse(AuthBridge.isSessionExpired("Couldn't reach the music service. Check your connection and try again."))
        assertFalse(AuthBridge.isSessionExpired("Your saved session could not be verified. Try again when connected."))
    }
}
