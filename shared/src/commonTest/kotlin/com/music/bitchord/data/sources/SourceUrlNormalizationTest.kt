package com.music.bitchord.data.sources

import com.music.bitchord.data.sources.addon.AddonClient
import kotlin.test.Test
import kotlin.test.assertEquals

class SourceUrlNormalizationTest {
    @Test fun preservesTopLevelDomains() {
        val url = "https://bitchord-hires-addon.vercel.app"
        assertEquals(url, SourceRegistry.normalizeAddonBase(url))
        assertEquals(url, SourceRegistry.normalizeAddonBase("$url/"))
        assertEquals("https://losslessbridge.de.deplexo.com",
            SourceRegistry.normalizeAddonBase("https://losslessbridge.de.deplexo.com"))
    }

    @Test fun stripsManifestPathNotHostname() {
        val base = "https://bitchord-hires-addon.vercel.app"
        assertEquals(base, SourceRegistry.normalizeAddonBase("$base/manifest.json"))
        assertEquals(base, SourceRegistry.normalizeAddonBase("$base/MANIFEST.JSON/"))
    }

    @Test fun preservesOpaqueGglosUserPathAndBuildsCorrectManifestUrl() {
        val base = "https://gglos.ahusaktk.workers.dev/u/opaque-config-token"
        val pasted = "$base/manifest.json"
        assertEquals(base, SourceRegistry.normalizeAddonBase(pasted))
        assertEquals(base, AddonClient.normalizeBase(pasted))
        assertEquals(pasted, AddonClient.manifestUrl(SourceRegistry.normalizeAddonBase(pasted)))
        assertEquals(base, SourceRegistry.normalizeAddonBase(base))
    }

    @Test fun leavesNestedPathAndJsonLikeHostnameIntact() {
        assertEquals("https://host.example/a/b",
            SourceRegistry.normalizeAddonBase("https://host.example/a/b/"))
        assertEquals("https://addon.example/u/key.with.dots",
            SourceRegistry.normalizeAddonBase("https://addon.example/u/key.with.dots"))
    }
    @Test fun preservesJsonLikeHostnameAndOtherPathSuffixes() {
        for (base in listOf("https://addon.json", "https://addon.example/u/config.json.token")) {
            assertEquals(base, SourceRegistry.normalizeAddonBase(base))
            assertEquals(base, AddonClient.normalizeBase(base))
        }
    }

    @Test fun preservesLongEncodedConfigurationVerbatim() {
        val token = "AbC_123-xyz%2F%2B%3D.with.dots".repeat(80)
        val base = "https://gglos.example/u/$token"
        val normalized = SourceRegistry.normalizeAddonBase("  $base/manifest.json/  ")
        assertEquals(base, normalized)
        assertEquals("$base/manifest.json", AddonClient.manifestUrl(normalized))
        assertEquals(normalized, SourceRegistry.normalizeAddonBase(normalized))
    }

    @Test fun keepsAlternateJsonManifestSupportAndBlankInput() {
        assertEquals("https://addon.example/u/token",
            SourceRegistry.normalizeAddonBase("https://addon.example/u/token/addon.json"))
        assertEquals("", SourceRegistry.normalizeAddonBase("   "))
    }

}
