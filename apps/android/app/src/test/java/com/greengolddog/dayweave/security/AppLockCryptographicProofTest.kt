package com.greengolddog.dayweave.security

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.spec.ECGenParameterSpec

class AppLockCryptographicProofTest {
    @Test
    fun platformSuccessWithoutAValidCryptographicProofFailsClosed() {
        val attempt = AppLockAuthenticationAttempt(
            processAttemptId = 7,
            controllerRequestId = 11,
            purpose = AppLockAuthenticationPurpose.UNLOCK,
        )
        val signature = Signature.getInstance("SHA256withECDSA")

        assertTrue(
            cryptographicAuthenticationOutcome(FakeProof(result = true), attempt, signature) ==
                AppLockAuthenticationOutcome.SUCCESS,
        )
        assertTrue(
            cryptographicAuthenticationOutcome(FakeProof(result = false), attempt, signature) ==
                AppLockAuthenticationOutcome.ERROR,
        )
        assertTrue(
            cryptographicAuthenticationOutcome(FakeProof(result = true), attempt, null) ==
                AppLockAuthenticationOutcome.ERROR,
        )
        assertTrue(
            cryptographicAuthenticationOutcome(
                FakeProof(result = true, throws = true),
                attempt,
                signature,
            ) == AppLockAuthenticationOutcome.ERROR,
        )
    }

    @Test
    fun signedChallengeIsBoundToTheExpectedKeyAndBytes() {
        val expected = keyPair()
        val other = keyPair()
        val challenge = "synthetic-app-lock-challenge".encodeToByteArray()
        val signature = Signature.getInstance("SHA256withECDSA").run {
            initSign(expected.private)
            update(challenge)
            sign()
        }

        assertTrue(verifyAppLockSignedChallenge(expected.public, challenge, signature))
        assertFalse(
            verifyAppLockSignedChallenge(
                expected.public,
                "different-challenge".encodeToByteArray(),
                signature,
            ),
        )
        assertFalse(verifyAppLockSignedChallenge(other.public, challenge, signature))
        assertFalse(verifyAppLockSignedChallenge(expected.public, challenge, byteArrayOf(1, 2, 3)))
    }

    private fun keyPair() = KeyPairGenerator.getInstance("EC").run {
        initialize(ECGenParameterSpec("secp256r1"))
        generateKeyPair()
    }
}

private class FakeProof(
    private val result: Boolean,
    private val throws: Boolean = false,
) : AppLockCryptographicProof {
    override fun createSigningOperation(): Signature = Signature.getInstance("SHA256withECDSA")

    override fun verify(
        attempt: AppLockAuthenticationAttempt,
        authenticatedSignature: Signature,
    ): Boolean {
        if (throws) error("synthetic verification failure")
        return result
    }
}
