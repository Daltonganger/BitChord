package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.PlatformSettings
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull

class SourceRegistryColdStartTest {
    @Test
    fun listingRestoresThreeStoredAddonsBeforeAnySaveOrPlayback() {
        val previous = PlatformSettings.getString("sources", "")
        val previousMarker = PlatformSettings.getBoolean("jiosaavn_opt_in_v1", false)
        val stored = (1..3).map { index -> SourceConfig(
            id = "cold-addon-$index", kind = SourceKind.ADDON,
            label = "Stored addon $index", baseUrl = "https://addon-$index.invalid/u/config",
        ) }
        PlatformSettings.putString("sources", Json.encodeToString(stored))
        PlatformSettings.putBoolean("jiosaavn_opt_in_v1", true)
        try {
            // This is the sources screen's first entry into the registry in a fresh process.
            // No call to init(), config(), add() or active() may precede this read.
            var document: String? = null
            SourceResolverBridge.configs { document = it }
            val listing = Json.decodeFromString<ConfigListing>(assertNotNull(document))
            assertEquals(stored.map { it.id }, listing.sources.filter { it.kind == "ADDON" }.map { it.id })
            assertEquals(stored.map { it.baseUrl }, listing.sources.filter { it.kind == "ADDON" }.map { it.baseUrl })

            val extra = SourceConfig(id = "cold-extra", kind = SourceKind.ADDON, baseUrl = "https://extra.invalid")
            SourceRegistry.add(extra)
            assertEquals(stored.map { it.id } + extra.id,
                SourceRegistry.configs.value.filter { it.kind == SourceKind.ADDON }.map { it.id })
            SourceRegistry.remove(extra.id)
            assertEquals(stored.map { it.id },
                SourceRegistry.configs.value.filter { it.kind == SourceKind.ADDON }.map { it.id })
        } finally {
            PlatformSettings.putString("sources", previous)
            PlatformSettings.putBoolean("jiosaavn_opt_in_v1", previousMarker)
        }
    }
}
