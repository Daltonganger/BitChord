package com.music.bitchord.data.innertube

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.assertFalse

class VideoVersionBridgeTest {
    @Test
    fun readsOnlyTheCounterpartOnTheMatchingQueueRow() {
        val fixture = Json.parseToJsonElement(
            """
            {
              "queue": [
                {
                  "playlistPanelVideoWrapperRenderer": {
                    "primaryRenderer": {"playlistPanelVideoRenderer": {"videoId": "other-song"}},
                    "counterpart": [{"counterpartRenderer": {"playlistPanelVideoRenderer": {"videoId": "wrong-video"}}}]
                  }
                },
                {
                  "playlistPanelVideoWrapperRenderer": {
                    "primaryRenderer": {
                      "playlistPanelVideoRenderer": {
                        "videoId": "source-song",
                        "title": {"runs": [{"text": "Down"}]}
                      }
                    },
                    "counterpart": [
                      {"counterpartRenderer": {"playlistPanelVideoRenderer": {
                        "videoId": "paired-video",
                        "title": {"runs": [{"text": "Down (Official Video)"}]},
                        "lengthText": {"simpleText": "3:12"},
                        "navigationEndpoint": {"watchEndpoint": {
                          "videoId": "paired-video",
                          "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {
                            "musicVideoType": "MUSIC_VIDEO_TYPE_OMV"
                          }}
                        }}
                      }}}
                    ]
                  }
                }
              ]
            }
            """.trimIndent(),
        ).jsonObject

        val candidate = VideoVersionBridge.parseCounterpart(fixture, "source-song")

        assertEquals("paired-video", candidate?.videoId)
        assertEquals("Down (Official Video)", candidate?.title)
        assertEquals("3:12", candidate?.durationText)
        assertEquals("MUSIC_VIDEO_TYPE_OMV", candidate?.type)
        assertEquals("counterpart", candidate?.source)
        assertNull(VideoVersionBridge.parseCounterpart(fixture, "missing-song"))
    }

    @Test
    fun searchRejectsWholeWordUploadMarkersAndUnrequestedLiveVersions() {
        listOf(
            "Down (Lyrics)",
            "Down - letra official video",
            "Down visualiser",
            "Down: Official Audio",
            "Down fan-made video",
            "Down AMV",
            "Down (LIVE at Wembley)",
        ).forEach { title ->
            assertTrue(VideoVersionBridge.isExcludedSearchTitle(title, "Down"), title)
        }
        assertFalse(VideoVersionBridge.isExcludedSearchTitle("Downward (Official Video)", "Down"))
        assertFalse(VideoVersionBridge.isExcludedSearchTitle("Down (Live)", "Down Live"))
    }
}
