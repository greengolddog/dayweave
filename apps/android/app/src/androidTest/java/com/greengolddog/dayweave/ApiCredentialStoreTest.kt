package com.greengolddog.dayweave

import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.network.KeystoreApiCredentialStore
import java.security.KeyStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ApiCredentialStoreTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Before
    fun setUp() = cleanUp()

    @After
    fun cleanUp() {
        context.getSharedPreferences(TEST_PREFERENCES, Context.MODE_PRIVATE)
            .edit()
            .clear()
            .commit()
        KeyStore.getInstance(ANDROID_KEY_STORE).apply {
            load(null)
            if (containsAlias(TEST_KEY_ALIAS)) deleteEntry(TEST_KEY_ALIAS)
        }
    }

    @Test
    fun bearerTokenIsKeystoreWrappedAndCanBeForgotten() {
        val store = KeystoreApiCredentialStore(
            context = context,
            configuredBaseUrl = "",
            preferencesName = TEST_PREFERENCES,
            keyAlias = TEST_KEY_ALIAS,
        )

        store.update("https://api.example.com/dayweave", TEST_TOKEN)

        val rawValues = context.getSharedPreferences(TEST_PREFERENCES, Context.MODE_PRIVATE).all
        assertTrue(rawValues.isNotEmpty())
        assertFalse(rawValues.values.any { it.toString().contains(TEST_TOKEN) })
        assertTrue(
            KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
                .containsAlias(TEST_KEY_ALIAS),
        )
        val authenticated = requireNotNull(store.authenticatedConfiguration())
        assertEquals("https://api.example.com/dayweave/", authenticated.baseUrl.toString())
        assertEquals(TEST_TOKEN, authenticated.bearerToken)
        assertFalse(authenticated.toString().contains(TEST_TOKEN))

        store.clear()

        assertFalse(store.snapshot().hasBearerToken)
        assertFalse(
            KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }
                .containsAlias(TEST_KEY_ALIAS),
        )
    }

    private companion object {
        const val TEST_PREFERENCES = "api-credential-store-test"
        const val TEST_KEY_ALIAS = "com.greengolddog.dayweave.test-api-token-key"
        const val TEST_TOKEN = "instrumentation-secret-token"
        const val ANDROID_KEY_STORE = "AndroidKeyStore"
    }
}
