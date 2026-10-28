package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.SecretRead

/** A damaged settings document must never become a successful empty source list. */
internal class SourceListStorage(
    private val readPrimary: () -> String,
    private val writePrimary: (String) -> Unit,
    private val readBackup: () -> SecretRead,
    private val writeBackup: (String?) -> Boolean,
    private val isValid: (String) -> Boolean,
) {
    fun read(): String {
        val primary = readPrimary()
        // Blank means a new install; [] means an intentionally empty list.
        if (primary.isBlank()) return primary
        if (isValid(primary)) {
            // Seed a protected copy when upgrading an existing installation.
            runCatching {
                val backup = readBackup()
                if (backup.available && backup.value?.let(isValid) != true) writeBackup(primary)
            }
            return primary
        }
        val backup = readBackup()
        return backup.value?.takeIf { backup.available && isValid(it) }
            ?: error("Sources could not be read. Unlock your device or Keychain and try again.")
    }

    fun write(document: String) {
        check(isValid(document)) { "Could not save the source list" }
        val before = readBackup()
        check(before.available && writeBackup(document)) {
            "Could not securely save sources. Unlock your device or Keychain and try again."
        }
        try {
            writePrimary(document)
        } catch (failure: Exception) {
            runCatching { writeBackup(before.value) }
            throw failure
        }
    }
}
