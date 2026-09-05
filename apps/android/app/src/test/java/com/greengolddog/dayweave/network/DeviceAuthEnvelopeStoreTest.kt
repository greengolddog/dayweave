package com.greengolddog.dayweave.network

import android.content.Context
import java.time.Instant
import java.security.SecureRandom
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class DeviceAuthEnvelopeStoreTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val preferences by lazy {
        context.getSharedPreferences(TEST_PREFERENCES, Context.MODE_PRIVATE)
    }

    @After
    fun tearDown() {
        preferences.edit().clear().commit()
    }

    @Test
    fun envelopeIsEncryptedAndCasRequiresExactDurableIdentity() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        val initial = store.read()
        val token = "synthetic-bootstrap"
        val legacy = StoredDeviceAuthState.Legacy(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
            bindingId = "33333333-3333-4333-8333-333333333333",
            bootstrapToken = DeviceAuthSecret(token),
        )

        assertTrue(store.compareAndSet(initial, legacy))
        val ciphertext = preferences.getString(
            KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE,
            null,
        )
        assertNotNull(ciphertext)
        assertFalse(requireNotNull(ciphertext).contains(token))
        assertFalse(ciphertext.contains(SYNTHETIC_CLIENT_INSTANCE_ID))

        val current = store.read()
        assertEquals(initial.revision + 1, current.revision)
        assertEquals(legacy, current.state)
        assertNotEquals(initial.storageIdentity, current.storageIdentity)
        assertFalse(
            store.compareAndSet(
                initial,
                StoredDeviceAuthState.Unconfigured(
                    baseUrl = null,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )
    }

    @Test
    fun legacyRecordsSurviveFailedEnvelopeReadbackAndCleanupRetriesLater() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val legacyStore = KeystoreApiCredentialStore.createForTest(
            context = context,
            configuredBaseUrl = "",
            preferencesName = TEST_PREFERENCES,
            keyAlias = TEST_KEY_ALIAS,
            keyAccess = keys,
            clearPreferenceRecords = { preferences.edit().clear().commit() },
        )
        legacyStore.update(SYNTHETIC_BASE_URL, "synthetic-bootstrap")
        assertTrue(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_WRAPPED_TOKEN))

        val corruptReadback = testStore(keys, readbackOverride = { "corrupt-readback" })
        assertThrows(IllegalStateException::class.java) { corruptReadback.read() }
        assertTrue(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_BASE_URL))
        assertTrue(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_WRAPPED_TOKEN))
        assertTrue(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_CONFIGURATION_ID))

        var cleanupAllowed = false
        val recovered = testStore(
            keys,
            legacyCleanupOverride = { cleanupAllowed },
        )
        assertTrue(recovered.read().state is StoredDeviceAuthState.Legacy)
        assertTrue(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_WRAPPED_TOKEN))
        cleanupAllowed = true
        assertTrue(recovered.read().state is StoredDeviceAuthState.Legacy)
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_BASE_URL))
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_WRAPPED_TOKEN))
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.LEGACY_CONFIGURATION_ID))
    }

    @Test
    fun changedMalformedCiphertextCannotReuseIncompatibleIdentityForDestroy() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, "malformed-a")
            .commit()
        val first = store.read()
        assertTrue(first.state is StoredDeviceAuthState.Incompatible)

        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, "malformed-b")
            .commit()
        val changed = store.read()
        assertTrue(changed.state is StoredDeviceAuthState.Incompatible)
        assertNotEquals(first.storageIdentity, changed.storageIdentity)
        assertEquals(DeviceAuthDestroyResult.STALE, store.destroy(first))
        assertEquals(
            "malformed-b",
            preferences.getString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, null),
        )
        assertTrue(keys.hasKey(TEST_KEY_ALIAS))
    }

    @Test
    fun destroyAndReinitializeCannotAbaThroughRevisionReset() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        val old = store.read()
        assertEquals(1L, old.revision)
        assertEquals(DeviceAuthDestroyResult.DESTROYED, store.destroy(old))
        assertFalse(keys.hasKey(TEST_KEY_ALIAS))

        val replacement = store.read()
        assertEquals(1L, replacement.revision)
        assertNotEquals(old.storageIdentity, replacement.storageIdentity)
        assertNotEquals(old.state.clientInstanceId, replacement.state.clientInstanceId)
        assertEquals(DeviceAuthDestroyResult.STALE, store.destroy(old))
        assertFalse(
            store.compareAndSet(
                old,
                StoredDeviceAuthState.Unconfigured(
                    baseUrl = null,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )
        assertEquals(replacement, store.read())
    }

    @Test
    fun maxRevisionEnvelopeLoadsAsDeterministicIncompatibleState() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        val plaintext = Json {
            classDiscriminator = "phase"
            ignoreUnknownKeys = false
            explicitNulls = true
            encodeDefaults = true
        }.encodeToString(
            StoredDeviceAuthEnvelope(
                schemaVersion = DEVICE_AUTH_ENVELOPE_VERSION,
                revision = Long.MAX_VALUE,
                state = StoredDeviceAuthState.Unconfigured(
                    baseUrl = null,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )
        val encrypt = store.javaClass.getDeclaredMethod(
            "encrypt",
            String::class.java,
            SecretKey::class.java,
        ).apply { isAccessible = true }
        val encoded = encrypt.invoke(
            store,
            plaintext,
            requireNotNull(keys.existing(TEST_KEY_ALIAS)),
        ) as String
        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, encoded)
            .commit()

        val loaded = store.read()

        assertTrue(loaded.state is StoredDeviceAuthState.Incompatible)
        assertEquals(0L, loaded.revision)
        assertFalse(
            store.compareAndSet(
                loaded,
                StoredDeviceAuthState.Unconfigured(
                    baseUrl = null,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )

        val nearMaximumPlaintext = Json {
            classDiscriminator = "phase"
            ignoreUnknownKeys = false
            explicitNulls = true
            encodeDefaults = true
        }.encodeToString(
            StoredDeviceAuthEnvelope(
                schemaVersion = DEVICE_AUTH_ENVELOPE_VERSION,
                revision = Long.MAX_VALUE - 1,
                state = StoredDeviceAuthState.Unconfigured(
                    baseUrl = null,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )
        val nearMaximumEncoded = encrypt.invoke(
            store,
            nearMaximumPlaintext,
            requireNotNull(keys.existing(TEST_KEY_ALIAS)),
        ) as String
        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, nearMaximumEncoded)
            .commit()
        val nearMaximum = store.read()
        assertEquals(Long.MAX_VALUE - 1, nearMaximum.revision)
        assertFalse(
            store.compareAndSet(
                nearMaximum,
                StoredDeviceAuthState.Unconfigured(
                    baseUrl = SYNTHETIC_BASE_URL,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )
        assertEquals(nearMaximum, store.read())
    }

    @Test
    fun versionOnePendingJournalCannotBeReconstructedFromCurrentSettings() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        val oldPending = StoredDeviceAuthState.EnrollmentPending(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
            sessionId = SYNTHETIC_SESSION_ID,
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            preparedAt = "2026-08-29T12:00:00Z",
            scopes = ANDROID_DEVICE_AUTH_SCOPES,
            capabilities = ANDROID_DEVICE_AUTH_CAPABILITIES,
            enrollmentToken = DeviceAuthSecret(
                syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 201),
            ),
            accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 202)),
            refreshToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 203)),
        )
        val plaintext = Json {
            classDiscriminator = "phase"
            ignoreUnknownKeys = false
            explicitNulls = true
            encodeDefaults = true
        }.encodeToString(
            StoredDeviceAuthEnvelope(
                schemaVersion = 1,
                revision = 7,
                state = oldPending,
            ),
        )
        val encrypt = store.javaClass.getDeclaredMethod(
            "encrypt",
            String::class.java,
            SecretKey::class.java,
        ).apply { isAccessible = true }
        val encoded = encrypt.invoke(
            store,
            plaintext,
            requireNotNull(keys.existing(TEST_KEY_ALIAS)),
        ) as String
        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, encoded)
            .commit()

        val loaded = store.read()

        val incompatible = loaded.state as StoredDeviceAuthState.Incompatible
        assertEquals("legacy_pending_request_binding_unavailable", incompatible.reason)
        assertEquals(0L, loaded.revision)
    }

    @Test
    fun contractOneSessionWithoutPublishScopeFailsClosedForReenrollment() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        val now = java.time.Instant.parse("2026-08-29T12:00:00Z")
        val oldSession = syntheticSession(now).copy(
            scopes = ANDROID_DEVICE_AUTH_SCOPES.filterNot { it == "schedule_publish" },
            clientContractVersion = 1,
        )
        val plaintext = Json {
            classDiscriminator = "phase"
            ignoreUnknownKeys = false
            explicitNulls = true
            encodeDefaults = true
        }.encodeToString(
            StoredDeviceAuthEnvelope(
                schemaVersion = DEVICE_AUTH_ENVELOPE_VERSION,
                revision = 7,
                state = syntheticActiveState(now, session = oldSession),
            ),
        )
        val encrypt = store.javaClass.getDeclaredMethod(
            "encrypt",
            String::class.java,
            SecretKey::class.java,
        ).apply { isAccessible = true }
        val encoded = encrypt.invoke(
            store,
            plaintext,
            requireNotNull(keys.existing(TEST_KEY_ALIAS)),
        ) as String
        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, encoded)
            .commit()

        val loaded = store.read()

        assertTrue(loaded.state is StoredDeviceAuthState.Incompatible)
        assertEquals("stored_state_invalid", (loaded.state as StoredDeviceAuthState.Incompatible).reason)
        assertEquals(0L, loaded.revision)
        assertFalse(
            store.compareAndSet(
                loaded,
                StoredDeviceAuthState.Unconfigured(
                    baseUrl = null,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                ),
            ),
        )
    }

    @Test
    fun versionTwoEnvelopeMigratesToEncryptedVersionThreeWithoutInventingRecoveryState() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        val active = syntheticActiveState(Instant.parse("2026-09-05T09:00:00Z"))
        val json = envelopeJson()
        val versionTwo = json.encodeToString(
            StoredDeviceAuthEnvelope(
                schemaVersion = 2,
                revision = 7,
                state = active,
            ),
        ).replace(",\"account_recovery_journal\":null", "")
        writeEncrypted(store, keys, versionTwo)

        val migrated = store.read()

        assertEquals(DEVICE_AUTH_ENVELOPE_VERSION, migrated.schemaVersion)
        assertEquals(8L, migrated.revision)
        assertEquals(active, migrated.state)
        assertEquals(null, migrated.accountRecoveryJournal)
        val ciphertext = requireNotNull(
            preferences.getString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, null),
        )
        assertFalse(ciphertext.contains(active.accessToken.value))
        assertFalse(ciphertext.contains(active.clientInstanceId))
    }

    @Test
    fun recoveryJournalIsEncryptedAndAdvancesWithTheCredentialEnvelope() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        val initial = store.read()
        val active = syntheticActiveState(Instant.parse("2026-09-05T09:00:00Z"))
        assertTrue(store.compareAndSet(initial, active))
        val activeEnvelope = store.read()
        val code = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 91)
        val journal = StoredAccountRecoveryJournal.DisclosurePending(
            baseUrl = SYNTHETIC_BASE_URL,
            id = "55555555-5555-4555-8555-555555555555",
            code = DeviceAuthSecret(code),
            createdAt = "2026-09-05T09:00:00Z",
            revision = 1,
            source = "issued",
        )

        assertTrue(store.compareAndSet(activeEnvelope, active, journal))

        assertEquals(journal, store.read().accountRecoveryJournal)
        val ciphertext = requireNotNull(
            preferences.getString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, null),
        )
        assertFalse(ciphertext.contains(code))
        assertFalse(ciphertext.contains(journal.id))
    }

    @Test
    fun unknownRecoveryJournalRequiresExactRecoveryOnlyRepairAndPreservesSession() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        val active = syntheticActiveState(Instant.parse("2026-09-05T09:00:00Z"))
        val base = envelopeJson().encodeToString(
            StoredDeviceAuthEnvelope(
                revision = 9,
                state = active,
            ),
        )
        val future = base.replace(
            "\"account_recovery_journal\":null",
            "\"account_recovery_journal\":{" +
                "\"phase\":\"future_pending\",\"opaque\":\"preserved\"}",
        )
        writeEncrypted(store, keys, future)

        val loaded = store.read()

        assertEquals(active, loaded.state)
        val repair = loaded.accountRecoveryJournal as
            StoredAccountRecoveryJournal.RepairRequired
        assertEquals(RECOVERY_JOURNAL_UNSUPPORTED, repair.reason)
        assertTrue(store.compareAndSet(loaded, loaded.state, null))
        assertEquals(active, store.read().state)
        assertEquals(null, store.read().accountRecoveryJournal)
    }

    @Test
    fun malformedKnownRecoveryJournalDoesNotDestroyValidDeviceAuthentication() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        store.read()
        val active = syntheticActiveState(Instant.parse("2026-09-05T09:00:00Z"))
        val malformed = StoredAccountRecoveryJournal.IssuancePending(
            baseUrl = SYNTHETIC_BASE_URL,
            configurationId = active.session.id,
            clientInstanceId = active.clientInstanceId,
            candidateId = "55555555-5555-4555-8555-555555555555",
            candidateCode = DeviceAuthSecret("not-a-recovery-code"),
            replacesId = null,
            replacesRevision = null,
            preparedAt = "2026-09-05T09:00:00Z",
        )
        writeEncrypted(
            store,
            keys,
            envelopeJson().encodeToString(
                StoredDeviceAuthEnvelope(
                    revision = 10,
                    state = active,
                    accountRecoveryJournal = malformed,
                ),
            ),
        )

        val loaded = store.read()

        assertEquals(active, loaded.state)
        val repair = loaded.accountRecoveryJournal as
            StoredAccountRecoveryJournal.RepairRequired
        assertEquals(RECOVERY_JOURNAL_MALFORMED, repair.reason)
    }

    @Test
    fun compareAndSetAndDestroyVerifyExactReadback() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val corrupt = testStore(keys, readbackOverride = { stored ->
            if (stored == null) null else stored.dropLast(1) + "x"
        })
        assertThrows(IllegalStateException::class.java) { corrupt.read() }

        preferences.edit().clear().commit()
        keys.delete(TEST_KEY_ALIAS)
        val store = testStore(keys)
        val expected = store.read()
        assertEquals(DeviceAuthDestroyResult.DESTROYED, store.destroy(expected))
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE))
        assertFalse(keys.hasKey(TEST_KEY_ALIAS))
    }

    @Test
    fun removedCiphertextNeverReappearsWhenObsoleteKeyCleanupMustRetry() {
        val keys = InMemoryDeviceAuthKeyAccess()
        val store = testStore(keys)
        val expected = store.read()
        keys.failDeleteAttempts = 2

        assertEquals(
            DeviceAuthDestroyResult.CREDENTIALS_DESTROYED_CLEANUP_PENDING,
            store.destroy(expected),
        )
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE))
        assertTrue(preferences.getBoolean(KeystoreDeviceAuthEnvelopeStore.DESTROY_CLEANUP_PENDING, false))
        assertTrue(keys.hasKey(TEST_KEY_ALIAS))

        val pending = store.read()
        assertTrue(pending.state is StoredDeviceAuthState.Incompatible)
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE))

        val recovered = store.read()
        assertTrue(recovered.state is StoredDeviceAuthState.Unconfigured)
        assertFalse(preferences.contains(KeystoreDeviceAuthEnvelopeStore.DESTROY_CLEANUP_PENDING))
    }

    private fun testStore(
        keys: InMemoryDeviceAuthKeyAccess,
        readbackOverride: ((String?) -> String?)? = null,
        legacyCleanupOverride: (() -> Boolean)? = null,
    ) = KeystoreDeviceAuthEnvelopeStore.createForTest(
        context = context,
        configuredBaseUrl = "",
        preferencesName = TEST_PREFERENCES,
        keyAlias = TEST_KEY_ALIAS,
        keyAccess = keys,
        readbackOverride = readbackOverride,
        legacyCleanupOverride = legacyCleanupOverride,
    )

    private fun envelopeJson() = Json {
        classDiscriminator = "phase"
        ignoreUnknownKeys = false
        explicitNulls = true
        encodeDefaults = true
    }

    private fun writeEncrypted(
        store: KeystoreDeviceAuthEnvelopeStore,
        keys: InMemoryDeviceAuthKeyAccess,
        plaintext: String,
    ) {
        val encrypt = store.javaClass.getDeclaredMethod(
            "encrypt",
            String::class.java,
            SecretKey::class.java,
        ).apply { isAccessible = true }
        val encoded = encrypt.invoke(
            store,
            plaintext,
            requireNotNull(keys.existing(TEST_KEY_ALIAS)),
        ) as String
        preferences.edit()
            .putString(KeystoreDeviceAuthEnvelopeStore.ENCRYPTED_ENVELOPE, encoded)
            .commit()
    }

    private companion object {
        const val TEST_PREFERENCES = "durable-device-auth-envelope-test"
        const val TEST_KEY_ALIAS = "com.greengolddog.dayweave.test-device-auth-key"
    }
}

private class InMemoryDeviceAuthKeyAccess : DeviceAuthKeyAccess, ApiCredentialKeyAccess {
    private val keys = mutableMapOf<String, SecretKey>()
    var failDeleteAttempts: Int = 0

    override fun existing(alias: String): SecretKey? = keys[alias]

    override fun create(alias: String): SecretKey = KeyGenerator.getInstance("AES")
        .apply { init(256, SecureRandom()) }
        .generateKey()
        .also { keys[alias] = it }

    override fun delete(alias: String) {
        if (failDeleteAttempts > 0) {
            failDeleteAttempts -= 1
            throw IllegalStateException("synthetic key cleanup failure")
        }
        keys.remove(alias)
    }

    fun hasKey(alias: String): Boolean = alias in keys
}
