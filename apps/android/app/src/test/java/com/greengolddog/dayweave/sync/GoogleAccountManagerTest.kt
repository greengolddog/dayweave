package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleAccountsApiException
import com.greengolddog.dayweave.network.GoogleAccountsTransport
import com.greengolddog.dayweave.network.RemoteGoogleAccount
import com.greengolddog.dayweave.network.RemoteGoogleAccounts
import com.greengolddog.dayweave.network.RemoteGoogleAuthorization
import com.greengolddog.dayweave.network.RemoteGoogleCleanupStatus
import com.greengolddog.dayweave.network.StartGoogleAuthorizationRequest
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleAccountManagerTest {
    @Test
    fun refreshMapsCapabilitiesAndTruthfulRecoveryState() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)

        manager.refresh()
        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        val mapped = manager.state.value.accounts.single()
        assertTrue(mapped.hasCalendar)
        assertTrue(mapped.hasTasks)
        assertTrue(mapped.isDefault)

        transport.accountsResult = accounts(account()).copy(
            cleanup = cleanup().copy(
                operatorRecoveryRequired = true,
                uncertainAuthorizations = 1,
            ),
        )
        manager.refresh()
        assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("owner attention"))
    }

    @Test
    fun connectNewExposesOnlyAnExactTrustedGoogleAuthorizationUrl() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            authorizationResult = RemoteGoogleAuthorization(
                authorizationUrl =
                    "https://accounts.google.com/o/oauth2/v2/auth?state=opaque&code_challenge=proof",
                expiresAt = NOW.plusSeconds(600).toString(),
            )
        }
        val manager = manager(credentials, transport)

        manager.connectNew()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertNotNull(manager.state.value.authorization)
        assertFalse(manager.state.value.toString().contains("state=opaque"))
        val request = requireNotNull(transport.authorizationRequests.single())
        assertEquals(setOf("calendar", "tasks"), request.services)
        assertTrue(request.connectNew)
        assertTrue(request.makeDefault)

        transport.authorizationResult = transport.authorizationResult.copy(
            authorizationUrl = "https://accounts.google.com.evil.example/o/oauth2/v2/auth?state=x",
        )
        manager.connectNew()
        assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
        assertNull(manager.state.value.authorization)
    }

    @Test
    fun responseFromReplacedApiCredentialsCannotRebindGoogleState() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            accountsHook = { credentials.configurationId = "configuration-b" }
        }
        val manager = manager(credentials, transport)

        manager.refresh()

        assertTrue(manager.state.value.accounts.isEmpty())
        assertEquals(GoogleAccountPhase.DISCONNECTED, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun optimisticConflictRefreshesAuthoritativeAccountInsteadOfGuessing() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            pauseError = GoogleAccountsApiException.Conflict()
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.accountsResult = accounts(account().copy(status = "paused", syncEnabled = false, revision = 8))

        manager.setPaused(ACCOUNT_ID, paused = true)

        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        assertEquals("paused", manager.state.value.accounts.single().status)
        assertFalse(manager.state.value.accounts.single().syncEnabled)
        assertEquals(2, transport.accountsCalls)
    }

    @Test
    fun rejectedPlannerCredentialOffersTheConfigurationFlow() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsError = GoogleAccountsApiException.Authentication()
        }
        val manager = manager(credentials, transport)

        manager.refresh()

        assertEquals(GoogleAccountPhase.AUTH_REQUIRED, manager.state.value.phase)
        assertTrue(manager.state.value.requiresPlannerApiConfiguration)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun credentialReplacementDropsCachedAccountsAndAuthorizationBeforeRefresh() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            authorizationResult = RemoteGoogleAuthorization(
                "https://accounts.google.com/o/oauth2/v2/auth?state=credential-a",
                NOW.plusSeconds(600).toString(),
            )
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()
        val oldUrl = requireNotNull(manager.state.value.authorization).url
        var openedUrl: String? = null
        assertTrue(manager.useAuthorizationUrlIfCurrent(oldUrl) { openedUrl = it })
        assertEquals(oldUrl, openedUrl)

        credentials.configurationId = "configuration-b"
        transport.accountsResult = accounts()
        manager.refresh()

        assertEquals("configuration-b", manager.state.value.configurationId)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertFalse(manager.useAuthorizationUrlIfCurrent(oldUrl) { error("must not open") })
    }

    @Test
    fun serializedCredentialReplacementClearsCachedIdentityBeforeUnlocking() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()

        manager.withConfigurationChangeLock {
            credentials.configurationId = "configuration-b"
        }

        assertEquals("configuration-b", manager.state.value.configurationId)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        val requestsBeforeRestart = transport.authorizationRequests.size
        manager.restartAuthorization()
        assertEquals(requestsBeforeRestart, transport.authorizationRequests.size)
    }

    @Test
    fun staleAccountActionNeverLeavesUnderReplacementCredential() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        credentials.configurationId = "configuration-b"

        manager.reauthorize(ACCOUNT_ID)

        assertTrue(transport.authorizationRequests.isEmpty())
        assertTrue(manager.state.value.accounts.isEmpty())
        assertEquals("configuration-b", manager.state.value.configurationId)
    }

    @Test
    fun conflictRefreshCannotApplyResponseAfterCredentialReplacement() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            pauseError = GoogleAccountsApiException.Conflict()
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.accountsHook = { credentials.configurationId = "configuration-b" }

        manager.setPaused(ACCOUNT_ID, paused = true)

        assertTrue(manager.state.value.accounts.isEmpty())
        assertEquals("configuration-b", manager.state.value.configurationId)
        assertEquals(GoogleAccountPhase.DISCONNECTED, manager.state.value.phase)
    }

    @Test
    fun configurationReplacingBetweenSnapshotAndDecryptionCannotSendARequest() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        credentials.configurationHook = { credentials.configurationId = "configuration-b" }

        manager.refresh()

        assertEquals(0, transport.accountsCalls)
        assertEquals("configuration-b", manager.state.value.configurationId)
        assertTrue(manager.state.value.accounts.isEmpty())
    }

    @Test
    fun successfulConnectNewClearsOneUseAuthorizationFromAuthoritativeState() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)

        transport.accountsResult = accounts(
            account(),
            account(id = SECOND_ACCOUNT_ID, label = "Second", isDefault = false),
        )
        manager.refresh()

        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        assertNull(manager.state.value.authorization)
        assertEquals(2, manager.state.value.accounts.size)
    }

    @Test
    fun successfulReauthorizationClearsPendingUrlAndFailedFlowCanStartOver() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(status = "reauthorization_required"))
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.reauthorize(ACCOUNT_ID)
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)

        // An unchanged authoritative account means denial/failure is not guessed as success.
        manager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        manager.restartAuthorization()
        assertEquals(2, transport.authorizationRequests.size)

        transport.accountsResult = accounts(account(status = "active", revision = 8))
        manager.refresh()
        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        assertNull(manager.state.value.authorization)
    }

    @Test
    fun failedDisconnectReconcilesAndNeverClaimsAccessWasRevoked() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.disconnectError = GoogleAccountsApiException.Unavailable()
        transport.accountsResult = accounts(
            account(status = "revocation_failed", revision = 8),
        )

        manager.disconnect(ACCOUNT_ID)

        assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("not confirmed revoked"))
        assertEquals("revocation_failed", manager.state.value.accounts.single().status)
        assertEquals(2, transport.accountsCalls)
    }

    private fun manager(
        credentials: FakeGoogleCredentials,
        transport: FakeGoogleAccountsTransport,
    ) = GoogleAccountManager(
        credentialStore = credentials,
        transport = transport,
        now = { NOW },
        newUuid = { UUID.fromString(IDEMPOTENCY_KEY) },
    )

    private fun accounts(vararg accounts: RemoteGoogleAccount) = RemoteGoogleAccounts(
        accounts = accounts.toList(),
        cleanup = cleanup(),
    )

    private fun cleanup() = RemoteGoogleCleanupStatus(
        held = 0,
        pending = 0,
        retrying = 0,
        exhausted = 0,
        volatileGuardians = 0,
        durabilityDegraded = false,
        revocationFenced = false,
        operatorRecoveryRequired = false,
        uncertainAuthorizations = 0,
        legacyRecoveryRequired = 0,
        nextAttemptAt = null,
        lastFailureAt = null,
    )

    private fun account(
        id: String = ACCOUNT_ID,
        label: String = "Owner",
        status: String = "active",
        isDefault: Boolean = true,
        revision: Long = 7,
    ) = RemoteGoogleAccount(
        id = id,
        externalAccountId = "google-owner",
        displayLabel = label,
        status = status,
        syncEnabled = status == "active",
        isDefault = isDefault,
        grantedScopes = setOf(
            "openid",
            "email",
            "https://www.googleapis.com/auth/calendar",
            "https://www.googleapis.com/auth/tasks",
        ),
        tokenExpiresAt = NOW.plusSeconds(3_600).toString(),
        revision = revision,
        createdAt = NOW.minusSeconds(3_600).toString(),
        updatedAt = NOW.toString(),
    )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-01T07:00:00Z")
        const val ACCOUNT_ID = "11111111-1111-4111-8111-111111111111"
        const val SECOND_ACCOUNT_ID = "33333333-3333-4333-8333-333333333333"
        const val IDEMPOTENCY_KEY = "22222222-2222-4222-8222-222222222222"
    }
}

private class FakeGoogleCredentials : ApiCredentialStore {
    var configurationId = "configuration-a"
    var configurationHook: (() -> Unit)? = null

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration {
        configurationHook?.also { configurationHook = null }?.invoke()
        return AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "test-secret",
            configurationId,
        )
    }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeGoogleAccountsTransport : GoogleAccountsTransport {
    var accountsResult = RemoteGoogleAccounts(emptyList(), emptyCleanup())
    var authorizationResult = RemoteGoogleAuthorization(
        "https://accounts.google.com/o/oauth2/v2/auth?state=opaque",
        "2026-09-01T07:10:00Z",
    )
    var pauseError: Exception? = null
    var disconnectError: Exception? = null
    var accountsError: Exception? = null
    var accountsHook: (() -> Unit)? = null
    var accountsCalls = 0
    val authorizationRequests = mutableListOf<StartGoogleAuthorizationRequest>()

    override suspend fun accounts(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteGoogleAccounts {
        accountsCalls += 1
        accountsHook?.invoke()
        accountsError?.let { throw it }
        return accountsResult
    }

    override suspend fun startAuthorization(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: StartGoogleAuthorizationRequest,
    ): RemoteGoogleAuthorization {
        authorizationRequests += request
        return authorizationResult
    }

    override suspend fun setPaused(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        paused: Boolean,
        idempotencyKey: String,
    ): RemoteGoogleAccount {
        pauseError?.let { throw it }
        return accountsResult.accounts.single()
    }

    override suspend fun disconnect(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        idempotencyKey: String,
    ): RemoteGoogleAccount {
        disconnectError?.let { throw it }
        return accountsResult.accounts.single()
    }

    companion object {
        private fun emptyCleanup() = RemoteGoogleCleanupStatus(
            held = 0,
            pending = 0,
            retrying = 0,
            exhausted = 0,
            volatileGuardians = 0,
            durabilityDegraded = false,
            revocationFenced = false,
            operatorRecoveryRequired = false,
            uncertainAuthorizations = 0,
            legacyRecoveryRequired = 0,
            nextAttemptAt = null,
            lastFailureAt = null,
        )
    }
}
