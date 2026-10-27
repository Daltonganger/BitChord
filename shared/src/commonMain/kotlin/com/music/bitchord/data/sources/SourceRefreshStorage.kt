package com.music.bitchord.data.sources

import com.music.bitchord.data.settings.SecretRead

/** Only an explicit edit/removal may change a renewal secret; list edits never rewrite them. */
internal class SourceRefreshStorage(
    private val read: (String) -> SecretRead,
    private val write: (String, String?) -> Boolean,
) {
    fun restore(config: SourceConfig): SourceConfig {
        if (config.kind != SourceKind.ADDON) return config
        val result = read(SourceRegistry.refreshSecretAccount(config.id))
        return if (result.available) {
            config.copy(refreshUrl = result.value ?: config.refreshUrl, refreshUrlUnavailable = false)
        } else {
            config.copy(refreshUrl = "", refreshUrlUnavailable = true)
        }
    }

    fun commit(next: List<SourceConfig>, previous: List<SourceConfig>, preferences: () -> Unit) {
        data class Change(val key: String, val before: String?, val after: String?)
        val changes = mutableListOf<Change>()
        val old = previous.associateBy { it.id }
        for (config in next) {
            val prior = old[config.id]
            if (config.refreshUrlUnavailable || config.refreshUrl == prior?.refreshUrl ||
                (prior == null && config.refreshUrl.isBlank())) continue
            val key = SourceRegistry.refreshSecretAccount(config.id)
            val before = read(key)
            check(before.available) { "Unlock your device or Keychain before changing the renewal link" }
            val after = config.refreshUrl.ifBlank { null }
            if (before.value != after) changes += Change(key, before.value, after)
        }
        for (removed in previous.filter { before -> next.none { it.id == before.id } }) {
            if (removed.refreshUrl.isBlank() && !removed.refreshEnabled && !removed.refreshUrlUnavailable) continue
            val key = SourceRegistry.refreshSecretAccount(removed.id)
            val before = read(key)
            check(before.available) { "Unlock your device or Keychain before removing this source" }
            if (before.value != null) changes += Change(key, before.value, null)
        }
        val applied = mutableListOf<Change>()
        try {
            for (change in changes) {
                check(write(change.key, change.after)) { "Could not securely save the addon renewal link" }
                applied += change
            }
            preferences()
        } catch (_: Exception) {
            // Restore earlier writes if a later write failed; the store may itself be unavailable.
            for (change in applied.asReversed()) runCatching { write(change.key, change.before) }
            throw IllegalStateException("Could not save source settings; unlock your device or Keychain and try again")
        }
    }
}
