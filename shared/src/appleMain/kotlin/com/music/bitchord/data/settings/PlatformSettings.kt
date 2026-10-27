package com.music.bitchord.data.settings

import platform.Foundation.NSUserDefaults
import kotlin.concurrent.Volatile

/**
 * Apple actual of the settings seam.
 *
 * Ordinary settings are `NSUserDefaults`. Credentials are the Keychain, reached
 * through a Swift implementation rather than through cinterop: the Security
 * framework's dictionary-and-out-parameter shape does not map cleanly onto
 * Kotlin/Native, and the Swift side already has a proven, reviewed Keychain
 * accessor ([AuthStore]) that this should share rather than duplicate.
 *
 * Until Swift installs an implementation — before `App`'s first `task` — a
 * secret read answers null, so a launch that somehow reaches settings first
 * behaves as "no credential" rather than crashing.
 */
actual object PlatformSettings {
    private val defaults = NSUserDefaults.standardUserDefaults

    actual fun getString(key: String, default: String): String =
        defaults.stringForKey(key) ?: default

    actual fun putString(key: String, value: String) {
        defaults.setObject(value, forKey = key)
    }

    actual fun getInt(key: String, default: Int): Int =
        if (defaults.objectForKey(key) != null) defaults.integerForKey(key).toInt() else default

    actual fun putInt(key: String, value: Int) {
        defaults.setInteger(value.toLong(), forKey = key)
    }

    actual fun getBoolean(key: String, default: Boolean): Boolean =
        if (defaults.objectForKey(key) != null) defaults.boolForKey(key) else default

    actual fun putBoolean(key: String, value: Boolean) {
        defaults.setBool(value, forKey = key)
    }

    actual fun getFloat(key: String, default: Float): Float =
        if (defaults.objectForKey(key) != null) defaults.floatForKey(key) else default

    actual fun putFloat(key: String, value: Float) {
        defaults.setFloat(value, forKey = key)
    }

    actual fun getLong(key: String, default: Long): Long =
        if (defaults.objectForKey(key) != null) defaults.integerForKey(key) else default

    actual fun putLong(key: String, value: Long) {
        defaults.setInteger(value, forKey = key)
    }

    actual fun getSecret(key: String): String? = SecretStoreBridge.get(key)

    actual fun putSecret(key: String, value: String?) = SecretStoreBridge.put(key, value)
    actual fun readSecret(key: String): SecretRead = SecretStoreBridge.read(key)
    actual fun writeSecret(key: String, value: String?): Boolean = SecretStoreBridge.write(key, value)
}

/**
 * Swift-facing registration surface for the Keychain-backed half of the settings
 * store. Installed once at launch, alongside [CipherUnlockBridge].
 */
object SecretStoreBridge {

    interface Impl {
        fun get(key: String): String?
        fun put(key: String, value: String?)
        fun read(key: String): SecretRead = SecretRead(get(key), true)
        fun write(key: String, value: String?): Boolean {
            put(key, value)
            val result = read(key)
            return result.available && result.value == value
        }
    }

    @Volatile
    private var impl: Impl? = null

    fun setImpl(value: Impl?) {
        impl = value
    }

    fun get(key: String): String? = impl?.get(key)
    fun read(key: String): SecretRead = impl?.read(key) ?: SecretRead(null, false)
    fun write(key: String, value: String?): Boolean = impl?.write(key, value) ?: false

    fun put(key: String, value: String?) {
        impl?.put(key, value)
    }
}
