package com.music.bitchord.data.settings

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class UpgradeLengthSlackTest {
    @Test
    fun `slack is bounded persisted and included in settings backup`() {
        val old = AppSettings.upgradeLengthSlackSeconds.value
        try {
            assertEquals(3, AppSettings.DEFAULT_UPGRADE_LENGTH_SLACK_SECONDS)
            assertEquals(0, AppSettings.MIN_UPGRADE_LENGTH_SLACK_SECONDS)
            assertEquals(10, AppSettings.MAX_UPGRADE_LENGTH_SLACK_SECONDS)
            AppSettings.setUpgradeLengthSlackSeconds(-2)
            assertEquals(AppSettings.MIN_UPGRADE_LENGTH_SLACK_SECONDS, AppSettings.upgradeLengthSlackSeconds.value)

            AppSettings.setUpgradeLengthSlackSeconds(12)
            assertEquals(AppSettings.MAX_UPGRADE_LENGTH_SLACK_SECONDS, AppSettings.upgradeLengthSlackSeconds.value)

            AppSettings.setUpgradeLengthSlackSeconds(7)
            val backup = AppSettings.exportPrefsJson()
            assertTrue(backup.contains("\"upgrade_length_slack_seconds\":\"7\""))

            AppSettings.setUpgradeLengthSlackSeconds(1)
            AppSettings.importPrefsJson(backup)
            assertEquals(7, AppSettings.upgradeLengthSlackSeconds.value)
        } finally {
            AppSettings.setUpgradeLengthSlackSeconds(old)
        }
    }

    @Test
    fun `player settings are included in backup and restore`() {
        val oldSpeaker = AppSettings.loudnessOffOnSpeaker.value
        val oldStatus = AppSettings.hideStatusBarInPlayer.value
        val oldQueue = AppSettings.showQueueHistory.value
        val oldClose = AppSettings.stopMusicOnClose.value
        try {
            AppSettings.setLoudnessOffOnSpeaker(false)
            AppSettings.setHideStatusBarInPlayer(true)
            AppSettings.setShowQueueHistory(true)
            AppSettings.setStopMusicOnClose(true)
            val backup = AppSettings.exportPrefsJson()

            AppSettings.setLoudnessOffOnSpeaker(true)
            AppSettings.setHideStatusBarInPlayer(false)
            AppSettings.setShowQueueHistory(false)
            AppSettings.setStopMusicOnClose(false)
            AppSettings.importPrefsJson(backup)

            assertFalse(AppSettings.loudnessOffOnSpeaker.value)
            assertTrue(AppSettings.hideStatusBarInPlayer.value)
            assertTrue(AppSettings.showQueueHistory.value)
            assertTrue(AppSettings.stopMusicOnClose.value)
        } finally {
            AppSettings.setLoudnessOffOnSpeaker(oldSpeaker)
            AppSettings.setHideStatusBarInPlayer(oldStatus)
            AppSettings.setShowQueueHistory(oldQueue)
            AppSettings.setStopMusicOnClose(oldClose)
        }
    }

    @Test
    fun `artist signature preferences persist and restore`() {
        val oldEnabled = AppSettings.artistSignaturesEnabled.value
        val oldUrl = AppSettings.artistSignaturesWikimediaUrl.value
        try {
            AppSettings.setArtistSignaturesEnabled(false)
            AppSettings.setArtistSignaturesWikimediaUrl("  https://commons.wikimedia.org/wiki/Artist  ")
            val backup = AppSettings.exportPrefsJson()
            assertTrue(backup.contains("\"artist_signatures_enabled\":\"false\""))
            assertTrue(backup.contains("\"artist_signatures_wikimedia_url\":\"https://commons.wikimedia.org/wiki/Artist\""))

            AppSettings.setArtistSignaturesEnabled(true)
            AppSettings.setArtistSignaturesWikimediaUrl("")
            AppSettings.importPrefsJson(backup)
            assertFalse(AppSettings.artistSignaturesEnabled.value)
            assertEquals("https://commons.wikimedia.org/wiki/Artist", AppSettings.artistSignaturesWikimediaUrl.value)
        } finally {
            AppSettings.setArtistSignaturesEnabled(oldEnabled)
            AppSettings.setArtistSignaturesWikimediaUrl(oldUrl)
        }
    }
}
