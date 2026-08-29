package com.greengolddog.dayweave.security

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import androidx.annotation.RequiresApi
import java.nio.ByteBuffer
import java.security.GeneralSecurityException
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PublicKey
import java.security.SecureRandom
import java.security.Signature
import java.security.spec.ECGenParameterSpec

/**
 * Cryptographic half of local app-lock authentication.
 *
 * The private signing key is non-exportable and auth-per-use. A platform callback is accepted only
 * when its [Signature] can sign a fresh challenge that verifies against this exact Keystore key.
 * No planner data, credential, biometric, PIN, pattern, or password enters this component.
 */
internal interface AppLockCryptographicProof {
    /** Creates the auth-per-use operation passed to `BiometricPrompt.CryptoObject`. */
    fun createSigningOperation(): Signature

    /** Consumes the authenticated operation exactly once and verifies a fresh challenge. */
    fun verify(
        attempt: AppLockAuthenticationAttempt,
        authenticatedSignature: Signature,
    ): Boolean
}

internal class AndroidKeystoreAppLockCryptographicProof(
    private val secureRandom: SecureRandom = SecureRandom(),
) : AppLockCryptographicProof {
    private val keyLock = Any()

    override fun createSigningOperation(): Signature = synchronized(keyLock) {
        createSigningOperation(loadOrCreateKeyPair())
    }

    override fun verify(
        attempt: AppLockAuthenticationAttempt,
        authenticatedSignature: Signature,
    ): Boolean {
        val challenge = challengeFor(attempt)
        var signedChallenge = ByteArray(0)
        return try {
            authenticatedSignature.update(challenge)
            signedChallenge = authenticatedSignature.sign()
            val publicKey = synchronized(keyLock) { loadKeyPair()?.public } ?: return false
            verifyAppLockSignedChallenge(publicKey, challenge, signedChallenge)
        } catch (_: GeneralSecurityException) {
            false
        } catch (_: RuntimeException) {
            false
        } finally {
            challenge.fill(0)
            signedChallenge.fill(0)
        }
    }

    private fun createSigningOperation(keyPair: KeyPair): Signature = try {
        Signature.getInstance(SIGNATURE_ALGORITHM).apply { initSign(keyPair.private) }
    } catch (_: GeneralSecurityException) {
        // An invalidated auth key protects no ciphertext, so replacing it cannot disclose data or
        // bypass authentication. The replacement is still auth-per-use and must pass the prompt.
        createReplacementKeyPair().let { replacement ->
            Signature.getInstance(SIGNATURE_ALGORITHM).apply { initSign(replacement.private) }
        }
    }

    private fun loadOrCreateKeyPair(): KeyPair = loadKeyPair() ?: createReplacementKeyPair()

    private fun loadKeyPair(): KeyPair? {
        val keyStore = KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
        val privateKey = keyStore.getKey(KEY_ALIAS, null) ?: return null
        val publicKey = keyStore.getCertificate(KEY_ALIAS)?.publicKey ?: return null
        return KeyPair(publicKey, privateKey as? java.security.PrivateKey ?: return null)
    }

    private fun createReplacementKeyPair(): KeyPair = synchronized(keyLock) {
        val keyStore = KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
        if (keyStore.containsAlias(KEY_ALIAS)) keyStore.deleteEntry(KEY_ALIAS)

        val builder = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_SIGN,
        )
            .setAlgorithmParameterSpec(ECGenParameterSpec(EC_CURVE))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(true)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            configureModernAuthentication(builder)
        } else {
            @Suppress("DEPRECATION")
            builder.setUserAuthenticationValidityDurationSeconds(-1)
            builder.setInvalidatedByBiometricEnrollment(true)
        }

        KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEY_STORE).run {
            initialize(builder.build())
            generateKeyPair()
        }
    }

    @RequiresApi(Build.VERSION_CODES.R)
    private fun configureModernAuthentication(builder: KeyGenParameterSpec.Builder) {
        builder.setUserAuthenticationParameters(
            0,
            KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL,
        )
    }

    private fun challengeFor(attempt: AppLockAuthenticationAttempt): ByteArray {
        val nonce = ByteArray(CHALLENGE_NONCE_BYTES).also(secureRandom::nextBytes)
        return ByteBuffer.allocate(CHALLENGE_DOMAIN.size + nonce.size + Long.SIZE_BYTES * 2 + 1)
            .put(CHALLENGE_DOMAIN)
            .put(nonce)
            .putLong(attempt.processAttemptId)
            .putLong(attempt.controllerRequestId)
            .put(attempt.purpose.ordinal.toByte())
            .array()
            .also { nonce.fill(0) }
    }

    private companion object {
        const val ANDROID_KEY_STORE = "AndroidKeyStore"
        const val KEY_ALIAS = "dayweave.app-lock.proof.v1"
        const val SIGNATURE_ALGORITHM = "SHA256withECDSA"
        const val EC_CURVE = "secp256r1"
        const val CHALLENGE_NONCE_BYTES = 32
        val CHALLENGE_DOMAIN = "dayweave/app-lock/proof/v1\u0000".encodeToByteArray()
    }
}

/** Pure verification seam used by the production proof and deterministic JVM tests. */
internal fun verifyAppLockSignedChallenge(
    publicKey: PublicKey,
    challenge: ByteArray,
    signedChallenge: ByteArray,
): Boolean = try {
    Signature.getInstance("SHA256withECDSA").run {
        initVerify(publicKey)
        update(challenge)
        verify(signedChallenge)
    }
} catch (_: GeneralSecurityException) {
    false
} catch (_: RuntimeException) {
    false
}
