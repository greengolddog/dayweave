package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeviceAuthApiException
import com.greengolddog.dayweave.network.DeviceSessionContract
import com.greengolddog.dayweave.network.DeviceSessionDeleteOutcomeAmbiguousException
import com.greengolddog.dayweave.network.DeviceSessionListResponse
import com.greengolddog.dayweave.network.DeviceSessionsTransport
import com.greengolddog.dayweave.network.syntheticSession
import java.io.IOException
import java.time.Instant
import java.util.ArrayDeque
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceSessionManagerTest {
    private val now = Instant.parse("2026-09-05T09:00:00Z")

    @Test
    fun refreshMarksOnlyExactCurrentIdAndSortsItFirst() = runBlocking {
        val credentials = FakeCredentials(CURRENT_ID)
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(remoteSession(REMOTE_ID, lastSeenOffset = 100), currentSession())
        }
        val manager = manager(credentials, transport)

        manager.refresh()

        assertEquals(DeviceSessionsPhase.READY, manager.state.value.phase)
        assertEquals(listOf(CURRENT_ID, REMOTE_ID), manager.state.value.sessions.map { it.id })
        assertEquals(listOf(true, false), manager.state.value.sessions.map { it.isCurrent })
        assertEquals(CURRENT_ID, manager.state.value.configurationId)
        assertEquals(now.plusSeconds(1), manager.state.value.lastRefreshedAt)
    }

    @Test
    fun invalidRefreshBecomesStaleAndOfflineRefreshRetainsOnlyMemoryRows() = runBlocking {
        val credentials = FakeCredentials(CURRENT_ID)
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession(), remoteSession())
            enqueueListFailure(DeviceAuthApiException.InvalidResponse())
            enqueueListFailure(IOException("offline"))
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        val acceptedRows = manager.state.value.sessions

        manager.refresh()
        assertEquals(DeviceSessionsPhase.STALE, manager.state.value.phase)
        assertEquals(acceptedRows, manager.state.value.sessions)
        assertFalse(manager.state.value.canRevokeRemoteSessions)

        manager.refresh()
        assertEquals(DeviceSessionsPhase.OFFLINE, manager.state.value.phase)
        assertEquals(acceptedRows, manager.state.value.sessions)
        assertFalse(manager.state.value.canRevokeRemoteSessions)
    }

    @Test
    fun oldConfirmationIsRejectedAfterAnyAuthoritativeRefresh() = runBlocking {
        val credentials = FakeCredentials(CURRENT_ID)
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession(), remoteSession(revision = 1))
            enqueueList(currentSession(), remoteSession(revision = 1))
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        val oldConfirmation = requireNotNull(manager.revocationConfirmation(REMOTE_ID))

        manager.refresh()

        assertFalse(manager.revokeRemote(oldConfirmation))
        assertEquals(0, transport.revokeCalls.size)
        assertEquals(1, manager.state.value.sessions.single { it.id == REMOTE_ID }.revision)
    }

    @Test
    fun currentSessionCannotProduceRemoteRevocationConfirmation() = runBlocking {
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession(), remoteSession())
        }
        val manager = manager(FakeCredentials(CURRENT_ID), transport)
        manager.refresh()

        assertNull(manager.revocationConfirmation(CURRENT_ID))
        assertTrue(manager.revocationConfirmation(REMOTE_ID) != null)
    }

    @Test
    fun refreshRejectsAnAuthoritativeListThatOmitsCurrentSession() = runBlocking {
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(remoteSession())
        }
        val manager = manager(FakeCredentials(CURRENT_ID), transport)

        manager.refresh()

        assertEquals(DeviceSessionsPhase.ERROR, manager.state.value.phase)
        assertTrue(manager.state.value.sessions.isEmpty())
        assertFalse(manager.state.value.canRevokeRemoteSessions)
    }

    @Test
    fun refreshRequiresCurrentRowToMatchDurableAndroidDeviceIdentity() = runBlocking {
        listOf(
            currentSession().copy(clientKind = "macos"),
            currentSession().copy(clientInstanceId = REMOTE_INSTANCE_ID),
        ).forEach { mismatchedCurrent ->
            val transport = FakeDeviceSessionsTransport().apply {
                enqueueList(mismatchedCurrent, remoteSession())
            }
            val manager = manager(FakeCredentials(CURRENT_ID), transport)

            manager.refresh()

            assertEquals(DeviceSessionsPhase.ERROR, manager.state.value.phase)
            assertTrue(manager.state.value.sessions.isEmpty())
            assertFalse(manager.state.value.canRevokeRemoteSessions)
        }
    }

    @Test
    fun missingDurableClientIdentityRequiresReconnectionWithoutDispatch() = runBlocking {
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession())
        }
        val manager = manager(
            FakeCredentials(CURRENT_ID).apply { clientInstanceId = null },
            transport,
        )

        manager.refresh()

        assertEquals(DeviceSessionsPhase.AUTH_REQUIRED, manager.state.value.phase)
        assertEquals(0, transport.listCalls)
        assertTrue(manager.state.value.message.contains("session identity"))
    }

    @Test
    fun revokePermissionComesOnlyFromExactCurrentRow() = runBlocking {
        val readOnlyCurrent = currentSession().copy(
            scopes = currentSession().scopes.filterNot { it == "auth_sessions_write" },
        )
        val writableRemote = remoteSession()
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(readOnlyCurrent, writableRemote)
        }
        val manager = manager(FakeCredentials(CURRENT_ID), transport)

        manager.refresh()

        assertEquals(DeviceSessionsPhase.READY, manager.state.value.phase)
        assertFalse(manager.state.value.currentSessionCanRevoke)
        assertFalse(manager.state.value.canRevokeRemoteSessions)
        assertTrue(manager.state.value.message.contains("Read-only"))
        assertNull(manager.revocationConfirmation(REMOTE_ID))
    }

    @Test
    fun remoteRevocationsAreSerializedAndSecondStaleCapabilityIsRefused() = runBlocking {
        val secondRemoteId = "55555555-5555-4555-8555-555555555555"
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession(), remoteSession(), remoteSession(secondRemoteId))
            enqueueList(currentSession(), remoteSession(secondRemoteId))
            revokeStarted = CompletableDeferred()
            revokeGate = CompletableDeferred()
        }
        val manager = manager(FakeCredentials(CURRENT_ID), transport)
        manager.refresh()
        val first = requireNotNull(manager.revocationConfirmation(REMOTE_ID))
        val second = requireNotNull(manager.revocationConfirmation(secondRemoteId))

        val firstResult = async { manager.revokeRemote(first) }
        withTimeout(2_000) { requireNotNull(transport.revokeStarted).await() }
        val secondResult = async { manager.revokeRemote(second) }
        assertEquals(1, transport.revokeCalls.size)
        assertEquals(1, transport.maximumConcurrentRevokes)
        requireNotNull(transport.revokeGate).complete(Unit)

        assertEquals(listOf(true, false), awaitAll(firstResult, secondResult))
        assertEquals(1, transport.maximumConcurrentRevokes)
        assertEquals(listOf(REMOTE_ID), transport.revokeCalls)
    }

    @Test
    fun confirmedAmbiguousAndMissingDeletesAlwaysReconcileWithAuthoritativeList() = runBlocking {
        val deleteOutcomes = listOf<Exception?>(
            null,
            IOException("connection dropped after write"),
            DeviceSessionDeleteOutcomeAmbiguousException(),
            DeviceAuthApiException.Unavailable(),
            DeviceAuthApiException.Http(404),
            DeviceAuthApiException.Http(408),
            DeviceAuthApiException.Http(425),
            DeviceAuthApiException.Http(429),
            DeviceAuthApiException.Http(500),
        )

        deleteOutcomes.forEach { outcome ->
            val transport = FakeDeviceSessionsTransport().apply {
                enqueueList(currentSession(), remoteSession())
                enqueueList(currentSession())
                revokeFailure = outcome
            }
            val manager = manager(FakeCredentials(CURRENT_ID), transport)
            manager.refresh()
            val confirmation = requireNotNull(manager.revocationConfirmation(REMOTE_ID))

            assertTrue(manager.revokeRemote(confirmation))
            assertEquals(listOf(REMOTE_ID), transport.revokeCalls)
            assertEquals(2, transport.listCalls)
            assertEquals(DeviceSessionsPhase.READY, manager.state.value.phase)
            assertEquals(listOf(CURRENT_ID), manager.state.value.sessions.map { it.id })
        }
    }

    @Test
    fun deterministicDeleteFailuresDoNotRelist() = runBlocking {
        val failures = listOf<Exception>(
            DeviceAuthApiException.Authentication(),
            DeviceAuthApiException.Forbidden(),
            DeviceAuthApiException.Conflict(),
            DeviceAuthApiException.Validation(),
            DeviceAuthApiException.InvalidResponse(),
            DeviceAuthApiException.Http(409),
        )

        failures.forEach { failure ->
            val transport = FakeDeviceSessionsTransport().apply {
                enqueueList(currentSession(), remoteSession())
                revokeFailure = failure
            }
            val manager = manager(FakeCredentials(CURRENT_ID), transport)
            manager.refresh()

            assertFalse(
                manager.revokeRemote(requireNotNull(manager.revocationConfirmation(REMOTE_ID))),
            )
            assertEquals(1, transport.listCalls)
            assertEquals(listOf(REMOTE_ID), transport.revokeCalls)
        }
    }

    @Test
    fun ambiguousDeleteThatCannotRelistKeepsTargetAsStaleAndUnconfirmed() = runBlocking {
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession(), remoteSession())
            enqueueListFailure(IOException("offline during reconciliation"))
            revokeFailure = IOException("connection dropped after write")
        }
        val manager = manager(FakeCredentials(CURRENT_ID), transport)
        manager.refresh()
        val confirmation = requireNotNull(manager.revocationConfirmation(REMOTE_ID))

        assertFalse(manager.revokeRemote(confirmation))
        assertTrue(manager.state.value.sessions.any { it.id == REMOTE_ID })
        assertEquals(DeviceSessionsPhase.OFFLINE, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("unconfirmed"))
    }

    @Test
    fun privacyQuarantineCancelsInflightRefreshClearsLabelsAndRejectsConfirmation() = runBlocking {
        val transport = FakeDeviceSessionsTransport().apply {
            enqueueList(currentSession(), remoteSession())
        }
        val manager = manager(FakeCredentials(CURRENT_ID), transport)
        manager.refresh()
        val confirmation = requireNotNull(manager.revocationConfirmation(REMOTE_ID))

        transport.enqueueList(currentSession(), remoteSession())
        transport.listStarted = CompletableDeferred()
        transport.listGate = CompletableDeferred()
        val refresh = async { manager.refresh() }
        withTimeout(2_000) { requireNotNull(transport.listStarted).await() }

        manager.quarantineBindingState()
        withTimeout(2_000) { refresh.join() }

        assertEquals(DeviceSessionsPhase.NOT_CONFIGURED, manager.state.value.phase)
        assertTrue(manager.state.value.sessions.isEmpty())
        assertNull(manager.state.value.configurationId)
        assertFalse(manager.revokeRemote(confirmation))
        assertTrue(transport.revokeCalls.isEmpty())
    }

    private fun manager(
        credentials: FakeCredentials,
        transport: FakeDeviceSessionsTransport,
    ): DeviceSessionManager {
        var clockCalls = 0L
        return DeviceSessionManager(
            credentialStore = credentials,
            transport = transport,
            now = { now.plusSeconds(++clockCalls) },
        )
    }

    private fun currentSession(): DeviceSessionContract = syntheticSession(
        now = now,
        id = CURRENT_ID,
        clientInstanceId = CURRENT_INSTANCE_ID,
    )

    private fun remoteSession(
        id: String = REMOTE_ID,
        revision: Long = 1,
        lastSeenOffset: Long = 0,
    ): DeviceSessionContract = syntheticSession(
        now = now,
        id = id,
        clientInstanceId = if (id == REMOTE_ID) REMOTE_INSTANCE_ID else SECOND_REMOTE_INSTANCE_ID,
        revision = revision,
        lastSeenAt = now.plusSeconds(lastSeenOffset),
    )

    private companion object {
        const val CURRENT_ID = "11111111-1111-4111-8111-111111111111"
        const val CURRENT_INSTANCE_ID = "22222222-2222-4222-8222-222222222222"
        const val REMOTE_ID = "33333333-3333-4333-8333-333333333333"
        const val REMOTE_INSTANCE_ID = "44444444-4444-4444-8444-444444444444"
        const val SECOND_REMOTE_INSTANCE_ID = "66666666-6666-4666-8666-666666666666"
    }
}

private class FakeCredentials(
    var configurationId: String,
) : ApiCredentialStore {
    var hasBearerToken = true
    var baseUrl = "https://api.example.test/tenant/"
    var clientInstanceId: String? = "22222222-2222-4222-8222-222222222222"

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = baseUrl,
        hasBearerToken = hasBearerToken,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
        clientInstanceId = clientInstanceId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
        if (hasBearerToken) {
            AuthenticatedApiConfiguration.createBound(baseUrl, "test-secret", configurationId)
        } else {
            null
        }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeDeviceSessionsTransport : DeviceSessionsTransport {
    private val listed = ArrayDeque<Result<DeviceSessionListResponse>>()
    var revokeFailure: Exception? = null
    var revokeStarted: CompletableDeferred<Unit>? = null
    var revokeGate: CompletableDeferred<Unit>? = null
    var listStarted: CompletableDeferred<Unit>? = null
    var listGate: CompletableDeferred<Unit>? = null
    var listCalls = 0
    val revokeCalls = mutableListOf<String>()
    var concurrentRevokes = 0
    var maximumConcurrentRevokes = 0

    fun enqueueList(vararg sessions: DeviceSessionContract) {
        listed.addLast(Result.success(DeviceSessionListResponse(sessions.toList())))
    }

    fun enqueueListFailure(error: Exception) {
        listed.addLast(Result.failure(error))
    }

    override suspend fun listSessions(
        configuration: AuthenticatedApiConfiguration,
    ): DeviceSessionListResponse {
        listCalls += 1
        listStarted?.complete(Unit)
        listGate?.await()
        return listed.removeFirst().getOrThrow()
    }

    override suspend fun revokeSession(
        configuration: AuthenticatedApiConfiguration,
        sessionId: String,
    ) {
        revokeCalls += sessionId
        concurrentRevokes += 1
        maximumConcurrentRevokes = maxOf(maximumConcurrentRevokes, concurrentRevokes)
        try {
            revokeStarted?.complete(Unit)
            revokeGate?.await()
            revokeFailure?.let { throw it }
        } finally {
            concurrentRevokes -= 1
        }
    }
}
