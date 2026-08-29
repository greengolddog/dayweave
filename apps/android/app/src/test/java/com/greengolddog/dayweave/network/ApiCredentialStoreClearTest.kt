package com.greengolddog.dayweave.network

import android.content.Context
import com.greengolddog.dayweave.sync.SuggestionSyncWorkBackend
import com.greengolddog.dayweave.sync.SuggestionSyncWorkPolicy
import com.greengolddog.dayweave.sync.SuggestionSyncSchedulingCoordinator
import java.security.SecureRandom
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class ApiCredentialStoreClearTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val preferences by lazy {
        context.getSharedPreferences(TEST_PREFERENCES, Context.MODE_PRIVATE)
    }

    @After
    fun tearDown() {
        preferences.edit().clear().commit()
    }

    @Test
    fun preferenceClearFailureStillDestroysKeyAndRestartCannotReloadCiphertext() {
        val keys = InMemoryCredentialKeyAccess()
        val store = testStore(keys, clearPreferenceRecords = { false })
        store.update(TEST_BASE_URL, TEST_TOKEN)
        assertTrue(store.snapshot().hasBearerToken)

        assertThrows(SecureCredentialException::class.java) { store.clear() }

        assertEquals(1, keys.deleteAttempts)
        assertFalse(keys.hasKey(TEST_KEY_ALIAS))
        assertTrue(preferences.all.isNotEmpty())

        val restarted = testStore(keys, clearPreferenceRecords = { false })
        assertFalse(restarted.snapshot().hasBearerToken)
        assertThrows(SecureCredentialException::class.java) {
            restarted.authenticatedConfiguration()
        }
        val backend = RecordingSyncBackend()
        SuggestionSyncSchedulingCoordinator(restarted, backend).onAppStart()
        assertTrue(backend.cancelled)
        assertEquals(0, backend.enqueueCount)
    }

    @Test
    fun keyDeletionFailureStillRemovesCiphertextAndRestartCannotReloadCredential() {
        val keys = InMemoryCredentialKeyAccess(failDelete = true)
        val store = testStore(
            keys,
            clearPreferenceRecords = { preferences.edit().clear().commit() },
        )
        store.update(TEST_BASE_URL, TEST_TOKEN)
        assertTrue(store.snapshot().hasBearerToken)

        assertThrows(SecureCredentialException::class.java) { store.clear() }

        assertEquals(1, keys.deleteAttempts)
        assertTrue(keys.hasKey(TEST_KEY_ALIAS))
        assertTrue(preferences.all.isEmpty())

        val restarted = testStore(
            keys,
            clearPreferenceRecords = { preferences.edit().clear().commit() },
        )
        assertFalse(restarted.snapshot().hasBearerToken)
        assertEquals(null, restarted.authenticatedConfiguration())
        val backend = RecordingSyncBackend()
        SuggestionSyncSchedulingCoordinator(restarted, backend).onAppStart()
        assertTrue(backend.cancelled)
        assertEquals(0, backend.enqueueCount)
    }

    @Test
    fun changingUrlRequiresAndAtomicallyBindsAReplacementToken() {
        val store = testStore(
            InMemoryCredentialKeyAccess(),
            clearPreferenceRecords = { preferences.edit().clear().commit() },
        )
        store.update(TEST_BASE_URL, TEST_TOKEN)
        val first = store.snapshot()

        store.update("$TEST_BASE_URL/", null)
        assertEquals(first.baseUrl, store.snapshot().baseUrl)
        assertEquals(TEST_TOKEN, store.authenticatedConfiguration()?.bearerToken)

        assertThrows(InvalidApiConfigurationException::class.java) {
            store.update("https://other.example.test/", null)
        }
        assertEquals(first.baseUrl, store.snapshot().baseUrl)
        assertEquals(TEST_TOKEN, store.authenticatedConfiguration()?.bearerToken)

        store.update("https://other.example.test/", "replacement-secret")
        val replaced = store.snapshot()
        assertEquals("https://other.example.test/", replaced.baseUrl)
        assertTrue(replaced.configurationId != first.configurationId)
        assertEquals("replacement-secret", store.authenticatedConfiguration()?.bearerToken)
    }

    private fun testStore(
        keys: ApiCredentialKeyAccess,
        clearPreferenceRecords: () -> Boolean,
    ) = KeystoreApiCredentialStore.createForTest(
        context = context,
        configuredBaseUrl = "",
        preferencesName = TEST_PREFERENCES,
        keyAlias = TEST_KEY_ALIAS,
        keyAccess = keys,
        clearPreferenceRecords = clearPreferenceRecords,
    )

    private companion object {
        const val TEST_PREFERENCES = "api-credential-clear-test"
        const val TEST_KEY_ALIAS = "com.greengolddog.dayweave.test-clear-key"
        const val TEST_BASE_URL = "https://api.example.test/dayweave"
        const val TEST_TOKEN = "test-clear-secret"
    }
}

private class InMemoryCredentialKeyAccess(
    private val failDelete: Boolean = false,
) : ApiCredentialKeyAccess {
    private val keys = mutableMapOf<String, SecretKey>()
    var deleteAttempts = 0
        private set

    override fun existing(alias: String): SecretKey? = keys[alias]

    override fun create(alias: String): SecretKey = KeyGenerator.getInstance("AES")
        .apply { init(256, SecureRandom()) }
        .generateKey()
        .also { keys[alias] = it }

    override fun delete(alias: String) {
        deleteAttempts += 1
        if (failDelete) throw IllegalStateException("synthetic key deletion failure")
        keys.remove(alias)
    }

    fun hasKey(alias: String): Boolean = alias in keys
}

private class RecordingSyncBackend : SuggestionSyncWorkBackend {
    var cancelled = false
        private set
    var enqueueCount = 0
        private set

    override fun ensurePeriodic(policy: SuggestionSyncWorkPolicy) {
        enqueueCount += 1
    }

    override fun enqueueStartupRefresh(policy: SuggestionSyncWorkPolicy) {
        enqueueCount += 1
    }

    override fun replaceConfigurationRefresh(policy: SuggestionSyncWorkPolicy) {
        enqueueCount += 1
    }

    override fun cancelAll() {
        cancelled = true
    }
}
