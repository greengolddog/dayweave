package com.greengolddog.dayweave.network

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.nio.charset.StandardCharsets
import java.security.KeyStore
import java.security.MessageDigest
import java.util.Base64
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json

internal interface DeviceAuthEnvelopeStore {
    fun read(): StoredDeviceAuthEnvelope

    /** Atomically replaces exactly [expected], incrementing its revision once. */
    fun compareAndSet(expected: StoredDeviceAuthEnvelope, nextState: StoredDeviceAuthState): Boolean

    /** Removes the envelope and device-bound key only if the exact envelope is still current. */
    fun destroy(expected: StoredDeviceAuthEnvelope): DeviceAuthDestroyResult

    fun lastSuccessfulSyncEpochMillis(): Long?

    fun recordSuccessfulSync(epochMillis: Long)
}

internal enum class DeviceAuthDestroyResult {
    DESTROYED,
    STALE,
    CREDENTIALS_DESTROYED_CLEANUP_PENDING,
}

/**
 * One AES-GCM envelope protected by a non-exportable Android Keystore key.
 *
 * SharedPreferences commit atomically replaces the single ciphertext record. Secrets, pending
 * tuples, and state metadata therefore advance together, while compare-and-set protects against
 * a stale callback deleting or replacing a newer session.
 */
internal class KeystoreDeviceAuthEnvelopeStore private constructor(
    context: Context,
    configuredBaseUrl: String,
    private val preferencesName: String,
    private val keyAlias: String,
    private val keyAccess: DeviceAuthKeyAccess,
    private val readbackOverride: ((String?) -> String?)?,
    private val legacyCleanupOverride: (() -> Boolean)?,
) : DeviceAuthEnvelopeStore {
    constructor(
        context: Context,
        configuredBaseUrl: String,
        preferencesName: String = PREFERENCES_NAME,
        keyAlias: String = KEY_ALIAS,
    ) : this(
        context = context,
        configuredBaseUrl = configuredBaseUrl,
        preferencesName = preferencesName,
        keyAlias = keyAlias,
        keyAccess = AndroidDeviceAuthKeyAccess,
        readbackOverride = null,
        legacyCleanupOverride = null,
    )

    private val preferences = context.applicationContext.getSharedPreferences(
        preferencesName,
        Context.MODE_PRIVATE,
    )
    private val defaultBaseUrl = configuredBaseUrl.trim().takeIf(String::isNotEmpty)
        ?.let(::normalizedHttpsApiBaseUrl)
    private val json = Json {
        classDiscriminator = "phase"
        ignoreUnknownKeys = false
        explicitNulls = true
        encodeDefaults = true
    }

    override fun read(): StoredDeviceAuthEnvelope = synchronized(STORE_LOCK) {
        loadOrInitialize()
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    override fun compareAndSet(
        expected: StoredDeviceAuthEnvelope,
        nextState: StoredDeviceAuthState,
    ): Boolean = synchronized(STORE_LOCK) {
        validateStoredDeviceAuthState(nextState)
        val current = loadOrInitialize()
        if (current != expected || current.state is StoredDeviceAuthState.Incompatible) {
            return@synchronized false
        }
        // Long.MAX_VALUE is deliberately not a readable envelope revision. Refuse the final
        // increment before writing so exhaustion is deterministic and cannot persist a record
        // that only decodes as unrelated incompatible ciphertext on readback.
        if (current.revision >= Long.MAX_VALUE - 1) return@synchronized false
        val next = StoredDeviceAuthEnvelope(
            revision = Math.addExact(current.revision, 1),
            state = nextState,
        )
        val canonical = json.encodeToString(next)
        val encoded = encrypt(canonical, getOrCreateKey())
        val expectedReadback = next.copy(storageIdentity = storageIdentity(encoded, canonical))
        check(
            preferences.edit()
                .putString(ENCRYPTED_ENVELOPE, encoded)
                .commit(),
        ) { "Unable to persist device authentication state" }
        val readback = verifiedEnvelopeReadback()
            ?.let(::decodeEnvelope)
        check(readback == expectedReadback) { "Device authentication state readback did not match" }
        retryLegacyCleanup()
        true
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    override fun destroy(
        expected: StoredDeviceAuthEnvelope,
    ): DeviceAuthDestroyResult = synchronized(STORE_LOCK) {
        val current = loadOrInitialize()
        if (current != expected) return@synchronized DeviceAuthDestroyResult.STALE
        check(
            preferences.edit()
                .putBoolean(DESTROY_CLEANUP_PENDING, true)
                .remove(ENCRYPTED_ENVELOPE)
                .remove(LEGACY_BASE_URL)
                .remove(LEGACY_WRAPPED_TOKEN)
                .remove(LEGACY_CONFIGURATION_ID)
                .remove(LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS)
                .commit(),
        ) { "Unable to remove device authentication state" }
        check(
            !preferences.contains(ENCRYPTED_ENVELOPE) &&
                !preferences.contains(LEGACY_BASE_URL) &&
                !preferences.contains(LEGACY_WRAPPED_TOKEN) &&
                !preferences.contains(LEGACY_CONFIGURATION_ID) &&
                preferences.getBoolean(DESTROY_CLEANUP_PENDING, false)
        ) { "Device authentication removal readback did not match" }
        if (finishDestroyCleanup()) {
            DeviceAuthDestroyResult.DESTROYED
        } else {
            DeviceAuthDestroyResult.CREDENTIALS_DESTROYED_CLEANUP_PENDING
        }
    }

    override fun lastSuccessfulSyncEpochMillis(): Long? = synchronized(STORE_LOCK) {
        preferences.getLong(LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS, NO_SYNC_RECORDED)
            .takeUnless { it == NO_SYNC_RECORDED }
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    override fun recordSuccessfulSync(epochMillis: Long) = synchronized(STORE_LOCK) {
        require(epochMillis >= 0) { "Sync time cannot be negative" }
        check(
            preferences.edit()
                .putLong(LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS, epochMillis)
                .commit(),
        ) { "Unable to persist the last successful sync time" }
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun loadOrInitialize(): StoredDeviceAuthEnvelope {
        if (preferences.getBoolean(DESTROY_CLEANUP_PENDING, false)) {
            if (!finishDestroyCleanup()) return destroyCleanupPendingEnvelope()
        }
        val encoded = preferences.getString(ENCRYPTED_ENVELOPE, null)
        if (encoded != null) {
            var envelope = decodeEnvelope(encoded)
            if (envelope.schemaVersion == LEGACY_ENVELOPE_VERSION) {
                envelope = migrateVersionOneEnvelope(envelope)
            }
            if (envelope.state !is StoredDeviceAuthState.Incompatible) retryLegacyCleanup()
            return envelope
        }

        val migrated = migrateLegacyState()
        val initial = StoredDeviceAuthEnvelope(
            revision = 1,
            state = migrated ?: StoredDeviceAuthState.Unconfigured(
                baseUrl = defaultBaseUrl,
                clientInstanceId = UUID.randomUUID().toString(),
            ),
        )
        validateStoredDeviceAuthState(initial.state)
        val canonical = json.encodeToString(initial)
        val ciphertext = encrypt(canonical, getOrCreateKey())
        val expectedReadback = initial.copy(storageIdentity = storageIdentity(ciphertext, canonical))
        check(
            preferences.edit()
                .putString(ENCRYPTED_ENVELOPE, ciphertext)
                .commit(),
        ) { "Unable to initialize device authentication state" }
        val readback = verifiedEnvelopeReadback()
            ?.let(::decodeEnvelope)
        check(readback == expectedReadback) { "Device authentication initialization readback did not match" }
        retryLegacyCleanup()
        return requireNotNull(readback)
    }

    /** A failed legacy-record cleanup is retried on every read until durable readback is empty. */
    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun retryLegacyCleanup(): Boolean {
        if (
            !preferences.contains(LEGACY_BASE_URL) &&
            !preferences.contains(LEGACY_WRAPPED_TOKEN) &&
            !preferences.contains(LEGACY_CONFIGURATION_ID)
        ) {
            return true
        }
        if (legacyCleanupOverride?.invoke() == false) return false
        val committed = preferences.edit()
            .remove(LEGACY_BASE_URL)
            .remove(LEGACY_WRAPPED_TOKEN)
            .remove(LEGACY_CONFIGURATION_ID)
            .commit()
        return committed &&
            !preferences.contains(LEGACY_BASE_URL) &&
                !preferences.contains(LEGACY_WRAPPED_TOKEN) &&
                !preferences.contains(LEGACY_CONFIGURATION_ID)
    }

    private fun migrateLegacyState(): StoredDeviceAuthState.Legacy? {
        val wrappedToken = preferences.getString(LEGACY_WRAPPED_TOKEN, null) ?: return null
        val baseUrl = preferences.getString(LEGACY_BASE_URL, null)
            ?.takeIf(String::isNotBlank)
            ?: defaultBaseUrl
            ?: throw SecureCredentialException("Legacy authentication has no API endpoint")
        val token = decryptLegacyToken(wrappedToken)
        return StoredDeviceAuthState.Legacy(
            baseUrl = normalizedHttpsApiBaseUrl(baseUrl),
            clientInstanceId = UUID.randomUUID().toString(),
            bindingId = preferences.getString(LEGACY_CONFIGURATION_ID, null)
                ?.takeIf { runCatching { UUID.fromString(it) }.isSuccess }
                ?: UUID.randomUUID().toString(),
            bootstrapToken = DeviceAuthSecret(validateLegacyBootstrapToken(token)),
        )
    }

    private fun decodeEnvelope(encoded: String): StoredDeviceAuthEnvelope {
        val plaintext = try {
            decrypt(encoded, existingKey() ?: return incompatible("device_key_unavailable", encoded))
        } catch (_: Exception) {
            return incompatible("encrypted_state_unreadable", encoded)
        }
        return try {
            val envelope = json.decodeFromString<StoredDeviceAuthEnvelope>(plaintext)
            if (
                envelope.schemaVersion !in setOf(
                    LEGACY_ENVELOPE_VERSION,
                    DEVICE_AUTH_ENVELOPE_VERSION,
                ) ||
                envelope.revision !in 1 until Long.MAX_VALUE ||
                envelope.schemaVersion == LEGACY_ENVELOPE_VERSION &&
                envelope.revision >= Long.MAX_VALUE - 1
            ) {
                return incompatible("unsupported_state_version", encoded)
            }
            if (
                envelope.schemaVersion == LEGACY_ENVELOPE_VERSION &&
                envelope.state is StoredDeviceAuthState.EnrollmentCreationPending ||
                envelope.schemaVersion == LEGACY_ENVELOPE_VERSION &&
                envelope.state is StoredDeviceAuthState.EnrollmentPending ||
                envelope.schemaVersion == LEGACY_ENVELOPE_VERSION &&
                envelope.state is StoredDeviceAuthState.RefreshPending
            ) {
                return incompatible("legacy_pending_request_binding_unavailable", encoded)
            }
            validateStoredDeviceAuthState(envelope.state)
            val canonical = json.encodeToString(envelope)
            envelope.copy(storageIdentity = storageIdentity(encoded, canonical))
        } catch (_: SerializationException) {
            incompatible("stored_state_malformed", encoded)
        } catch (_: IllegalArgumentException) {
            incompatible("stored_state_invalid", encoded)
        }
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun migrateVersionOneEnvelope(
        legacy: StoredDeviceAuthEnvelope,
    ): StoredDeviceAuthEnvelope {
        check(legacy.schemaVersion == LEGACY_ENVELOPE_VERSION)
        val migrated = StoredDeviceAuthEnvelope(
            schemaVersion = DEVICE_AUTH_ENVELOPE_VERSION,
            revision = Math.addExact(legacy.revision, 1),
            state = legacy.state,
        )
        val canonical = json.encodeToString(migrated)
        val encoded = encrypt(canonical, getOrCreateKey())
        val expected = migrated.copy(storageIdentity = storageIdentity(encoded, canonical))
        check(
            preferences.edit().putString(ENCRYPTED_ENVELOPE, encoded).commit(),
        ) { "Unable to migrate device authentication state" }
        val readback = verifiedEnvelopeReadback()?.let(::decodeEnvelope)
        check(readback == expected) { "Device authentication migration readback did not match" }
        return requireNotNull(readback)
    }

    private fun incompatible(reason: String, encoded: String) = StoredDeviceAuthEnvelope(
        revision = INCOMPATIBLE_REVISION,
        state = StoredDeviceAuthState.Incompatible(reason),
        storageIdentity = storageIdentity(encoded, canonicalPlaintext = null),
    )

    private fun destroyCleanupPendingEnvelope() = StoredDeviceAuthEnvelope(
        revision = INCOMPATIBLE_REVISION,
        state = StoredDeviceAuthState.Incompatible("local_destroy_cleanup_pending"),
        storageIdentity = storageIdentity("local_destroy_cleanup_pending", canonicalPlaintext = null),
    )

    /** Credentials are already absent; every read retries obsolete-key/tombstone cleanup. */
    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun finishDestroyCleanup(): Boolean {
        try {
            keyAccess.delete(keyAlias)
            if (keyAccess.existing(keyAlias) != null) return false
        } catch (_: Exception) {
            return false
        }
        val committed = preferences.edit().remove(DESTROY_CLEANUP_PENDING).commit()
        return committed && !preferences.contains(DESTROY_CLEANUP_PENDING)
    }

    private fun storageIdentity(
        encoded: String,
        canonicalPlaintext: String?,
    ): DeviceAuthStorageIdentity {
        val encodedBytes = encoded.toByteArray(StandardCharsets.UTF_8)
        val canonicalBytes = canonicalPlaintext?.toByteArray(StandardCharsets.UTF_8)
        return try {
            val digest = MessageDigest.getInstance("SHA-256")
            digest.update(encodedBytes)
            digest.update(0.toByte())
            canonicalBytes?.let(digest::update)
            DeviceAuthStorageIdentity(digest.digest())
        } finally {
            encodedBytes.fill(0)
            canonicalBytes?.fill(0)
        }
    }

    private fun encrypt(plaintext: String, key: SecretKey): String {
        val bytes = plaintext.toByteArray(StandardCharsets.UTF_8)
        return try {
            val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, key)
            cipher.updateAAD(aad())
            val ciphertext = cipher.doFinal(bytes)
            listOf(
                ENCRYPTION_FORMAT,
                Base64.getUrlEncoder().withoutPadding().encodeToString(cipher.iv),
                Base64.getUrlEncoder().withoutPadding().encodeToString(ciphertext),
            ).joinToString(":")
        } catch (error: Exception) {
            throw SecureCredentialException("Unable to encrypt device authentication state", error)
        } finally {
            bytes.fill(0)
        }
    }

    private fun decrypt(encoded: String, key: SecretKey): String {
        val parts = encoded.split(':')
        if (parts.size != 3 || parts[0] != ENCRYPTION_FORMAT) {
            throw SecureCredentialException("Unsupported encrypted device authentication format")
        }
        val plaintext = try {
            val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
            val iv = Base64.getUrlDecoder().decode(parts[1])
            try {
                cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, iv))
                cipher.updateAAD(aad())
                cipher.doFinal(Base64.getUrlDecoder().decode(parts[2]))
            } finally {
                iv.fill(0)
            }
        } catch (error: Exception) {
            throw SecureCredentialException("Unable to decrypt device authentication state", error)
        }
        return try {
            String(plaintext, StandardCharsets.UTF_8)
        } finally {
            plaintext.fill(0)
        }
    }

    private fun decryptLegacyToken(encoded: String): String {
        val parts = encoded.split(':')
        if (parts.size != 3 || parts[0] != LEGACY_WRAP_FORMAT) {
            throw SecureCredentialException("Unsupported legacy authentication format")
        }
        val key = existingKey()
            ?: throw SecureCredentialException("Legacy authentication key is unavailable")
        val plaintext = try {
            val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
            val iv = android.util.Base64.decode(parts[1], android.util.Base64.NO_WRAP)
            try {
                cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, iv))
                cipher.doFinal(android.util.Base64.decode(parts[2], android.util.Base64.NO_WRAP))
            } finally {
                iv.fill(0)
            }
        } catch (error: Exception) {
            throw SecureCredentialException("Unable to migrate legacy authentication", error)
        }
        return try {
            String(plaintext, StandardCharsets.UTF_8)
        } finally {
            plaintext.fill(0)
        }
    }

    private fun aad(): ByteArray =
        "dayweave/android/device-auth-envelope/v1|$preferencesName"
            .toByteArray(StandardCharsets.UTF_8)

    private fun verifiedEnvelopeReadback(): String? {
        val stored = preferences.getString(ENCRYPTED_ENVELOPE, null)
        return readbackOverride?.invoke(stored) ?: stored
    }

    private fun existingKey(): SecretKey? = try {
        keyAccess.existing(keyAlias)
    } catch (error: Exception) {
        throw SecureCredentialException("Unable to access device authentication key", error)
    }

    private fun getOrCreateKey(): SecretKey = existingKey() ?: try {
        keyAccess.create(keyAlias)
    } catch (error: Exception) {
        throw SecureCredentialException("Unable to create device authentication key", error)
    }

    companion object {
        const val PREFERENCES_NAME = "dayweave_api_credentials"
        const val KEY_ALIAS = "com.greengolddog.dayweave.api-token-wrapping-key.v1"

        internal const val ENCRYPTED_ENVELOPE = "device_auth_envelope"
        internal const val LEGACY_BASE_URL = "base_url"
        internal const val LEGACY_WRAPPED_TOKEN = "wrapped_bearer_token"
        internal const val LEGACY_CONFIGURATION_ID = "configuration_id"
        internal const val LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS = "last_successful_sync_epoch_millis"
        internal const val DESTROY_CLEANUP_PENDING = "device_auth_destroy_cleanup_pending"
        private const val NO_SYNC_RECORDED = -1L
        private const val LEGACY_ENVELOPE_VERSION = 1
        private const val INCOMPATIBLE_REVISION = 0L
        private const val ENCRYPTION_FORMAT = "dw_auth_v1"
        private const val LEGACY_WRAP_FORMAT = "v1"
        private const val CIPHER_TRANSFORMATION = "AES/GCM/NoPadding"
        private const val GCM_TAG_BITS = 128
        private val STORE_LOCK = Any()

        internal fun createForTest(
            context: Context,
            configuredBaseUrl: String,
            preferencesName: String,
            keyAlias: String,
            keyAccess: DeviceAuthKeyAccess,
            readbackOverride: ((String?) -> String?)? = null,
            legacyCleanupOverride: (() -> Boolean)? = null,
        ) = KeystoreDeviceAuthEnvelopeStore(
            context = context,
            configuredBaseUrl = configuredBaseUrl,
            preferencesName = preferencesName,
            keyAlias = keyAlias,
            keyAccess = keyAccess,
            readbackOverride = readbackOverride,
            legacyCleanupOverride = legacyCleanupOverride,
        )
    }
}

internal interface DeviceAuthKeyAccess {
    fun existing(alias: String): SecretKey?
    fun create(alias: String): SecretKey
    fun delete(alias: String)
}

private object AndroidDeviceAuthKeyAccess : DeviceAuthKeyAccess {
    private const val ANDROID_KEY_STORE = "AndroidKeyStore"

    override fun existing(alias: String): SecretKey? =
        KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }.getKey(alias, null) as? SecretKey

    override fun create(alias: String): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEY_STORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                alias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .build(),
        )
        return generator.generateKey()
    }

    override fun delete(alias: String) {
        KeyStore.getInstance(ANDROID_KEY_STORE).apply {
            load(null)
            if (containsAlias(alias)) deleteEntry(alias)
        }
    }
}
