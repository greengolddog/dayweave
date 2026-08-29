package com.greengolddog.dayweave.network

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.nio.charset.StandardCharsets
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

data class ApiConnectionSnapshot(
    val baseUrl: String?,
    val hasBearerToken: Boolean,
    val lastSuccessfulSyncEpochMillis: Long?,
    val configurationId: String? = null,
)

/**
 * Authenticated request material. Its string representation is deliberately redacted so accidental
 * diagnostics cannot expose the bearer token.
 */
class AuthenticatedApiConfiguration private constructor(
    val baseUrl: HttpUrl,
    internal val bearerToken: String,
    internal val configurationId: String? = null,
) {
    override fun toString(): String =
        "AuthenticatedApiConfiguration(baseUrl=$baseUrl, bearerToken=<redacted>)"

    companion object {
        fun create(baseUrl: String, bearerToken: String): AuthenticatedApiConfiguration =
            AuthenticatedApiConfiguration(
                baseUrl = normalizeBaseUrl(baseUrl, allowCleartextLoopback = false),
                bearerToken = validateBearerToken(bearerToken),
            )

        internal fun createForLoopbackTest(
            baseUrl: String,
            bearerToken: String,
        ): AuthenticatedApiConfiguration = AuthenticatedApiConfiguration(
            baseUrl = normalizeBaseUrl(baseUrl, allowCleartextLoopback = true),
            bearerToken = validateBearerToken(bearerToken),
        )

        internal fun createBound(
            baseUrl: String,
            bearerToken: String,
            configurationId: String,
        ): AuthenticatedApiConfiguration = AuthenticatedApiConfiguration(
            baseUrl = normalizeBaseUrl(baseUrl, allowCleartextLoopback = false),
            bearerToken = validateBearerToken(bearerToken),
            configurationId = configurationId,
        )
    }
}

class InvalidApiConfigurationException(message: String) : IllegalArgumentException(message)

class SecureCredentialException(message: String, cause: Throwable? = null) :
    IllegalStateException(message, cause)

interface ApiCredentialStore {
    /** Returns only non-secret connection metadata. */
    fun snapshot(): ApiConnectionSnapshot

    /** Decrypts the token only when an authenticated request is about to run. */
    fun authenticatedConfiguration(): AuthenticatedApiConfiguration?

    /** A null token preserves an already stored token. */
    fun update(baseUrl: String, bearerToken: String?)

    fun clear()

    fun recordSuccessfulSync(epochMillis: Long)
}

/**
 * Stores the API base URL as non-secret metadata and only an AES-GCM wrapped bearer token.
 * The non-exportable wrapping key remains in Android Keystore. This preference file is also
 * excluded from cloud backup and device transfer.
 */
class KeystoreApiCredentialStore private constructor(
    context: Context,
    configuredBaseUrl: String,
    private val preferencesName: String = PREFERENCES_NAME,
    private val keyAlias: String = KEY_ALIAS,
    private val keyAccess: ApiCredentialKeyAccess,
    private val clearPreferenceRecordsOverride: (() -> Boolean)?,
) : ApiCredentialStore {
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
        keyAccess = AndroidApiCredentialKeyAccess,
        clearPreferenceRecordsOverride = null,
    )

    private val preferences = context.applicationContext.getSharedPreferences(
        preferencesName,
        Context.MODE_PRIVATE,
    )
    private val defaultBaseUrl = configuredBaseUrl
        .trim()
        .takeIf(String::isNotEmpty)
        ?.let { normalizeBaseUrl(it, allowCleartextLoopback = false).toString() }

    override fun snapshot(): ApiConnectionSnapshot = synchronized(CREDENTIAL_LOCK) {
        ApiConnectionSnapshot(
            baseUrl = effectiveBaseUrl(),
            // Ciphertext restored without its device-bound Keystore key is not a credential.
            // Treating it as configured would resurrect background work after a partial forget.
            hasBearerToken = preferences.contains(WRAPPED_BEARER_TOKEN) && hasWrappingKey(),
            lastSuccessfulSyncEpochMillis = preferences
                .getLong(LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS, NO_SYNC_RECORDED)
                .takeUnless { it == NO_SYNC_RECORDED },
            configurationId = preferences.getString(CONFIGURATION_ID, null),
        )
    }

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
        synchronized(CREDENTIAL_LOCK) {
            val baseUrl = effectiveBaseUrl() ?: return@synchronized null
            val wrappedToken = preferences.getString(WRAPPED_BEARER_TOKEN, null)
                ?: return@synchronized null
            val configurationId = preferences.getString(CONFIGURATION_ID, null)
                ?: throw SecureCredentialException("The API credential binding is unavailable")
            AuthenticatedApiConfiguration.createBound(
                baseUrl = baseUrl,
                bearerToken = unwrap(wrappedToken),
                configurationId = configurationId,
            )
        }

    @SuppressLint("UseKtx")
    override fun update(baseUrl: String, bearerToken: String?) = synchronized(CREDENTIAL_LOCK) {
        val normalizedBaseUrl = normalizeBaseUrl(
            baseUrl.trim(),
            allowCleartextLoopback = false,
        ).toString()
        val previousBaseUrl = effectiveBaseUrl()
        if (bearerToken == null && previousBaseUrl != normalizedBaseUrl) {
            throw InvalidApiConfigurationException(
                "Enter a replacement bearer token when changing the API URL",
            )
        }
        val encodedToken = bearerToken?.let { token ->
            val validated = validateBearerToken(token)
            wrap(validated, getOrCreateWrappingKey())
        }

        val editor = preferences.edit()
            .putString(BASE_URL, normalizedBaseUrl)
            .putString(CONFIGURATION_ID, UUID.randomUUID().toString())
        if (encodedToken != null) editor.putString(WRAPPED_BEARER_TOKEN, encodedToken)
        check(editor.commit()) { "Unable to persist API connection settings" }
    }

    override fun clear() = synchronized(CREDENTIAL_LOCK) {
        var preferenceFailure: Exception? = null
        try {
            check(clearPreferenceRecords()) {
                "Unable to durably clear API connection settings"
            }
        } catch (error: Exception) {
            preferenceFailure = error
        }

        var keyFailure: Exception? = null
        try {
            keyAccess.delete(keyAlias)
        } catch (error: Exception) {
            keyFailure = error
        }

        if (preferenceFailure != null || keyFailure != null) {
            val primary = preferenceFailure ?: requireNotNull(keyFailure)
            if (preferenceFailure != null && keyFailure != null) {
                primary.addSuppressed(keyFailure)
            }
            throw SecureCredentialException(
                "API credentials could not be completely removed from this device",
                primary,
            )
        }
        Unit
    }

    @SuppressLint("UseKtx")
    override fun recordSuccessfulSync(epochMillis: Long) = synchronized(CREDENTIAL_LOCK) {
        require(epochMillis >= 0) { "Sync time cannot be negative" }
        check(
            preferences.edit()
                .putLong(LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS, epochMillis)
                .commit(),
        ) { "Unable to persist the last successful sync time" }
    }

    private fun effectiveBaseUrl(): String? =
        preferences.getString(BASE_URL, null)?.takeIf(String::isNotBlank) ?: defaultBaseUrl

    private fun unwrap(encoded: String): String {
        val parts = encoded.split(':')
        if (parts.size != 3 || parts[0] != WRAP_FORMAT) {
            throw SecureCredentialException("Unsupported encrypted API credential format")
        }
        val key = existingWrappingKey()
            ?: throw SecureCredentialException(
                "The API credential key is unavailable; re-enter the bearer token",
            )

        val plaintext = try {
            val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
            val initializationVector = Base64.decode(parts[1], Base64.NO_WRAP)
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, initializationVector))
            cipher.doFinal(Base64.decode(parts[2], Base64.NO_WRAP))
        } catch (error: Exception) {
            throw SecureCredentialException(
                "Unable to decrypt the API credential; re-enter the bearer token",
                error,
            )
        }

        return try {
            validateBearerToken(String(plaintext, StandardCharsets.UTF_8))
        } finally {
            plaintext.fill(0)
        }
    }

    private fun wrap(token: String, key: SecretKey): String {
        val plaintext = token.toByteArray(StandardCharsets.UTF_8)
        return try {
            val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, key)
            val ciphertext = cipher.doFinal(plaintext)
            listOf(
                WRAP_FORMAT,
                Base64.encodeToString(cipher.iv, Base64.NO_WRAP),
                Base64.encodeToString(ciphertext, Base64.NO_WRAP),
            ).joinToString(":")
        } catch (error: Exception) {
            throw SecureCredentialException("Unable to encrypt the API credential", error)
        } finally {
            plaintext.fill(0)
        }
    }

    // Forget must know whether ciphertext reached durable storage; apply() cannot report failure.
    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun clearPreferenceRecords(): Boolean = clearPreferenceRecordsOverride?.invoke()
        ?: preferences.edit()
            .remove(BASE_URL)
            .remove(WRAPPED_BEARER_TOKEN)
            .remove(LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS)
            .remove(CONFIGURATION_ID)
            .commit()

    private fun hasWrappingKey(): Boolean = try {
        existingWrappingKey() != null
    } catch (_: SecureCredentialException) {
        // Scheduling must fail closed when Keystore metadata is temporarily or permanently
        // unavailable. The authenticated request path will surface the actionable error.
        false
    }

    private fun existingWrappingKey(): SecretKey? = try {
        keyAccess.existing(keyAlias)
    } catch (error: Exception) {
        throw SecureCredentialException("Unable to access the API credential key", error)
    }

    private fun getOrCreateWrappingKey(): SecretKey = existingWrappingKey() ?: try {
        keyAccess.create(keyAlias)
    } catch (error: Exception) {
        throw SecureCredentialException("Unable to create the API credential key", error)
    }

    companion object {
        const val PREFERENCES_NAME = "dayweave_api_credentials"
        const val KEY_ALIAS = "com.greengolddog.dayweave.api-token-wrapping-key.v1"

        internal fun createForTest(
            context: Context,
            configuredBaseUrl: String,
            preferencesName: String,
            keyAlias: String,
            keyAccess: ApiCredentialKeyAccess,
            clearPreferenceRecords: () -> Boolean,
        ) = KeystoreApiCredentialStore(
            context = context,
            configuredBaseUrl = configuredBaseUrl,
            preferencesName = preferencesName,
            keyAlias = keyAlias,
            keyAccess = keyAccess,
            clearPreferenceRecordsOverride = clearPreferenceRecords,
        )

        private const val BASE_URL = "base_url"
        private const val WRAPPED_BEARER_TOKEN = "wrapped_bearer_token"
        private const val LAST_SUCCESSFUL_SYNC_EPOCH_MILLIS = "last_successful_sync_epoch_millis"
        private const val CONFIGURATION_ID = "configuration_id"
        private const val NO_SYNC_RECORDED = -1L
        private const val WRAP_FORMAT = "v1"
        private const val KEY_SIZE_BITS = 256
        private const val GCM_TAG_BITS = 128
        private const val ANDROID_KEY_STORE = "AndroidKeyStore"
        private const val CIPHER_TRANSFORMATION = "AES/GCM/NoPadding"
        private val CREDENTIAL_LOCK = Any()

        private object AndroidApiCredentialKeyAccess : ApiCredentialKeyAccess {
            override fun existing(alias: String): SecretKey? =
                KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
                    .getKey(alias, null) as? SecretKey

            override fun create(alias: String): SecretKey {
                val generator = KeyGenerator.getInstance(
                    KeyProperties.KEY_ALGORITHM_AES,
                    ANDROID_KEY_STORE,
                )
                generator.init(
                    KeyGenParameterSpec.Builder(
                        alias,
                        KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                    )
                        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                        .setKeySize(KEY_SIZE_BITS)
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
    }
}

internal interface ApiCredentialKeyAccess {
    fun existing(alias: String): SecretKey?

    fun create(alias: String): SecretKey

    fun delete(alias: String)
}

private fun normalizeBaseUrl(rawBaseUrl: String, allowCleartextLoopback: Boolean): HttpUrl {
    val parsed = rawBaseUrl.toHttpUrlOrNull()
        ?: throw InvalidApiConfigurationException("Enter a valid DayWeave API URL")
    val cleartextLoopback = allowCleartextLoopback &&
        parsed.scheme == "http" &&
        parsed.host in setOf("localhost", "127.0.0.1", "::1")
    if (parsed.scheme != "https" && !cleartextLoopback) {
        throw InvalidApiConfigurationException("The DayWeave API URL must use HTTPS")
    }
    if (
        parsed.username.isNotEmpty() ||
        parsed.password.isNotEmpty() ||
        parsed.query != null ||
        parsed.fragment != null
    ) {
        throw InvalidApiConfigurationException(
            "The DayWeave API URL cannot contain credentials, a query, or a fragment",
        )
    }
    return if (parsed.encodedPath.endsWith('/')) {
        parsed
    } else {
        parsed.newBuilder().addPathSegment("").build()
    }
}

internal fun normalizedHttpsApiBaseUrl(rawBaseUrl: String): String =
    normalizeBaseUrl(rawBaseUrl.trim(), allowCleartextLoopback = false).toString()

private fun validateBearerToken(token: String): String {
    if (token.isBlank() || token.any(Char::isWhitespace)) {
        throw InvalidApiConfigurationException("Enter a bearer token without spaces")
    }
    if (token.length > MAX_BEARER_TOKEN_LENGTH) {
        throw InvalidApiConfigurationException("The bearer token is unexpectedly long")
    }
    return token
}

private const val MAX_BEARER_TOKEN_LENGTH = 8_192
