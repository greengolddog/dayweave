package com.greengolddog.dayweave.network

import java.io.IOException
import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceAuthCoordinatorTest {
    private val now = Instant.parse("2026-08-29T12:00:00Z")

    @Test
    fun bootstrapCreationCrashRetriesExactJournalWithoutRetargeting() = runBlocking {
        val store = unconfiguredStore()
        val firstTransport = RecordingDeviceAuthTransport().apply {
            createHandler = { throw IOException("synthetic creation response loss") }
        }
        val first = coordinator(store, firstTransport)

        assertEquals(
            DeviceAuthActionResult.PENDING_RETRY,
            first.upgradeWithBootstrap(SYNTHETIC_BASE_URL, "synthetic-bootstrap"),
        )
        val pending = store.envelope.state as StoredDeviceAuthState.EnrollmentCreationPending
        val exactRequest = pending.request
        assertEquals(pending.enrollmentId, firstTransport.createCalls.single().request.id)
        assertEquals(pending.enrollmentToken.value, firstTransport.createCalls.single().request.enrollmentToken)
        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            first.upgradeWithBootstrap("https://other.example.test/", "different-bootstrap"),
        )
        assertEquals(pending, store.envelope.state)

        val restartedTransport = RecordingDeviceAuthTransport().apply {
            createHandler = { call ->
                DeviceEnrollmentIssuedResponse(
                    call.request.id,
                    call.request.enrollmentToken,
                    now.plusSeconds(600).toString(),
                    DEVICE_AUTH_CONTRACT_VERSION,
                    replayed = true,
                )
            }
            consumeHandler = { call ->
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = call.request.sessionId,
                        clientInstanceId = pending.clientInstanceId,
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, restartedTransport).recoverPendingOrUpgradeLegacy(),
        )
        assertEquals(exactRequest, restartedTransport.createCalls.single().journal)
        val active = store.envelope.state as StoredDeviceAuthState.Active
        assertEquals(pending.clientInstanceId, active.clientInstanceId)
        assertEquals(
            3,
            listOf(
                pending.enrollmentToken.value.substringAfter("dw_en1_"),
                active.accessToken.value.substringAfter("dw_ac1_"),
                active.refreshToken.value.substringAfter("dw_rf1_"),
            ).distinct().size,
        )
    }

    @Test
    fun cancelledBootstrapWaiterCannotCancelSharedCreationLeader() = runBlocking {
        val store = unconfiguredStore()
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val transport = RecordingDeviceAuthTransport().apply {
            createHandler = { call ->
                started.complete(Unit)
                release.await()
                DeviceEnrollmentIssuedResponse(
                    call.request.id,
                    call.request.enrollmentToken,
                    now.plusSeconds(600).toString(),
                    DEVICE_AUTH_CONTRACT_VERSION,
                    replayed = false,
                )
            }
            consumeHandler = { call ->
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = call.request.sessionId,
                        clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                    ),
                    replayed = false,
                )
            }
        }
        val coordinator = coordinator(store, transport)

        val cancelledWaiter = async {
            coordinator.upgradeWithBootstrap(SYNTHETIC_BASE_URL, "synthetic-bootstrap")
        }
        withTimeout(3_000) { started.await() }
        val survivingWaiter = async { coordinator.recoverPendingOrUpgradeLegacy() }
        yield()
        cancelledWaiter.cancelAndJoin()
        release.complete(Unit)

        assertEquals(DeviceAuthActionResult.SUCCESS, withTimeout(3_000) { survivingWaiter.await() })
        assertEquals(1, transport.createCalls.size)
        assertEquals(1, transport.consumeCalls.size)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Active)
    }

    @Test
    fun directEnrollmentJournalsBeforeSendAndRestartRetriesExactTuple() = runBlocking {
        val store = unconfiguredStore()
        val firstTransport = RecordingDeviceAuthTransport().apply {
            consumeHandler = { throw IOException("synthetic lost enrollment response") }
        }
        val first = coordinator(store, firstTransport)
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 1)

        assertEquals(
            DeviceAuthActionResult.PENDING_RETRY,
            first.consumeOneTimeEnrollmentCode(SYNTHETIC_BASE_URL, enrollment),
        )
        val pending = store.envelope.state as StoredDeviceAuthState.EnrollmentPending
        assertEquals(1, firstTransport.consumeCalls.size)
        assertEquals(pending.sessionId, firstTransport.consumeCalls.single().request.sessionId)
        assertEquals(pending.accessToken.value, firstTransport.consumeCalls.single().request.accessToken)
        assertEquals(pending.refreshToken.value, firstTransport.consumeCalls.single().request.refreshToken)

        val restartedTransport = RecordingDeviceAuthTransport().apply {
            consumeHandler = { call ->
                DeviceSessionMutationResponse(
                    session = syntheticSession(
                        now = now,
                        id = call.request.sessionId,
                        clientInstanceId = pending.clientInstanceId,
                    ),
                    replayed = true,
                )
            }
        }
        val fence = RecordingDeviceAuthFence()
        val restarted = coordinator(store, restartedTransport, fence = fence)

        assertEquals(DeviceAuthActionResult.SUCCESS, restarted.recoverPendingOrUpgradeLegacy())
        val replay = restartedTransport.consumeCalls.single()
        assertEquals(enrollment, replay.enrollmentToken)
        assertEquals(pending.sessionId, replay.request.sessionId)
        assertEquals(pending.accessToken.value, replay.request.accessToken)
        assertEquals(pending.refreshToken.value, replay.request.refreshToken)
        assertEquals(pending.sessionId, restarted.snapshot().configurationId)
        assertEquals(
            listOf(SYNTHETIC_BASE_URL, null, SYNTHETIC_BASE_URL, pending.sessionId),
            fence.calls.single(),
        )
    }

    @Test
    fun delayedExactEnrollmentReplaySurvivesAccessExpiry() = runBlocking {
        val preparedAt = now.minus(Duration.ofMinutes(20))
        val pending = StoredDeviceAuthState.EnrollmentPending(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
            sessionId = SYNTHETIC_SESSION_ID,
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            preparedAt = preparedAt.toString(),
            scopes = ANDROID_DEVICE_AUTH_SCOPES,
            capabilities = ANDROID_DEVICE_AUTH_CAPABILITIES,
            enrollmentToken = DeviceAuthSecret(
                syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 83),
            ),
            accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 84)),
            refreshToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 85)),
        )
        val store = FakeDeviceAuthEnvelopeStore(pending)
        val transport = RecordingDeviceAuthTransport().apply {
            consumeHandler = {
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = pending.sessionId,
                        clientInstanceId = pending.clientInstanceId,
                        createdAt = preparedAt,
                        issuedAt = preparedAt,
                        lastSeenAt = preparedAt,
                        accessExpiresAt = preparedAt.plus(DEVICE_AUTH_ACCESS_TTL),
                        refreshIdleExpiresAt = preparedAt.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                        absoluteExpiresAt = preparedAt.plus(DEVICE_AUTH_ABSOLUTE_TTL),
                    ),
                    replayed = true,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, transport).recoverPendingOrUpgradeLegacy(),
        )
        val recovered = store.envelope.state as StoredDeviceAuthState.Active
        assertTrue(recovered.session.accessExpiry.isBefore(now))
    }

    @Test
    fun legacyBootstrapUpgradeEndsWithOnlyDeviceCredentialAuthority() = runBlocking {
        val legacyBinding = "33333333-3333-4333-8333-333333333333"
        val bootstrap = "synthetic-bootstrap"
        val legacy = StoredDeviceAuthState.Legacy(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
            bindingId = legacyBinding,
            bootstrapToken = DeviceAuthSecret(bootstrap),
        )
        val store = FakeDeviceAuthEnvelopeStore(legacy)
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 86)
        val transport = RecordingDeviceAuthTransport().apply {
            createHandler = { call ->
                DeviceEnrollmentIssuedResponse(
                    call.request.id,
                    call.request.enrollmentToken,
                    now.plus(Duration.ofMinutes(10)).toString(),
                    DEVICE_AUTH_CONTRACT_VERSION,
                    replayed = false,
                )
            }
            consumeHandler = { call ->
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = call.request.sessionId,
                        clientInstanceId = legacy.clientInstanceId,
                    ),
                    replayed = false,
                )
            }
        }
        val fence = RecordingDeviceAuthFence()
        val coordinator = coordinator(store, transport, fence = fence)

        assertEquals(DeviceAuthActionResult.SUCCESS, coordinator.recoverPendingOrUpgradeLegacy())
        val active = store.envelope.state as StoredDeviceAuthState.Active
        assertEquals(bootstrap, transport.createCalls.single().bootstrapToken)
        assertTrue(active.accessToken.value.startsWith(DEVICE_ACCESS_TOKEN_PREFIX))
        assertFalse(active.toString().contains(bootstrap))
        assertEquals(
            listOf(SYNTHETIC_BASE_URL, legacyBinding, SYNTHETIC_BASE_URL, active.session.id),
            fence.calls.single(),
        )
    }

    @Test
    fun legacyBootstrapIsNeverOrdinaryApiAuthority() {
        val bootstrap = "synthetic-bootstrap"
        val legacy = StoredDeviceAuthState.Legacy(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
            bindingId = "33333333-3333-4333-8333-333333333333",
            bootstrapToken = DeviceAuthSecret(bootstrap),
        )
        val coordinator = coordinator(
            FakeDeviceAuthEnvelopeStore(legacy),
            RecordingDeviceAuthTransport(),
        )

        assertFalse(coordinator.snapshot().hasBearerToken)
        assertNull(coordinator.authenticatedConfiguration())
        assertFalse(coordinator.toString().contains(bootstrap))
    }

    @Test
    fun enrollmentGenerationCollisionFailsBeforeJournalOrNetwork() = runBlocking {
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 7)
        val generator = QueueDeviceCredentialGenerator().apply {
            repeat(16) { enqueueToken(enrollment) }
        }
        val store = unconfiguredStore()
        val transport = RecordingDeviceAuthTransport()

        val result = coordinator(store, transport, generator = generator)
            .consumeOneTimeEnrollmentCode(SYNTHETIC_BASE_URL, enrollment)

        assertEquals(DeviceAuthActionResult.STORAGE_FAILURE, result)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Unconfigured)
        assertTrue(transport.consumeCalls.isEmpty())
    }

    @Test
    fun staleCasDoesNotConsumeOneTimeCode() = runBlocking {
        val store = unconfiguredStore().apply { failNextCompareAndSet = true }
        val transport = RecordingDeviceAuthTransport()

        val result = coordinator(store, transport).consumeOneTimeEnrollmentCode(
            SYNTHETIC_BASE_URL,
            syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 8),
        )

        assertEquals(DeviceAuthActionResult.STALE_STATE, result)
        assertTrue(transport.consumeCalls.isEmpty())
    }

    @Test
    fun mismatchedDirectCodeClientBindingIsQuarantined() = runBlocking {
        val store = unconfiguredStore()
        val transport = RecordingDeviceAuthTransport().apply {
            consumeHandler = { call ->
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = call.request.sessionId,
                        clientInstanceId = "33333333-3333-4333-8333-333333333333",
                    ),
                    replayed = false,
                )
            }
        }

        val result = coordinator(store, transport).consumeOneTimeEnrollmentCode(
            SYNTHETIC_BASE_URL,
            syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 9),
        )

        assertEquals(DeviceAuthActionResult.SERVER_REJECTED, result)
        val reauth = store.envelope.state as StoredDeviceAuthState.Reauth
        assertEquals(transport.consumeCalls.single().request.sessionId, reauth.previousSessionId)
        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            coordinator(store, transport).consumeOneTimeEnrollmentCode(
                "https://other.example.test/",
                syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 90),
            ),
        )
        assertEquals(1, transport.consumeCalls.size)
    }

    @Test
    fun enrollmentRevisionMustBeExactlyOne() = runBlocking {
        val store = unconfiguredStore()
        val transport = RecordingDeviceAuthTransport().apply {
            consumeHandler = { call ->
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = call.request.sessionId,
                        clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                        revision = 2,
                    ),
                    replayed = false,
                )
            }
        }

        val result = coordinator(store, transport).consumeOneTimeEnrollmentCode(
            SYNTHETIC_BASE_URL,
            syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 12),
        )

        assertEquals(DeviceAuthActionResult.SERVER_REJECTED, result)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Reauth)
    }

    @Test
    fun reauthBootstrapFailureNeverRestoresStaticApiAuthority() = runBlocking {
        val reauth = StoredDeviceAuthState.Reauth(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
            previousSessionId = SYNTHETIC_SESSION_ID,
            reason = REAUTH_REFRESH_REJECTED,
        )
        val store = FakeDeviceAuthEnvelopeStore(reauth)
        val transport = RecordingDeviceAuthTransport().apply {
            createHandler = { throw IOException("synthetic response loss") }
        }
        val coordinator = coordinator(store, transport)

        assertEquals(
            DeviceAuthActionResult.PENDING_RETRY,
            coordinator.upgradeWithBootstrap(SYNTHETIC_BASE_URL, "synthetic-bootstrap"),
        )
        assertTrue(store.envelope.state is StoredDeviceAuthState.EnrollmentCreationPending)
        assertNull(coordinator.authenticatedConfiguration())
        assertFalse(coordinator.snapshot().hasBearerToken)
    }

    @Test
    fun possiblyLiveReauthSessionCannotSwitchOriginWithoutLocalOnlyConfirmation() = runBlocking {
        val store = FakeDeviceAuthEnvelopeStore(
            StoredDeviceAuthState.Reauth(
                baseUrl = SYNTHETIC_BASE_URL,
                clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                previousSessionId = SYNTHETIC_SESSION_ID,
                reason = REAUTH_REFRESH_REJECTED,
            ),
        )
        val transport = RecordingDeviceAuthTransport()
        val coordinator = coordinator(store, transport)

        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            coordinator.consumeOneTimeEnrollmentCode(
                "https://other.example.test/",
                syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 13),
            ),
        )
        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            coordinator.upgradeWithBootstrap(
                "https://other.example.test/",
                "synthetic-bootstrap",
            ),
        )
        assertTrue(transport.createCalls.isEmpty())
        assertTrue(transport.consumeCalls.isEmpty())
    }

    @Test
    fun healthyActiveAndRefreshPendingSessionsRejectReplacement() = runBlocking {
        val active = syntheticActiveState(now)
        val activeStore = FakeDeviceAuthEnvelopeStore(active)
        val transport = RecordingDeviceAuthTransport()
        val activeCoordinator = coordinator(activeStore, transport)

        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            activeCoordinator.consumeOneTimeEnrollmentCode(
                SYNTHETIC_BASE_URL,
                syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 14),
            ),
        )
        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            activeCoordinator.upgradeWithBootstrap(SYNTHETIC_BASE_URL, "synthetic-bootstrap"),
        )

        val pending = StoredDeviceAuthState.RefreshPending(
            baseUrl = active.baseUrl,
            clientInstanceId = active.clientInstanceId,
            session = active.session,
            preparedAt = now.toString(),
            currentAccessToken = active.accessToken,
            currentRefreshToken = active.refreshToken,
            nextAccessToken = DeviceAuthSecret(
                syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 15),
            ),
            nextRefreshToken = DeviceAuthSecret(
                syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 16),
            ),
        )
        val pendingCoordinator = coordinator(FakeDeviceAuthEnvelopeStore(pending), transport)
        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            pendingCoordinator.upgradeWithBootstrap(SYNTHETIC_BASE_URL, "synthetic-bootstrap"),
        )
        assertEquals(
            DeviceAuthActionResult.NOT_ALLOWED,
            pendingCoordinator.consumeOneTimeEnrollmentCode(
                SYNTHETIC_BASE_URL,
                syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 17),
            ),
        )
        assertTrue(transport.createCalls.isEmpty())
        assertTrue(transport.consumeCalls.isEmpty())
    }

    @Test
    fun refreshResponseLossAndRestartReuseExactJournaledPair() = runBlocking {
        val priorIssued = now.minus(Duration.ofMinutes(14))
        val active = syntheticActiveState(
            now = now,
            session = syntheticSession(
                now = now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = priorIssued,
                lastSeenAt = priorIssued,
                accessExpiresAt = now.plus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = priorIssued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.minus(Duration.ofDays(1)).plus(DEVICE_AUTH_ABSOLUTE_TTL),
            ),
        )
        val store = FakeDeviceAuthEnvelopeStore(active)
        val firstTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = { throw IOException("synthetic lost refresh response") }
        }

        assertEquals(
            DeviceAuthActionResult.PENDING_RETRY,
            coordinator(store, firstTransport).signOutRevokeFirst(),
        )
        val pending = store.envelope.state as StoredDeviceAuthState.RefreshPending
        val firstCall = firstTransport.refreshCalls.single()

        val restartedTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = { call ->
                DeviceSessionMutationResponse(
                    session = syntheticSession(
                        now = now,
                        id = pending.session.id,
                        clientInstanceId = pending.clientInstanceId,
                        revision = pending.session.revision + 1,
                        createdAt = Instant.parse(pending.session.createdAt),
                        issuedAt = now,
                        lastSeenAt = now,
                        absoluteExpiresAt = Instant.parse(pending.session.absoluteExpiresAt),
                    ),
                    replayed = true,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, restartedTransport).recoverPendingOrUpgradeLegacy(),
        )
        val replay = restartedTransport.refreshCalls.single()
        assertEquals(firstCall.refreshToken, replay.refreshToken)
        assertEquals(firstCall.request.nextAccessToken, replay.request.nextAccessToken)
        assertEquals(firstCall.request.nextRefreshToken, replay.request.nextRefreshToken)
        val recovered = store.envelope.state as StoredDeviceAuthState.Active
        assertEquals(pending.nextAccessToken, recovered.accessToken)
        assertEquals(pending.nextRefreshToken, recovered.refreshToken)
    }

    @Test
    fun refreshGenerationCollisionFailsCleanlyBeforeTransition() = runBlocking {
        val issued = now.minus(Duration.ofMinutes(14))
        val active = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = issued,
                lastSeenAt = issued,
                accessExpiresAt = now.plus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = issued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val generator = QueueDeviceCredentialGenerator().apply {
            repeat(16) { enqueueToken(active.accessToken.value) }
        }
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = RecordingDeviceAuthTransport()

        assertEquals(
            DeviceAuthActionResult.STORAGE_FAILURE,
            coordinator(store, transport, generator = generator).signOutRevokeFirst(),
        )
        assertEquals(active, store.envelope.state)
        assertTrue(transport.refreshCalls.isEmpty())
        assertTrue(transport.revokeCalls.isEmpty())
    }

    @Test
    fun refreshAcceptsEqualSameTickTimestampsWhenRevisionAdvances() = runBlocking {
        val issued = now
        val active = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = issued,
                lastSeenAt = issued,
                accessExpiresAt = now.plus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = issued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = RecordingDeviceAuthTransport().apply {
            revokeHandler = {}
            refreshHandler = {
                DeviceSessionMutationResponse(
                    session = syntheticSession(
                        now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        issuedAt = issued,
                        lastSeenAt = issued,
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, transport).signOutRevokeFirst(),
        )
        assertEquals(1, transport.refreshCalls.size)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Unconfigured)
    }

    @Test
    fun refreshRejectsTimestampRegressionAndIssuanceBeforeJournal() = runBlocking {
        val active = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = now,
                lastSeenAt = now,
                accessExpiresAt = now.plus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = now.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val regressionStore = FakeDeviceAuthEnvelopeStore(active)
        val regressionTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                val regressed = now.minusSeconds(1)
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        issuedAt = regressed,
                        lastSeenAt = regressed,
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
        }
        assertEquals(
            DeviceAuthActionResult.SERVER_REJECTED,
            coordinator(regressionStore, regressionTransport).signOutRevokeFirst(),
        )
        assertTrue(regressionStore.envelope.state is StoredDeviceAuthState.Reauth)

        val olderIssued = now.minus(Duration.ofMinutes(10))
        val older = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = olderIssued,
                lastSeenAt = olderIssued,
                accessExpiresAt = now.plus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = olderIssued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val beforeJournalStore = FakeDeviceAuthEnvelopeStore(older)
        val beforeJournalTransport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                val issuedBeforeJournal = now.minus(Duration.ofMinutes(5)).minusSeconds(1)
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = older.session.id,
                        clientInstanceId = older.clientInstanceId,
                        revision = older.session.revision + 1,
                        createdAt = Instant.parse(older.session.createdAt),
                        issuedAt = issuedBeforeJournal,
                        lastSeenAt = issuedBeforeJournal,
                        absoluteExpiresAt = Instant.parse(older.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
        }
        assertEquals(
            DeviceAuthActionResult.SERVER_REJECTED,
            coordinator(beforeJournalStore, beforeJournalTransport).signOutRevokeFirst(),
        )
        assertTrue(beforeJournalStore.envelope.state is StoredDeviceAuthState.Reauth)
    }

    @Test
    fun delayedExactRefreshReplaySurvivesAccessExpiry() = runBlocking {
        val preparedAt = now.minus(Duration.ofMinutes(20))
        val priorIssued = now.minus(Duration.ofMinutes(30))
        val active = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = priorIssued,
                lastSeenAt = priorIssued,
                accessExpiresAt = priorIssued.plus(DEVICE_AUTH_ACCESS_TTL),
                refreshIdleExpiresAt = priorIssued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val pending = StoredDeviceAuthState.RefreshPending(
            baseUrl = active.baseUrl,
            clientInstanceId = active.clientInstanceId,
            session = active.session,
            preparedAt = preparedAt.toString(),
            currentAccessToken = active.accessToken,
            currentRefreshToken = active.refreshToken,
            nextAccessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 81)),
            nextRefreshToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 82)),
        )
        val store = FakeDeviceAuthEnvelopeStore(pending)
        val transport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        issuedAt = preparedAt,
                        lastSeenAt = preparedAt,
                        accessExpiresAt = preparedAt.plus(DEVICE_AUTH_ACCESS_TTL),
                        refreshIdleExpiresAt = preparedAt.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = true,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, transport).recoverPendingOrUpgradeLegacy(),
        )
        val recovered = store.envelope.state as StoredDeviceAuthState.Active
        assertTrue(recovered.session.accessExpiry.isBefore(now))
    }

    @Test
    fun expiredRefreshValidityRequiresReauthenticationWithoutNetworkUse() = runBlocking {
        val issued = now.minus(Duration.ofMinutes(16))
        val expired = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = issued,
                lastSeenAt = issued,
                accessExpiresAt = now.minus(Duration.ofMinutes(1)),
                refreshIdleExpiresAt = now,
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val store = FakeDeviceAuthEnvelopeStore(expired)
        val transport = RecordingDeviceAuthTransport()

        assertEquals(
            DeviceAuthActionResult.AUTH_REQUIRED,
            coordinator(store, transport).signOutRevokeFirst(),
        )
        assertTrue(store.envelope.state is StoredDeviceAuthState.Reauth)
        assertTrue(transport.refreshCalls.isEmpty())
        assertTrue(transport.revokeCalls.isEmpty())
    }

    @Test
    fun signOutRevokesFirstThenFencesAndDestroys() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = RecordingDeviceAuthTransport().apply { revokeHandler = {} }
        val fence = RecordingDeviceAuthFence()

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, transport, fence = fence).signOutRevokeFirst(),
        )
        assertEquals(active.session.id, transport.revokeCalls.single().sessionId)
        assertEquals(
            listOf(SYNTHETIC_BASE_URL, active.session.id, null, null),
            fence.calls.single(),
        )
        assertTrue(store.envelope.state is StoredDeviceAuthState.Unconfigured)
    }

    @Test
    fun signOutReportsCredentialsRemovedWhenOnlyObsoleteKeyCleanupRemains() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active).apply {
            leaveDestroyCleanupPending = true
        }
        val transport = RecordingDeviceAuthTransport().apply { revokeHandler = {} }
        val coordinator = coordinator(store, transport)

        assertEquals(DeviceAuthActionResult.CLEANUP_PENDING, coordinator.signOutRevokeFirst())
        assertTrue(store.envelope.state is StoredDeviceAuthState.Incompatible)
        assertEquals(DeviceAuthPhase.INCOMPATIBLE, coordinator.uiState.value.phase)
        assertTrue(coordinator.uiState.value.message.contains("credentials were removed"))
        assertFalse(coordinator.uiState.value.message.contains("retained"))
    }

    @Test
    fun snapshotPublishesUnconfiguredAfterDestroyTombstoneCleanupCompletes() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active).apply {
            leaveDestroyCleanupPending = true
        }
        val coordinator = coordinator(
            store,
            RecordingDeviceAuthTransport().apply { revokeHandler = {} },
        )

        assertEquals(DeviceAuthActionResult.CLEANUP_PENDING, coordinator.signOutRevokeFirst())
        assertEquals(DeviceAuthPhase.INCOMPATIBLE, coordinator.uiState.value.phase)
        store.forceState(
            StoredDeviceAuthState.Unconfigured(
                baseUrl = null,
                clientInstanceId = "99999999-9999-4999-8999-999999999999",
            ),
        )

        assertFalse(coordinator.snapshot().hasBearerToken)
        assertEquals(DeviceAuthPhase.UNCONFIGURED, coordinator.uiState.value.phase)
        assertTrue(coordinator.uiState.value.message.contains("cleanup finished"))
    }

    @Test
    fun failedOrStaleSignOutRetainsNewerLocalState() = runBlocking {
        val active = syntheticActiveState(now)
        val failedStore = FakeDeviceAuthEnvelopeStore(active)
        val failedTransport = RecordingDeviceAuthTransport().apply {
            revokeHandler = { throw IOException("synthetic network failure") }
        }
        assertEquals(
            DeviceAuthActionResult.NETWORK_FAILURE,
            coordinator(failedStore, failedTransport).signOutRevokeFirst(),
        )
        assertEquals(active, failedStore.envelope.state)

        val staleStore = FakeDeviceAuthEnvelopeStore(active)
        val replacement = active.copy(
            accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 91)),
        )
        val staleTransport = RecordingDeviceAuthTransport().apply {
            revokeHandler = { staleStore.forceState(replacement) }
        }
        val fence = RecordingDeviceAuthFence()
        assertEquals(
            DeviceAuthActionResult.STALE_STATE,
            coordinator(staleStore, staleTransport, fence = fence).signOutRevokeFirst(),
        )
        assertEquals(replacement, staleStore.envelope.state)
        assertTrue(fence.calls.isEmpty())
    }

    @Test
    fun signOut401RefreshesOnceAndRetriesSameSessionDelete() = runBlocking {
        val priorIssued = now.minus(Duration.ofMinutes(10))
        val active = syntheticActiveState(
            now,
            session = syntheticSession(
                now,
                createdAt = now.minus(Duration.ofDays(1)),
                issuedAt = priorIssued,
                lastSeenAt = priorIssued,
                accessExpiresAt = now.plus(Duration.ofMinutes(5)),
                refreshIdleExpiresAt = priorIssued.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
                absoluteExpiresAt = now.plus(Duration.ofDays(179)),
            ),
        )
        val store = FakeDeviceAuthEnvelopeStore(active)
        var revokeCount = 0
        val transport = RecordingDeviceAuthTransport().apply {
            revokeHandler = {
                revokeCount += 1
                if (revokeCount == 1) throw DeviceAuthApiException.Authentication()
            }
            refreshHandler = {
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        issuedAt = now,
                        lastSeenAt = now,
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, transport).signOutRevokeFirst(),
        )
        assertEquals(2, transport.revokeCalls.size)
        assertEquals(
            transport.revokeCalls.first().sessionId,
            transport.revokeCalls.last().sessionId,
        )
        assertEquals(transport.revokeCalls.first().baseUrl, transport.revokeCalls.last().baseUrl)
        assertNotEquals(
            transport.revokeCalls.first().accessToken,
            transport.revokeCalls.last().accessToken,
        )
        assertEquals(1, transport.refreshCalls.size)
    }

    @Test
    fun secondTrustedRevoke401QuarantinesOnlyExactRefreshedEnvelope() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        val fence = RecordingDeviceAuthFence()
        val transport = RecordingDeviceAuthTransport().apply {
            revokeHandler = { throw DeviceAuthApiException.Authentication() }
            refreshHandler = {
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            DeviceAuthActionResult.AUTH_REQUIRED,
            coordinator(store, transport, fence = fence).signOutRevokeFirst(),
        )
        val reauth = store.envelope.state as StoredDeviceAuthState.Reauth
        assertEquals(active.session.id, reauth.previousSessionId)
        assertEquals(REAUTH_SESSION_REVOKED, reauth.reason)
        assertEquals(2, transport.revokeCalls.size)
        assertEquals(1, transport.refreshCalls.size)
        assertTrue(fence.calls.isEmpty())
    }

    @Test
    fun staleSecondRevoke401CannotRetireNewerEnvelope() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        lateinit var replacement: StoredDeviceAuthState.Active
        var revokeCount = 0
        val transport = RecordingDeviceAuthTransport().apply {
            refreshHandler = {
                DeviceSessionMutationResponse(
                    syntheticSession(
                        now,
                        id = active.session.id,
                        clientInstanceId = active.clientInstanceId,
                        revision = active.session.revision + 1,
                        createdAt = Instant.parse(active.session.createdAt),
                        absoluteExpiresAt = Instant.parse(active.session.absoluteExpiresAt),
                    ),
                    replayed = false,
                )
            }
            revokeHandler = {
                revokeCount += 1
                if (revokeCount == 1) {
                    throw DeviceAuthApiException.Authentication()
                }
                val refreshed = store.envelope.state as StoredDeviceAuthState.Active
                replacement = refreshed.copy(
                    accessToken = DeviceAuthSecret(
                        syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 119),
                    ),
                )
                store.forceState(replacement)
                throw DeviceAuthApiException.Authentication()
            }
        }

        assertEquals(
            DeviceAuthActionResult.STALE_STATE,
            coordinator(store, transport).signOutRevokeFirst(),
        )
        assertEquals(replacement, store.envelope.state)
    }

    @Test
    fun concurrentRevokeWaitersShareOneNetworkLeader() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val transport = RecordingDeviceAuthTransport().apply {
            revokeHandler = {
                started.complete(Unit)
                release.await()
            }
        }
        val coordinator = coordinator(store, transport)

        val first = async { coordinator.signOutRevokeFirst() }
        withTimeout(3_000) { started.await() }
        val second = async { coordinator.signOutRevokeFirst() }
        yield()
        release.complete(Unit)
        val results = listOf(
            withTimeout(3_000) { first.await() },
            withTimeout(3_000) { second.await() },
        )

        assertEquals(1, transport.revokeCalls.size)
        assertTrue(DeviceAuthActionResult.SUCCESS in results)
        assertTrue(DeviceAuthActionResult.STALE_STATE in results)
    }

    @Test
    fun revoke404IsNotSuccessAndLocalStateRemains() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        val transport = RecordingDeviceAuthTransport().apply {
            revokeHandler = { throw DeviceAuthApiException.Http(404) }
        }

        assertEquals(
            DeviceAuthActionResult.SERVER_REJECTED,
            coordinator(store, transport).signOutRevokeFirst(),
        )
        assertEquals(active, store.envelope.state)
    }

    @Test
    fun compatibilityWritesCannotBypassBindingFence() {
        val active = syntheticActiveState(now)
        val coordinator = coordinator(FakeDeviceAuthEnvelopeStore(active), RecordingDeviceAuthTransport())

        val failure = runCatching {
            coordinator.update(SYNTHETIC_BASE_URL, "synthetic-bootstrap")
        }.exceptionOrNull()

        assertTrue(failure is InvalidApiConfigurationException)
        assertEquals(active.session.id, coordinator.snapshot().configurationId)
    }

    @Test
    fun localDestroyKeepsCacheFenceAndCredentialCommitInOneWriterSection() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        val writerEntered = CompletableDeferred<Unit>()
        val releaseFence = CompletableDeferred<Unit>()
        val fence = object : DeviceAuthBindingFence {
            override suspend fun beforeBindingChange(
                previousBaseUrl: String?,
                previousBindingId: String?,
                nextBaseUrl: String?,
                nextBindingId: String?,
            ): Boolean {
                assertEquals(active.baseUrl, previousBaseUrl)
                assertEquals(active.session.id, previousBindingId)
                assertNull(nextBaseUrl)
                assertNull(nextBindingId)
                assertEquals(active, store.envelope.state)
                writerEntered.complete(Unit)
                releaseFence.await()
                assertEquals(active, store.envelope.state)
                return true
            }
        }
        val gate = ApiBindingOperationGate()
        val coordinator = coordinator(
            store = store,
            transport = RecordingDeviceAuthTransport(),
            fence = fence,
            gate = gate,
        )

        val destroy = async { coordinator.destroyLocalOnly(confirmed = true) }
        withTimeout(3_000) { writerEntered.await() }
        val configuration = requireNotNull(coordinator.authenticatedConfiguration())
        val operationStarted = CompletableDeferred<Unit>()
        val operation = async {
            runCatching {
                configuration.withBindingOperation {
                    operationStarted.complete(Unit)
                }
            }.exceptionOrNull()
        }
        yield()

        assertEquals(active, store.envelope.state)
        assertFalse(operationStarted.isCompleted)
        releaseFence.complete(Unit)

        assertEquals(DeviceAuthActionResult.SUCCESS, withTimeout(3_000) { destroy.await() })
        assertTrue(withTimeout(3_000) { operation.await() } is ApiBindingChangedException)
        assertFalse(operationStarted.isCompleted)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Unconfigured)
    }

    @Test
    fun confirmedLocalDestroyUsesTheExplicitAmbiguousJournalQuarantinePath() = runBlocking {
        val active = syntheticActiveState(now)
        val store = FakeDeviceAuthEnvelopeStore(active)
        var ordinaryBindingChanges = 0
        var confirmedDestructions = 0
        val fence = object : DeviceAuthBindingFence {
            override suspend fun beforeBindingChange(
                previousBaseUrl: String?,
                previousBindingId: String?,
                nextBaseUrl: String?,
                nextBindingId: String?,
            ): Boolean {
                ordinaryBindingChanges += 1
                return false
            }

            override suspend fun beforeConfirmedLocalDestruction(
                previousBaseUrl: String?,
                previousBindingId: String?,
            ): Boolean {
                assertEquals(active.baseUrl, previousBaseUrl)
                assertEquals(active.session.id, previousBindingId)
                confirmedDestructions += 1
                return true
            }
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            coordinator(store, RecordingDeviceAuthTransport(), fence).destroyLocalOnly(true),
        )
        assertEquals(0, ordinaryBindingChanges)
        assertEquals(1, confirmedDestructions)
        assertTrue(store.envelope.state is StoredDeviceAuthState.Unconfigured)
    }

    @Test
    fun exhaustedBindingGenerationRejectsWriterBeforeAnyMutation() = runBlocking {
        val gate = ApiBindingOperationGate(initialGeneration = Long.MAX_VALUE)
        var mutationEntered = false

        val failure = runCatching {
            gate.invalidateBeforeQuarantine {
                mutationEntered = true
            }
        }.exceptionOrNull()

        assertTrue(failure is SecureCredentialException)
        assertFalse(mutationEntered)
        val retryFailure = withTimeout(3_000) {
            runCatching {
                gate.invalidateBeforeQuarantine {
                    mutationEntered = true
                }
            }.exceptionOrNull()
        }
        assertTrue(retryFailure is SecureCredentialException)
        assertFalse(mutationEntered)
    }

    @Test
    fun secretBearingDiagnosticsAreRedacted() {
        val secret = syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 42)
        val enrollment = DeviceEnrollmentIssuedResponse(
            SYNTHETIC_SESSION_ID,
            syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 43),
            now.plus(Duration.ofMinutes(10)).toString(),
            DEVICE_AUTH_CONTRACT_VERSION,
            replayed = false,
        )
        val consume = ConsumeDeviceEnrollmentRequest(
            SYNTHETIC_SESSION_ID,
            secret,
            syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 44),
        )
        val active = syntheticActiveState(now, accessMarker = 45, refreshMarker = 46)

        assertFalse(DeviceAuthSecret(secret).toString().contains(secret))
        assertFalse(enrollment.toString().contains(enrollment.enrollmentToken))
        assertFalse(consume.toString().contains(secret))
        assertFalse(active.toString().contains(active.accessToken.value))
        assertFalse(active.toString().contains(active.refreshToken.value))
        assertFalse(
            AuthenticatedApiConfiguration.create(SYNTHETIC_BASE_URL, secret)
                .toString()
                .contains(secret),
        )
    }

    private fun unconfiguredStore() = FakeDeviceAuthEnvelopeStore(
        StoredDeviceAuthState.Unconfigured(
            baseUrl = SYNTHETIC_BASE_URL,
            clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
        ),
    )

    private fun coordinator(
        store: FakeDeviceAuthEnvelopeStore,
        transport: RecordingDeviceAuthTransport,
        fence: DeviceAuthBindingFence = AllowDeviceAuthBindingChange,
        generator: DeviceCredentialGenerator = QueueDeviceCredentialGenerator(),
        gate: ApiBindingOperationGate = ApiBindingOperationGate(),
    ) = DurableDeviceAuthCoordinator(
        store = store,
        transport = transport,
        clientVersion = SYNTHETIC_CLIENT_VERSION,
        deviceLabel = SYNTHETIC_DEVICE_LABEL,
        bindingOperationGate = gate,
        bindingFence = fence,
        now = { now },
        generator = generator,
    )
}
