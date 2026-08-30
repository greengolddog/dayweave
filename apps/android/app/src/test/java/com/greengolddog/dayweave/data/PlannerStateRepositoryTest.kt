package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.PendingSchedulePublication
import com.greengolddog.dayweave.model.PendingProposalApplicationMutation
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.model.ProposalApplicationReceiptSnapshot
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.TerminalExecutionOutcomeSnapshot
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SchedulePublishRequest
import com.greengolddog.dayweave.network.buildSchedulePublishHttpRequest
import com.greengolddog.dayweave.network.prepareProposalApplyHttpRequest
import com.greengolddog.dayweave.network.prepareProposalUndoHttpRequest
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class PlannerStateRepositoryTest {
    @Test
    fun legacyV2PayloadDefaultsSensitivityAndIsRewrittenAsV7() = runBlocking {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 7,
                payloadFormat = PlannerSnapshotFormats.JSON_V2,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 11 }

        val restored = requireNotNull(repository.load())

        assertFalse(restored.schedule.single().isSensitive)
        assertFalse(restored.canonicalItems.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)
        assertEquals(11L, dao.snapshot?.updatedAtEpochMillis)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":false"))
    }

    @Test
    fun sensitiveCanarySurvivesEncryptedSnapshotRoundTrip() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 13 }
        val state = DayWeaveUiState(
            schedule = listOf(
                ScheduleItem(
                    id = "SYNTHETIC-SENSITIVE-BLOCK-ANDROID",
                    isSensitive = true,
                    title = "SYNTHETIC-SENSITIVE-BLOCK-TITLE",
                    kind = ItemKind.TASK,
                    startMinute = 540,
                    durationMinutes = 30,
                    status = ItemStatus.SCHEDULED,
                ),
            ),
            canonicalItems = listOf(sensitiveCanonicalItem()),
            inbox = listOf(
                InboxItem(
                    id = "SYNTHETIC-SENSITIVE-INBOX-ANDROID",
                    isSensitive = true,
                    title = "SYNTHETIC-SENSITIVE-INBOX-TITLE",
                    source = InboxSource.QUICK_CAPTURE,
                ),
            ),
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertTrue(restored.schedule.single().isSensitive)
        assertTrue(restored.canonicalItems.single().isSensitive)
        assertTrue(restored.inbox.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":true"))
    }

    @Test
    fun deferredExecutionHistorySurvivesEncryptedSnapshotRoundTrip() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 17 }
        val deferred = CanonicalExecutionSessionSnapshot(
            id = "44444444-4444-4444-8444-444444444444",
            itemId = "11111111-1111-4111-8111-111111111111",
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = "22222222-2222-4222-8222-222222222222",
            sourceDeviceId = "33333333-3333-4333-8333-333333333333",
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 135,
            actualSeconds = 135,
            startedAt = "2026-09-01T06:45:00Z",
            endedAt = "2026-09-01T07:00:00Z",
            moveStart = "2026-09-01T08:00:00Z",
            moveEnd = "2026-09-01T09:00:00Z",
            createdAt = "2026-09-01T06:45:00Z",
            updatedAt = "2026-09-01T07:00:00Z",
        )
        val state = DayWeaveUiState(
            canonicalExecutionSyncOrigin = "https://api.example.test/",
            canonicalExecutionRevision = 2,
            canonicalExecutionHistoryWindow = listOf(deferred),
            canonicalExecutionHistoryWindowRevision = 2,
            canonicalExecutionHistoryContinuityEstablished = true,
            canonicalExecutionHistoryVerified = true,
            terminalExecutionOutcomes = mapOf(
                deferred.id to TerminalExecutionOutcomeSnapshot(
                    syncOrigin = "https://api.example.test/",
                    session = deferred,
                    requiresCanonicalItemProjection = false,
                    recordedAt = requireNotNull(deferred.endedAt),
                ),
            ),
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(listOf(deferred), restored.canonicalExecutionHistoryWindow)
        assertEquals(2L, restored.canonicalExecutionHistoryWindowRevision)
        assertTrue(restored.canonicalExecutionHistoryVerified)
        val retained = restored.terminalExecutionOutcomes.getValue(deferred.id)
        assertEquals(deferred, retained.session)
        assertFalse(retained.requiresCanonicalItemProjection)
        assertEquals(deferred.endedAt, retained.recordedAt)
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"moveStart\":"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"moveEnd\":"))
    }

    @Test
    fun exactSchedulePublicationJournalRoundTripsAndTamperingFailsClosed() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)
        val state = pendingPublicationState()

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(
            state.pendingSchedulePublication,
            restored.pendingSchedulePublication,
        )
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)

        val digest = "sha256:${"a".repeat(64)}"
        val tampered = requireNotNull(dao.snapshot).payload.replaceFirst(
            digest,
            "sha256:${"b".repeat(64)}",
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(payload = tampered)

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun exactProposalApplyJournalRoundTripsAndEndpointTamperingFailsClosed() = runBlocking {
        val configuration = AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "synthetic-token",
            "connection-1",
        )
        val proposalId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val previewId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        val reviewHash = "sha256:${"c".repeat(64)}"
        val pending = PendingProposalApplicationMutation(
            schemaVersion = 1,
            kind = ProposalApplicationMutationKind.APPLY,
            idempotencyKey = "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = "connection-1",
            proposalId = proposalId,
            expectedProposalRevision = 4,
            expectedCommandIds = listOf("dddddddd-dddd-4ddd-8ddd-dddddddddddd"),
            previewId = previewId,
            expectedReviewHash = reviewHash,
            preparedAt = "2026-08-30T10:00:00Z",
            request = prepareProposalApplyHttpRequest(configuration, previewId, reviewHash),
        )
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)

        repository.save(DayWeaveUiState(pendingProposalApplicationMutation = pending))
        assertEquals(
            pending,
            requireNotNull(repository.load()).pendingProposalApplicationMutation,
        )

        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = requireNotNull(dao.snapshot).payload.replaceFirst(
                "/application-previews/$previewId/apply",
                "/application-previews/$previewId/undo",
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun v5MigrationCreatesNoProposalApplicationState() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 41 }
        repository.save(DayWeaveUiState())
        val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload)
            .let { it as JsonObject }
        val legacy = JsonObject(
            root.filterKeys {
                it != "pendingProposalApplicationMutation" && it != "proposalApplications"
            },
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(JsonObject.serializer(), legacy),
            payloadFormat = PlannerSnapshotFormats.JSON_V5,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(null, restored.pendingProposalApplicationMutation)
        assertTrue(restored.proposalApplications.isEmpty())
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)
    }

    @Test
    fun exactUndoJournalRoundTripsOnlyWithItsMatchingAppliedReceipt() = runBlocking {
        val configuration = AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "synthetic-token",
            "connection-1",
        )
        val proposalId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val applicationId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        val commandId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        val receipt = ProposalApplicationReceiptSnapshot(
            schemaVersion = 1,
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = "connection-1",
            applicationId = applicationId,
            proposalId = proposalId,
            appliedProposalRevision = 2,
            applicationRevision = 1,
            status = ProposalApplicationStatusSnapshot.APPLIED,
            commandIds = listOf(commandId),
            affectedItemIds = listOf("dddddddd-dddd-4ddd-8ddd-dddddddddddd"),
            appliedAt = "2026-08-30T10:00:00Z",
            undoExpiresAt = "2026-08-30T10:15:00Z",
        )
        val pending = PendingProposalApplicationMutation(
            schemaVersion = 1,
            kind = ProposalApplicationMutationKind.UNDO,
            idempotencyKey = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = "connection-1",
            proposalId = proposalId,
            expectedProposalRevision = 2,
            expectedCommandIds = listOf(commandId),
            applicationId = applicationId,
            expectedApplicationRevision = 1,
            preparedAt = "2026-08-30T10:05:00Z",
            request = prepareProposalUndoHttpRequest(configuration, applicationId, 1),
        )
        val repository = RoomPlannerStateRepository(FakePlannerSnapshotDao())
        val state = DayWeaveUiState(
            pendingProposalApplicationMutation = pending,
            proposalApplications = mapOf(proposalId to receipt),
        )

        repository.save(state)
        assertEquals(pending, requireNotNull(repository.load()).pendingProposalApplicationMutation)
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.save(state.copy(proposalApplications = emptyMap())) }
        }
        Unit
    }

    @Test
    fun currentV4PayloadMissingSensitivityFailsClosed() {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 17,
                payloadFormat = PlannerSnapshotFormats.JSON_V4,
            ),
        )
        val repository = RoomPlannerStateRepository(dao)

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(PlannerSnapshotFormats.JSON_V4, dao.snapshot?.payloadFormat)
        assertEquals(17L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun legacyV3StillRejectsMissingPreviouslyRequiredSensitivity() {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 18,
                payloadFormat = PlannerSnapshotFormats.JSON_V3,
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).load() }
        }
        assertEquals(PlannerSnapshotFormats.JSON_V3, dao.snapshot?.payloadFormat)
        assertEquals(18L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun legacyV3DerivesPendingSensitivityFromExactReplacementBody() = runBlocking {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V3_PENDING_PAYLOAD,
                updatedAtEpochMillis = 19,
                payloadFormat = PlannerSnapshotFormats.JSON_V3,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 23 }

        val restored = requireNotNull(repository.load())

        assertTrue(requireNotNull(restored.pendingCanonicalMutation).targetIsSensitive)
        assertFalse(restored.inbox.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"targetIsSensitive\":true"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":false"))
        assertTrue(requireNotNull(repository.load()).pendingCanonicalMutation?.targetIsSensitive == true)
    }

    @Test
    fun legacyV2PendingJournalWithoutPreexistingSensitivityMigratesExplicitlyFalse() = runBlocking {
        val preSensitivityPayload = LEGACY_V3_PENDING_PAYLOAD
            .replace("\"isSensitive\": true", "\"isSensitive\": false")
            .replace(",\\\"is_sensitive\\\":true", "")
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = preSensitivityPayload,
                updatedAtEpochMillis = 24,
                payloadFormat = PlannerSnapshotFormats.JSON_V2,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 25 }

        val restored = requireNotNull(repository.load())

        assertFalse(requireNotNull(restored.pendingCanonicalMutation).targetIsSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V7, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"targetIsSensitive\":false"))
    }

    @Test
    fun currentV4PendingMutationMissingSensitivityTargetFailsClosed() {
        val currentPayload = LEGACY_V3_PENDING_PAYLOAD.replace(
            "\"source\": \"QUICK_CAPTURE\"",
            "\"source\": \"QUICK_CAPTURE\", \"isSensitive\": false",
        )
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = currentPayload,
                updatedAtEpochMillis = 29,
                payloadFormat = PlannerSnapshotFormats.JSON_V4,
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).load() }
        }
        assertEquals(29L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun currentV4RejectsSensitivityTargetThatDisagreesWithWireJournal() {
        val currentPayload = LEGACY_V3_PENDING_PAYLOAD
            .replace(
                "\"source\": \"QUICK_CAPTURE\"",
                "\"source\": \"QUICK_CAPTURE\", \"isSensitive\": false",
            )
            .replace(
                "\"targetStatus\": \"planned\",",
                "\"targetStatus\": \"planned\", \"targetIsSensitive\": false,",
            )
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = currentPayload,
                updatedAtEpochMillis = 31,
                payloadFormat = PlannerSnapshotFormats.JSON_V4,
            ),
        )

        val failure = assertThrows(SerializationException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).load() }
        }
        assertTrue(requireNotNull(failure.message).contains("does not match its exact replacement"))
        assertEquals(31L, dao.snapshot?.updatedAtEpochMillis)
    }

    private fun sensitiveCanonicalItem() = CanonicalItemSnapshot(
        id = "SYNTHETIC-SENSITIVE-CANONICAL-ANDROID",
        isSensitive = true,
        kind = "task",
        status = "planned",
        title = "SYNTHETIC-SENSITIVE-CANONICAL-TITLE",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-29T08:00:00Z",
        updatedAt = "2026-08-29T08:00:00Z",
    )

    private fun pendingPublicationState(): DayWeaveUiState {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val idempotencyKey = "33333333-3333-4333-8333-333333333333"
        val digest = "sha256:${"a".repeat(64)}"
        val schedule = SchedulePreviewRequest(
            asOf = "2026-08-29T08:00:00Z",
            horizonStart = "2026-08-29T00:00:00Z",
            horizonEnd = "2026-08-30T00:00:00Z",
            timezoneName = "UTC",
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = "2026-08-29T00:00:00Z",
                    end = "2026-08-30T00:00:00Z",
                ),
            ),
        )
        val candidate = CanonicalPlanUpdate(
            items = emptyList(),
            schedule = emptyList(),
            syncOrigin = origin,
            configurationId = configurationId,
            deltaCursor = "cursor-1",
            inputDigest = digest,
            generatedAt = schedule.asOf,
            planningZoneId = schedule.timezoneName,
            rejectedItemCount = 0,
            unscheduledItemCount = 0,
            protectedFreeMinutes = 0,
            dayScore = 100,
            violationMessages = emptyList(),
            violationCount = 0,
            errorViolationCount = 0,
            unscheduledWork = emptyList(),
            occurrenceSeriesItemIds = emptyMap(),
            message = "Synthetic pending publication",
        )
        val configuration = AuthenticatedApiConfiguration.createBound(
            origin,
            "synthetic-token",
            configurationId,
        )
        return DayWeaveUiState(
            pendingSchedulePublication = PendingSchedulePublication(
                schemaVersion = 1,
                idempotencyKey = idempotencyKey,
                syncOrigin = origin,
                configurationId = configurationId,
                preparedAt = schedule.asOf,
                request = buildSchedulePublishHttpRequest(
                    configuration,
                    SchedulePublishRequest(idempotencyKey, digest, schedule),
                ),
                candidate = candidate,
            ),
        )
    }

    private class FakePlannerSnapshotDao(
        var snapshot: PlannerSnapshotEntity? = null,
    ) : PlannerSnapshotDao {
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
        }
    }

    private companion object {
        const val LEGACY_V2_PAYLOAD = """
            {
              "schedule": [{
                "id": "SYNTHETIC-LEGACY-V2-BLOCK",
                "title": "SYNTHETIC-LEGACY-V2-BLOCK-TITLE",
                "kind": "TASK",
                "startMinute": 540,
                "durationMinutes": 30,
                "status": "SCHEDULED"
              }],
              "canonicalItems": [{
                "id": "SYNTHETIC-LEGACY-V2-CANONICAL",
                "kind": "task",
                "status": "planned",
                "title": "SYNTHETIC-LEGACY-V2-CANONICAL-TITLE",
                "timezoneName": "UTC",
                "durationSeconds": 1800,
                "flexibleConstraintsJson": "{}",
                "splitPolicyJson": "{\"type\":\"indivisible\"}",
                "importance": 50,
                "urgency": 50,
                "siblingOrder": 0,
                "isExecutable": true,
                "revision": 1,
                "createdAt": "2026-08-29T08:00:00Z",
                "updatedAt": "2026-08-29T08:00:00Z"
              }]
            }
        """

        const val LEGACY_V3_PENDING_PAYLOAD = """
            {
              "schedule": [],
              "canonicalItems": [{
                "id": "11111111-1111-4111-8111-111111111111",
                "isSensitive": true,
                "kind": "task",
                "status": "planned",
                "title": "SYNTHETIC-LEGACY-PENDING-CANONICAL",
                "timezoneName": "UTC",
                "durationSeconds": 1800,
                "flexibleConstraintsJson": "{}",
                "splitPolicyJson": "{\"type\":\"indivisible\"}",
                "importance": 50,
                "urgency": 50,
                "siblingOrder": 0,
                "isExecutable": true,
                "revision": 1,
                "createdAt": "2026-08-29T08:00:00Z",
                "updatedAt": "2026-08-29T08:00:00Z"
              }],
              "inbox": [{
                "id": "SYNTHETIC-LEGACY-INBOX",
                "title": "SYNTHETIC-LEGACY-INBOX-TITLE",
                "source": "QUICK_CAPTURE"
              }],
              "pendingCanonicalMutation": {
                "idempotencyKey": "22222222-2222-4222-8222-222222222222",
                "syncOrigin": "https://api.example.test/",
                "configurationId": "SYNTHETIC-CONNECTION",
                "itemId": "11111111-1111-4111-8111-111111111111",
                "expectedRevision": 1,
                "targetStatus": "planned",
                "startedAt": "2026-08-29T08:01:00Z",
                "replacementRequestJson": "{\"expected_revision\":1,\"item\":{\"status\":\"planned\",\"is_sensitive\":true}}",
                "focusedBlockId": "11111111-1111-4111-8111-111111111111",
                "displayStatus": "SCHEDULED"
              }
            }
        """
    }
}
