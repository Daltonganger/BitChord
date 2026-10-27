package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.SecretRead
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class SourceRefreshStorageTest {
    private class Fixture {
        val secrets = mutableMapOf<String, String>()
        val writes = mutableListOf<Pair<String, String?>>()
        var available = true
        var failKey: String? = null
        var preferencesSaved = false
        val store = SourceRefreshStorage(
            read = { key -> SecretRead(if (available) secrets[key] else null, available) },
            write = { key, value ->
                writes += key to value
                if (!available || key == failKey) false else {
                    if (value == null) secrets.remove(key) else secrets[key] = value
                    true
                }
            },
        )
        fun config(id: String = "a", link: String = "https://renew.example/private") =
            SourceConfig(id = id, kind = SourceKind.ADDON, baseUrl = "https://addon.example", refreshEnabled = true, refreshUrl = link)
        fun key(config: SourceConfig) = SourceRegistry.refreshSecretAccount(config.id)
        fun commit(next: List<SourceConfig>, before: List<SourceConfig>) =
            store.commit(next, before) { preferencesSaved = true }
    }

    @Test fun unavailableIsDifferentFromMissingAndCanRecoverAfterUnlock() {
        val f = Fixture(); val stored = f.config(link = "")
        f.secrets[f.key(stored)] = "https://renew.example/secret"
        f.available = false
        val unavailable = f.store.restore(stored)
        assertTrue(unavailable.refreshUrlUnavailable)
        assertEquals("", unavailable.refreshUrl)
        f.available = true
        val recovered = f.store.restore(unavailable)
        assertFalse(recovered.refreshUrlUnavailable)
        assertEquals(f.secrets[f.key(stored)], recovered.refreshUrl)
        f.secrets.clear()
        val missing = f.store.restore(stored)
        assertFalse(missing.refreshUrlUnavailable)
        assertEquals("", missing.refreshUrl)
    }

    @Test fun togglingReorderingAndSavingAnotherSourceNeverDeleteAnUnreadableLink() {
        val f = Fixture(); val a = f.config(); val b = f.config("b", "")
        f.secrets[f.key(a)] = a.refreshUrl
        f.available = false
        val unavailable = f.store.restore(a.copy(refreshUrl = ""))
        f.available = true // The dangerous case: storage becomes writable before an unrelated save.
        f.commit(listOf(b, unavailable.copy(enabled = false)), listOf(unavailable, b))
        assertTrue(f.preferencesSaved)
        assertTrue(f.writes.isEmpty())
        assertEquals(a.refreshUrl, f.secrets[f.key(a)])
    }

    @Test fun explicitClearDeletesOnlyTheEditedSecretAndPreferencesContainNoLink() {
        val f = Fixture(); val a = f.config(); val b = f.config("b", "https://other.example/private")
        f.secrets[f.key(a)] = a.refreshUrl; f.secrets[f.key(b)] = b.refreshUrl
        val cleared = a.copy(refreshEnabled = false, refreshUrl = "")
        f.commit(listOf(cleared, b), listOf(a, b))
        assertEquals(listOf<Pair<String, String?>>(f.key(a) to null), f.writes)
        assertEquals(b.refreshUrl, f.secrets[f.key(b)])
        val preferences = Json.encodeToString(listOf(cleared, b).map { it.copy(refreshUrl = "") })
        assertFalse(preferences.contains("private"))
        assertFalse(preferences.contains("refreshUrlUnavailable"))
    }

    @Test fun failedSaveOrRemovalDoesNotPublishPreferencesOrDestroyExistingSecrets() {
        for (remove in listOf(false, true)) {
            val f = Fixture(); val a = f.config()
            f.secrets[f.key(a)] = a.refreshUrl; f.failKey = f.key(a)
            assertFailsWith<IllegalStateException> {
                f.commit(if (remove) emptyList() else listOf(a.copy(refreshUrl = "https://new.example/private")), listOf(a))
            }
            assertFalse(f.preferencesSaved)
            assertEquals(a.refreshUrl, f.secrets[f.key(a)])
        }
    }

    @Test fun aLaterFailedWriteRollsBackEarlierSecretChanges() {
        val f = Fixture(); val a = f.config(); val b = f.config("b", "https://b.example/private")
        f.secrets[f.key(a)] = a.refreshUrl; f.secrets[f.key(b)] = b.refreshUrl
        f.failKey = f.key(b)
        assertFailsWith<IllegalStateException> {
            f.commit(listOf(a.copy(refreshUrl = "https://new-a.example"), b.copy(refreshUrl = "https://new-b.example")), listOf(a, b))
        }
        assertFalse(f.preferencesSaved)
        assertEquals(a.refreshUrl, f.secrets[f.key(a)])
        assertEquals(b.refreshUrl, f.secrets[f.key(b)])
    }

    @Test fun unavailableStorageRefusesAnExplicitEditWithoutTouchingPreferences() {
        val f = Fixture(); val a = f.config(); f.available = false
        assertFailsWith<IllegalStateException> { f.commit(listOf(a.copy(refreshUrl = "https://new.example")), listOf(a)) }
        assertTrue(f.writes.isEmpty())
        assertFalse(f.preferencesSaved)
    }

    @Test fun anEditorOpenedWhileLockedPreservesItsLinkEvenIfStorageUnlocksBeforeSaving() {
        val f = Fixture(); val a = f.config()
        f.secrets[f.key(a)] = a.refreshUrl
        f.available = false
        val editor = f.store.restore(a.copy(refreshUrl = ""))
        f.available = true
        val recovered = f.store.restore(editor)
        f.commit(listOf(editor.copy(label = "Renamed", refreshEnabled = false)), listOf(recovered))
        assertTrue(f.preferencesSaved)
        assertTrue(f.writes.isEmpty())
        assertEquals(a.refreshUrl, f.secrets[f.key(a)])
    }

    @Test fun anOrdinaryAddonCanBeRemovedWithoutAccessingRenewalStorage() {
        val f = Fixture(); val ordinary = f.config(link = "").copy(refreshEnabled = false)
        f.available = false
        f.commit(emptyList(), listOf(ordinary))
        assertTrue(f.preferencesSaved)
        assertTrue(f.writes.isEmpty())
    }

    @Test fun sourceActionConvertsFailuresToOneSanitizedCallback() {
        for (operation in listOf("toggle", "remove", "reorder")) {
            var calls = 0
            SourceResolverBridge.sourceAction(SourceResolverBridge.ActionCallback { ok, message ->
                calls++
                assertFalse(ok)
                assertTrue(message.orEmpty().contains("unlock"))
                assertFalse(message.orEmpty().contains("private-token"))
            }) { error("$operation failed: private-token") }
            assertEquals(1, calls)
        }
        var success = false
        SourceResolverBridge.sourceAction(SourceResolverBridge.ActionCallback { ok, message ->
            success = ok && message == null
        }) { }
        assertTrue(success)
    }
}
