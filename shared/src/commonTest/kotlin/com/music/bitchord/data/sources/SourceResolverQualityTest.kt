package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.AppSettings
import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SourceResolverQualityTest {
    @Test
    fun `upgrade identity uses the configured length slack`() {
        val previous = AppSettings.upgradeLengthSlackSeconds.value
        try {
            AppSettings.setUpgradeLengthSlackSeconds(3)
            assertTrue(SourceResolver.sameRecordingAs(203, 200))
            assertFalse(SourceResolver.sameRecordingAs(204, 200))
            AppSettings.setUpgradeLengthSlackSeconds(5)
            assertTrue(SourceResolver.sameRecordingAs(205, 200))
            assertFalse(SourceResolver.sameRecordingAs(206, 200))
        } finally {
            AppSettings.setUpgradeLengthSlackSeconds(previous)
        }
    }

    @Test
    fun `unknown duration never proves recording identity`() {
        assertFalse(SourceResolver.sameRecordingAs(null, 200))
        assertFalse(SourceResolver.sameRecordingAs(200, null))
    }
}
