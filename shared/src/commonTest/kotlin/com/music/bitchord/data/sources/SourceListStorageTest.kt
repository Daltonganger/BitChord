package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.SecretRead
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SourceListStorageTest {
    private class Fixture {
        var primary = "[old]"
        var backup: String? = "[old]"
        var available = true
        var failBackup = false
        var failPrimary = false
        var primaryWrites = 0
        val storage = SourceListStorage(
            readPrimary = { primary },
            writePrimary = { check(!failPrimary); primaryWrites++; primary = it },
            readBackup = { SecretRead(if (available) backup else null, available) },
            writeBackup = { if (!available || failBackup) false else { backup = it; true } },
            isValid = { it in listOf("[]", "[old]", "[new]") },
        )
    }

    @Test fun corruptionRecoversProtectedCopyWithoutOverwritingTheOriginal() {
        val f = Fixture(); f.primary = "damaged"
        assertEquals("[old]", f.storage.read())
        assertEquals("damaged", f.primary)
        assertEquals(0, f.primaryWrites)
    }

    @Test fun corruptionWithoutAnAccessibleBackupIsAnErrorAndCanRetryAfterUnlock() {
        val f = Fixture(); f.primary = "damaged"; f.available = false
        assertFailsWith<IllegalStateException> { f.storage.read() }
        assertEquals("damaged", f.primary)
        f.available = true
        assertEquals("[old]", f.storage.read())
        f.backup = "also damaged"
        assertFailsWith<IllegalStateException> { f.storage.read() }
    }

    @Test fun newInstallAndExplicitEmptyListsDoNotResurrectRemovedSources() {
        val f = Fixture()
        for (value in listOf("", "[]")) {
            f.primary = value
            assertEquals(value, f.storage.read())
        }
    }

    @Test fun validExistingListSeedsProtectedBackupButStillReadsWhenLocked() {
        val f = Fixture(); f.backup = null
        assertEquals("[old]", f.storage.read())
        assertEquals("[old]", f.backup)
        f.backup = null; f.available = false
        assertEquals("[old]", f.storage.read())
        assertEquals(null, f.backup)
    }

    @Test fun saveUpdatesBothCopiesAndFailedPrimaryWriteRollsBackProtectedCopy() {
        val f = Fixture()
        f.storage.write("[new]")
        assertEquals("[new]", f.primary); assertEquals("[new]", f.backup)
        f.failPrimary = true
        assertFailsWith<IllegalStateException> { f.storage.write("[]") }
        assertEquals("[new]", f.primary); assertEquals("[new]", f.backup)
    }

    @Test fun unavailableOrFailedSecretWriteLeavesPrimaryUntouched() {
        for (locked in listOf(true, false)) {
            val f = Fixture(); f.available = !locked; f.failBackup = !locked
            assertFailsWith<IllegalStateException> { f.storage.write("[new]") }
            assertEquals("[old]", f.primary); assertEquals("[old]", f.backup)
            assertEquals(0, f.primaryWrites)
        }
    }

    @Test fun validationRejectsMalformedAndDuplicateListsButAllowsRetiredKindsBesideValidOnes() {
        assertFalse(SourceRegistry.isReadableDocument("broken"))
        assertFalse(SourceRegistry.isReadableDocument("{}"))
        assertFalse(SourceRegistry.isReadableDocument("[{\"kind\":\"YOUTUBE\"}]"))
        assertFalse(SourceRegistry.isReadableDocument("[{\"id\":\"yt\",\"kind\":\"YOUTUBE\"},{\"id\":\"broken\",\"kind\":\"ADDON\",\"baseUrl\":[]}]"))
        assertFalse(SourceRegistry.isReadableDocument("[{\"kind\":\"FUTURE_KIND\"}]"))
        assertFalse(SourceRegistry.isReadableDocument("[{\"id\":\"a\",\"kind\":\"YOUTUBE\"},{\"id\":\"a\",\"kind\":\"YOUTUBE\"}]"))
        assertTrue(SourceRegistry.isReadableDocument("[]"))
        assertTrue(SourceRegistry.isReadableDocument("[{\"id\":\"a\",\"kind\":\"YOUTUBE\"},{\"kind\":\"FUTURE_KIND\"}]"))
    }
}
