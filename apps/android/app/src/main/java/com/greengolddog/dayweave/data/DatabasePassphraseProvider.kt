package com.greengolddog.dayweave.data

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

fun interface DatabasePassphraseProvider {
    fun getOrCreatePassphrase(): ByteArray
}

/**
 * Stores only an AES-GCM wrapped random database passphrase. The wrapping key is non-exportable and
 * remains in Android Keystore; neither the key nor plaintext passphrase is written to app storage.
 */
class KeystoreWrappedPassphraseProvider(
    context: Context,
    private val preferencesName: String = PREFERENCES_NAME,
    private val keyAlias: String = KEY_ALIAS,
    private val databaseName: String = PlannerDatabaseFactory.DATABASE_NAME,
) : DatabasePassphraseProvider {
    private val applicationContext = context.applicationContext
    private val preferences = applicationContext.getSharedPreferences(
        preferencesName,
        Context.MODE_PRIVATE,
    )

    // KTX's edit helper discards commit()'s result; failure must remain observable here.
    @SuppressLint("UseKtx")
    override fun getOrCreatePassphrase(): ByteArray = synchronized(KEY_CREATION_LOCK) {
        val wrappedPassphrase = preferences.getString(WRAPPED_PASSPHRASE, null)
        if (wrappedPassphrase != null) {
            return@synchronized unwrap(wrappedPassphrase)
        }
        check(!encryptedDatabaseFilesExist()) {
            "Encrypted database exists without its wrapped passphrase; refusing to replace the key"
        }

        val passphrase = ByteArray(PASSPHRASE_BYTES).also(SecureRandom()::nextBytes)
        try {
            val encoded = wrap(passphrase, getOrCreateWrappingKey())
            check(preferences.edit().putString(WRAPPED_PASSPHRASE, encoded).commit()) {
                "Unable to persist the wrapped database passphrase"
            }
            passphrase.copyOf()
        } finally {
            passphrase.fill(0)
        }
    }

    private fun encryptedDatabaseFilesExist(): Boolean {
        val database = applicationContext.getDatabasePath(databaseName)
        val parent = database.parentFile ?: return database.exists()
        return database.exists() || DATABASE_SIDECAR_SUFFIXES.any { suffix ->
            parent.resolve("${database.name}$suffix").exists()
        }
    }

    private fun unwrap(encoded: String): ByteArray {
        val parts = encoded.split(':')
        check(parts.size == 3 && parts[0] == WRAP_FORMAT) {
            "Unsupported wrapped database passphrase format"
        }
        val key = existingWrappingKey()
            ?: error("The database wrapping key is unavailable; refusing to replace it")
        return try {
            val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
            val initializationVector = Base64.decode(parts[1], Base64.NO_WRAP)
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, initializationVector))
            cipher.doFinal(Base64.decode(parts[2], Base64.NO_WRAP)).also {
                check(it.size == PASSPHRASE_BYTES) { "Invalid database passphrase length" }
            }
        } catch (error: Exception) {
            throw IllegalStateException("Unable to unwrap the database passphrase", error)
        }
    }

    private fun wrap(passphrase: ByteArray, key: SecretKey): String {
        val cipher = Cipher.getInstance(CIPHER_TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val ciphertext = cipher.doFinal(passphrase)
        return listOf(
            WRAP_FORMAT,
            Base64.encodeToString(cipher.iv, Base64.NO_WRAP),
            Base64.encodeToString(ciphertext, Base64.NO_WRAP),
        ).joinToString(":")
    }

    private fun existingWrappingKey(): SecretKey? {
        val keyStore = KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
        return keyStore.getKey(keyAlias, null) as? SecretKey
    }

    private fun getOrCreateWrappingKey(): SecretKey = existingWrappingKey() ?: run {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEY_STORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                keyAlias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .build(),
        )
        generator.generateKey()
    }

    companion object {
        const val PREFERENCES_NAME = "auth"
        const val KEY_ALIAS = "com.greengolddog.dayweave.database-wrapping-key.v1"

        private const val WRAPPED_PASSPHRASE = "wrapped_database_passphrase"
        private const val WRAP_FORMAT = "v1"
        private const val PASSPHRASE_BYTES = 32
        private const val GCM_TAG_BITS = 128
        private const val ANDROID_KEY_STORE = "AndroidKeyStore"
        private const val CIPHER_TRANSFORMATION = "AES/GCM/NoPadding"
        private val DATABASE_SIDECAR_SUFFIXES = listOf("-journal", "-wal", "-shm")
        private val KEY_CREATION_LOCK = Any()
    }
}
