package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.PlatformSettings
import com.music.bitchord.data.settings.SecretStoreBridge
import com.music.bitchord.data.settings.SecretRead
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertFalse

class SourceRegistryColdStartTest {
    @Test
    fun coldStartPreservesUnreadableSourcesAndRecoversThreeAddonsAfterUnlock() {
        val previous = PlatformSettings.getString("sources", "")
        val previousMarker = PlatformSettings.getBoolean("jiosaavn_opt_in_v1", false)
        val stored = (1..3).map { index -> SourceConfig(
            id = "cold-addon-$index", kind = SourceKind.ADDON,
            label = "Stored addon $index", baseUrl = "https://addon-$index.invalid/u/config",
        ) }
        PlatformSettings.putString("sources", "damaged source document")
        PlatformSettings.putBoolean("jiosaavn_opt_in_v1", true)
        val secrets = mutableMapOf<String, String>()
        secrets["sources-document-backup-v1"] = Json.encodeToString(stored)
        var available = false
        SecretStoreBridge.setImpl(object : SecretStoreBridge.Impl {
            override fun get(key: String): String? = if (available) secrets[key] else null
            override fun read(key: String) = SecretRead(get(key), available)
            override fun put(key: String, value: String?) {
                if (!available) return
                if (value == null) secrets.remove(key) else secrets[key] = value
            }
        })
        try {
            // This is the sources screen's first entry into the registry in a fresh process.
            // No call to init(), config(), add() or active() may precede this read.
            var document: String? = null
            SourceResolverBridge.configs { document = it }
            assertNull(document)
            assertEquals("damaged source document", PlatformSettings.getString("sources", ""))
            assertFalse(SourceResolverBridge.canSubstituteForYouTube())
            assertEquals(0, SourceResolverBridge.addonCount())
            available = true
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
            SecretStoreBridge.setImpl(null)
            PlatformSettings.putString("sources", previous)
            PlatformSettings.putBoolean("jiosaavn_opt_in_v1", previousMarker)
        }
    }
}
