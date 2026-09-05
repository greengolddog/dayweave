package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecurrenceDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceKind
import com.greengolddog.dayweave.model.CanonicalBlockedReasonKind
import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitMissedPolicySnapshot
import com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot
import com.greengolddog.dayweave.model.HabitMissedResolutionSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceEvidenceSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeCommandSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.HabitPauseSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceMembershipProofSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceMembershipSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceStateSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionHintSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.RecurrenceMoveSnapshot
import com.greengolddog.dayweave.model.RecurrenceOutcomeSnapshot
import com.greengolddog.dayweave.model.RecurrenceOccurrenceSourceSnapshot
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import com.greengolddog.dayweave.model.assessMoveLater
import com.greengolddog.dayweave.model.toApprovalEnvelope
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.model.localScheduleCompositionFingerprintComputationCount
import com.greengolddog.dayweave.model.habitPolicyFingerprintOrNull
import com.greengolddog.dayweave.model.requireCanonicalReplacementSupport
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.CanonicalItemRevisionRequest
import com.greengolddog.dayweave.network.CreateCanonicalItemRequest
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.PlannerApiException
import com.greengolddog.dayweave.network.PlannerValidationReason
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.network.RemoteCurrentPublishedSchedule
import com.greengolddog.dayweave.network.RemoteItemDeltaChange
import com.greengolddog.dayweave.network.RemoteItemDeltaPage
import com.greengolddog.dayweave.network.RemoteItemTombstone
import com.greengolddog.dayweave.network.RemoteManualPlacementAssessment
import com.greengolddog.dayweave.network.RemoteManualPlacementConflict
import com.greengolddog.dayweave.network.RemoteManualPlacementViolation
import com.greengolddog.dayweave.network.RemotePlanViolation
import com.greengolddog.dayweave.network.RemotePlanScore
import com.greengolddog.dayweave.network.RemotePlanOccurrence
import com.greengolddog.dayweave.network.RemoteScheduleBlock
import com.greengolddog.dayweave.network.RemoteSchedulePlan
import com.greengolddog.dayweave.network.RemoteSchedulePreview
import com.greengolddog.dayweave.network.RemoteSchedulePublishResponse
import com.greengolddog.dayweave.network.RemotePublishedScheduleRevision
import com.greengolddog.dayweave.network.RemoteRejectedScheduleItem
import com.greengolddog.dayweave.network.RemoteUnscheduledWork
import com.greengolddog.dayweave.network.ReplaceCanonicalItemRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SchedulePublishHttpRequest
import com.greengolddog.dayweave.network.SchedulePublishRequest
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.scheduler.LocalScheduleComposer
import com.greengolddog.dayweave.scheduler.LocalScheduleComposition
import com.greengolddog.dayweave.scheduler.LocalScheduleCompositionRequestException
import com.greengolddog.dayweave.scheduler.LocalScheduleCompositionRequestTooLargeException
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.io.IOException
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalSyncManagerTest {
    @Test
    fun cursorEpochResetFenceCannotCrossCredentialBinding() = runBlocking {
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport()

        val outcome = manager(store, transport).refreshCurrentPublishedScheduleAfterCursorReset(
            ScheduleRevisionEpochResetFence(
                syncOrigin = "https://api.example.test/",
                configurationId = "replacement-connection",
                rejectedRevision = 9uL,
            ),
        )

        assertEquals(CanonicalRefreshOutcome.CONFIGURATION_ERROR, outcome)
        assertTrue(transport.currentScheduleConfigurations.isEmpty())
        assertTrue(transport.deltaCursors.isEmpty())
        assertEquals(DayWeaveUiState(), store.state.value)
    }

    @Test
    fun currentAndEmptyHeadRefreshFailIfStorageBecomesUnavailableBeforeInstall() = runBlocking {
        listOf(false, true).forEach { hasCurrentHead ->
            val initial = DayWeaveUiState(
                canonicalSyncOrigin = "https://api.example.test/",
                canonicalConfigurationId = "connection-1",
                executionDeviceId = DEVICE_ID,
            )
            var failSaves = false
            val repository = object : PlannerStateRepository {
                override suspend fun load(): DayWeaveUiState = initial

                override suspend fun save(state: DayWeaveUiState) {
                    if (failSaves) throw IOException("synthetic current-head persistence failure")
                }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val store = PlannerStore(initial, repository, scope)
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
                val head = currentSchedule(preview()).takeIf { hasCurrentHead }
                var currentCalls = 0
                val transport = FakeCanonicalTransport().apply {
                    pages[null] = RemoteItemDeltaPage(
                        listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                        "cursor-1",
                        false,
                    )
                    currentScheduleHandler = {
                        currentCalls += 1
                        if (!hasCurrentHead || currentCalls == 2) {
                            failSaves = true
                            val failed = requireNotNull(store.ensureExecutionDeviceId(DEVICE_ID))
                            assertFalse(failed.awaitDurable())
                            assertEquals(
                                PlannerLoadState.PERSISTENCE_FAILED,
                                store.loadState.value,
                            )
                        }
                        head
                    }
                }

                assertEquals(
                    CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
                    manager(store, transport).refreshCurrentPublishedSchedule(),
                )
                assertNull(store.state.value.publishedScheduleRevision)
                assertNull(store.state.value.publishedScheduleRevisionHint)
            } finally {
                scope.cancel()
            }
        }
    }

    private val clock = Instant.parse("2026-09-01T07:00:00Z")

    @Test
    fun explicitLegacyEquivalentStructureRemainsAuthorable() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val explicit = remoteItem(split = false).copy(
            flexibleConstraints = buildJsonObject { },
            durationKind = CanonicalDurationKind.EXACT,
            durationMinSeconds = 3_600,
            durationMaxSeconds = 3_600,
            durationSource = CanonicalDurationSource.USER,
            deadlineKind = CanonicalDeadlineKind.DATE_TIME,
            deadlineStrength = CanonicalDeadlineStrength.HARD,
            hasOwnEffort = false,
        )
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(preview())
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = explicit)),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshCurrentPublishedSchedule(),
        )
        val cached = plannerStore.state.value.canonicalItems.single()
        assertFalse(cached.hasExplicitStructuralMetadata)
        assertNotNull(cached.requireCanonicalReplacementSupport())
        assertTrue(
            runCatching {
                cached.copy(kind = "project").requireCanonicalReplacementSupport()
            }.isFailure,
        )
    }

    @Test
    fun futureStructureAndItemSemanticsRemainExactAndReadOnly() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val explicit = remoteItem(split = false).copy(
            kind = "travel_reservation",
            status = "waiting_for_review",
            durationKind = CanonicalDurationKind.EXACT,
            durationMinSeconds = 3_600,
            durationMaxSeconds = 3_600,
            durationSource = CanonicalDurationSource("future_estimator_v2"),
            deadlineKind = CanonicalDeadlineKind.NONE,
            deadlineStrength = null,
            hasOwnEffort = false,
            blockedReasonKind = CanonicalBlockedReasonKind.MANUAL,
            blockedReason = "Waiting for review",
        )
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(preview())
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = explicit)),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshCurrentPublishedSchedule(),
        )
        val cached = plannerStore.state.value.canonicalItems.single()
        assertEquals("travel_reservation", cached.kind)
        assertEquals("waiting_for_review", cached.status)
        assertEquals("future_estimator_v2", cached.durationSource?.wireValue)
        assertEquals(CanonicalBlockedReasonKind.MANUAL, cached.blockedReasonKind)
        assertTrue(cached.hasExplicitStructuralMetadata)
    }

    @Test
    fun oversizedFutureStructuralDiscriminatorFailsClosed() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val invalid = remoteItem(split = false).copy(
            durationKind = CanonicalDurationKind.EXACT,
            durationMinSeconds = 3_600,
            durationMaxSeconds = 3_600,
            durationSource = CanonicalDurationSource("x".repeat(65)),
            deadlineKind = CanonicalDeadlineKind.DATE_TIME,
            deadlineStrength = CanonicalDeadlineStrength.HARD,
            hasOwnEffort = false,
        )
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(preview())
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = invalid)),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(plannerStore, transport).refreshCurrentPublishedSchedule(),
        )
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
    }

    @Test
    fun dateOnlyDeadlineOutsidePortableCanonicalRangeFailsClosed() = runBlocking {
        listOf("0000-01-01", "9999-12-31").forEach { deadlineDate ->
            val plannerStore = PlannerStore(DayWeaveUiState())
            val invalid = remoteItem(split = false).copy(
                deadlineAt = null,
                deadlineKind = CanonicalDeadlineKind.DATE,
                deadlineDate = deadlineDate,
                deadlineStrength = CanonicalDeadlineStrength.HARD,
                hasOwnEffort = false,
            )
            val transport = FakeCanonicalTransport().apply {
                currentScheduleResult = currentSchedule(preview())
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = invalid)),
                    "cursor-1",
                    false,
                )
            }

            assertEquals(
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
                manager(plannerStore, transport).refreshCurrentPublishedSchedule(),
            )
            assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        }
    }

    @Test
    fun currentScheduleDeltaStaysReadOnlyUntilExactSnapshotIsAtomicallyInstalled() = runBlocking {
        val initial = DayWeaveUiState()
        val plannerStore = PlannerStore(initial)
        val deltaStarted = CompletableDeferred<Unit>()
        val releaseDelta = CompletableDeferred<Unit>()
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(preview())
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            this.deltaStarted = deltaStarted
            deltaGate = releaseDelta
        }
        val refresh = async { manager(plannerStore, transport).refreshCurrentPublishedSchedule() }

        withTimeout(2_000) { deltaStarted.await() }
        assertEquals(initial, plannerStore.state.value)
        assertEquals(initial, plannerStore.durableState.value)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
        assertEquals(null, plannerStore.state.value.publishedScheduleProof)

        releaseDelta.complete(Unit)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, withTimeout(2_000) { refresh.await() })
        val installed = plannerStore.state.value
        assertEquals("cursor-1", installed.canonicalDeltaCursor)
        assertEquals(7L, installed.canonicalItems.single().revision)
        assertEquals(1uL, installed.publishedScheduleRevision?.revisionNumber)
        assertTrue(requireNotNull(installed.publishedScheduleProof).matchesPublishedPlan(
            installed.schedule,
        ))
        assertEquals(installed, plannerStore.durableState.value)
        assertTrue(transport.previewRequests.isEmpty())
        assertTrue(transport.publicationRequests.isEmpty())
        assertEquals(2, transport.currentScheduleConfigurations.size)
    }

    @Test
    fun currentScheduleAcceptsNamedPublicationZoneAndRejectsJavaOnlyFixedZoneForms() = runBlocking {
        val parisHead = currentSchedule(preview()).let { current ->
            current.copy(revision = current.revision.copy(timezoneName = "Europe/Paris"))
        }
        val parisStore = PlannerStore(DayWeaveUiState())
        val parisTransport = FakeCanonicalTransport().apply {
            currentScheduleResult = parisHead
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-paris",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(parisStore, parisTransport).refreshCurrentPublishedSchedule(),
        )
        assertEquals("Europe/Paris", parisStore.state.value.schedulePlanningZoneId)

        listOf("+02:00", "GMT+02:00").forEach { invalidZone ->
            val invalidStore = PlannerStore(DayWeaveUiState())
            val invalidTransport = FakeCanonicalTransport().apply {
                currentScheduleResult = currentSchedule(preview()).let { current ->
                    current.copy(revision = current.revision.copy(timezoneName = invalidZone))
                }
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-invalid",
                    false,
                )
            }

            assertEquals(
                invalidZone,
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
                manager(invalidStore, invalidTransport).refreshCurrentPublishedSchedule(),
            )
            assertEquals(DayWeaveUiState(), invalidStore.durableState.value)
        }
    }

    @Test
    fun currentScheduleReplicaAcceptsEveryPositiveWireHorizonThroughNinetyDays() = runBlocking {
        data class Case(
            val name: String,
            val asOf: Instant,
            val start: Instant,
            val end: Instant,
            val zoneId: String,
        )
        val cases = listOf(
            Case(
                name = "cross-platform repeated midnight hour",
                asOf = Instant.parse("2026-11-01T04:30:00Z"),
                start = Instant.parse("2026-11-01T04:00:00Z"),
                end = Instant.parse("2026-11-01T05:00:00Z"),
                zoneId = "America/Havana",
            ),
            Case(
                name = "thirty-one days",
                asOf = clock,
                start = Instant.parse("2026-08-31T22:00:00Z"),
                end = Instant.parse("2026-10-01T22:00:00Z"),
                zoneId = "Europe/Madrid",
            ),
            Case(
                name = "ninety absolute days",
                asOf = clock,
                start = Instant.parse("2026-08-31T22:00:00Z"),
                end = Instant.parse("2026-08-31T22:00:00Z").plus(Duration.ofDays(90)),
                zoneId = "Europe/Madrid",
            ),
        )

        cases.forEach { case ->
            val schedule = emptyPreview().withWindow(
                asOf = case.asOf,
                horizonStart = case.start.toString(),
                horizonEnd = case.end.toString(),
            )
            val transport = FakeCanonicalTransport().apply {
                currentScheduleResult = currentSchedule(schedule).let { current ->
                    current.copy(revision = current.revision.copy(timezoneName = case.zoneId))
                }
                pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-${case.name}", false)
            }
            val store = PlannerStore(DayWeaveUiState())

            assertEquals(
                case.name,
                CanonicalRefreshOutcome.SUCCESS,
                manager(store, transport, currentInstant = case.asOf)
                    .refreshCurrentPublishedSchedule(),
            )
            assertEquals(case.start.toString(), store.state.value.publishedScheduleRevision?.horizonStart)
            assertEquals(case.end.toString(), store.state.value.publishedScheduleRevision?.horizonEnd)
            assertTrue(requireNotNull(store.state.value.publishedScheduleProof).hasValidShape())
        }
    }

    @Test
    fun currentScheduleReplicaRejectsMoreThanNinetyAbsoluteDays() = runBlocking {
        val start = Instant.parse("2026-08-31T22:00:00Z")
        val schedule = emptyPreview().withWindow(
            asOf = clock,
            horizonStart = start.toString(),
            horizonEnd = start.plus(Duration.ofDays(90)).plusSeconds(1).toString(),
        )
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(schedule)
            pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-too-long", false)
        }
        val store = PlannerStore(DayWeaveUiState())

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleAcceptsUnscheduledDemandBeyondThePublicationHorizon() = runBlocking {
        val remaining = 200_000L
        val longWork = remoteItem().copy(
            durationSeconds = (remaining + 60L) * 60L,
        )
        val replicated = preview().copy(
            plan = preview().plan.copy(
                unscheduled = listOf(
                    RemoteUnscheduledWork(
                        itemId = TASK_ID,
                        remaining = remaining,
                        reason = "no_capacity",
                        message = "Demand beyond this horizon remains visible",
                    ),
                ),
                violations = listOf(
                    RemotePlanViolation(
                        kind = "capacity",
                        severity = "warning",
                        itemIds = listOf(TASK_ID),
                        occurrenceIds = emptyList(),
                        penalty = ULong.MAX_VALUE,
                        message = "Wire-sized penalty remains safely clamped for display",
                    ),
                ),
                score = RemotePlanScore(
                    scheduledMinutes = 60,
                    unscheduledMinutes = remaining,
                    softPenalty = ULong.MAX_VALUE,
                    movedMinutes = 4_294_967_295L,
                ),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(replicated)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = longWork)),
                "cursor-long-demand",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(remaining, store.state.value.unscheduledWork.single().remainingMinutes)
        assertEquals(0, store.state.value.dayScore)
        assertEquals(1, store.state.value.scheduleViolationCount)
    }

    @Test
    fun currentScheduleReplicaAcceptsValidatedExternalFixedContextWithoutMintingAuthority() =
        runBlocking {
            val externalId = SECOND_BLOCK_ID
            val replicated = preview().copy(
                manualPlacementAssessments = listOf(
                    RemoteManualPlacementAssessment(
                        placementId = "88888888-8888-4888-8888-888888888888",
                        environmentDigest = "sha256:${"b".repeat(64)}",
                        approvalDigest = "sha256:${"c".repeat(64)}",
                        approvalRequired = false,
                        violations = listOf(
                            RemoteManualPlacementViolation(
                                code = "immutable_overlap",
                                itemIds = listOf(TASK_ID),
                                occurrenceIds = emptyList(),
                                conflictingBlockIds = listOf(SECOND_BLOCK_ID),
                                conflictingBlocks = listOf(
                                    RemoteManualPlacementConflict(
                                        blockId = SECOND_BLOCK_ID,
                                        externalBlockId = externalId,
                                        kind = "external_fixed",
                                        start = "2026-09-01T10:00:00+02:00",
                                        end = "2026-09-04T10:00:00+02:00",
                                    ),
                                ),
                                start = "2026-09-01T10:00:00+02:00",
                                end = "2026-09-01T10:30:00+02:00",
                                message = "Authorized pinned work overlaps immutable context",
                            ),
                        ),
                    ),
                ),
                plan = preview().plan.copy(
                    blocks = preview().plan.blocks + RemoteScheduleBlock(
                        id = SECOND_BLOCK_ID,
                        isSensitive = true,
                        externalBlockId = externalId,
                        title = "Private calendar hold",
                        start = "2026-09-01T10:00:00+02:00",
                        end = "2026-09-04T10:00:00+02:00",
                        sessionIndex = 0,
                        kind = "external_fixed",
                        explanations = emptyList(),
                    ) + RemoteScheduleBlock(
                        id = THIRD_BLOCK_ID,
                        isSensitive = false,
                        itemId = TASK_ID,
                        title = "Compose Android timeline",
                        start = "2026-09-01T10:00:00+02:00",
                        end = "2026-09-01T10:30:00+02:00",
                        sessionIndex = 1,
                        kind = "pinned",
                        explanations = emptyList(),
                    ),
                    score = RemotePlanScore(90, 0, 0uL, 0),
                ),
            )
            val transport = FakeCanonicalTransport().apply {
                currentScheduleResult = currentSchedule(replicated)
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-1",
                    false,
                )
            }
            val store = PlannerStore(DayWeaveUiState())

            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(store, transport).refreshCurrentPublishedSchedule(),
            )

            val external = store.state.value.schedule.single { it.id == SECOND_BLOCK_ID }
            assertEquals(null, external.canonicalItemId)
            assertEquals(null, external.canonicalRevision)
            assertEquals(ItemKind.EVENT, external.kind)
            assertTrue(external.isHardConstraint)
            assertTrue(external.isSensitive)
            assertEquals(3 * 24 * 60, external.durationMinutes)
            val proof = requireNotNull(store.state.value.publishedScheduleProof)
            assertEquals(
                listOf(BLOCK_ID, SECOND_BLOCK_ID, THIRD_BLOCK_ID),
                proof.blocks.map { it.id },
            )
            assertTrue(proof.matchesPublishedPlan(store.state.value.schedule))
            assertFalse(
                proof.matchesPublishedPlan(
                    store.state.value.schedule.filterNot { it.id == SECOND_BLOCK_ID },
                ),
            )
            assertFalse(
                proof.matchesPublishedPlan(
                    store.state.value.schedule.map { block ->
                        if (block.id == SECOND_BLOCK_ID) {
                            block.copy(isHardConstraint = false)
                        } else {
                            block
                        }
                    },
                ),
            )
            assertFalse(
                proof.matchesPublishedPlan(
                    store.state.value.schedule + external.copy(
                        id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    ),
                ),
            )
        }

    @Test
    fun currentScheduleRejectsExternalBlockWhoseIdentityDiffersFromFixedIdentity() = runBlocking {
        val invalid = preview().copy(
            plan = preview().plan.copy(
                blocks = preview().plan.blocks + RemoteScheduleBlock(
                    id = SECOND_BLOCK_ID,
                    isSensitive = false,
                    externalBlockId = "88888888-8888-4888-8888-888888888888",
                    title = "Forged external identity",
                    start = "2026-09-01T10:00:00+02:00",
                    end = "2026-09-01T10:30:00+02:00",
                    sessionIndex = 0,
                    kind = "external_fixed",
                    explanations = emptyList(),
                ),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(invalid)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleRejectsManualExternalConflictWithSplitIdentity() = runBlocking {
        val invalid = preview().copy(
            manualPlacementAssessments = listOf(
                RemoteManualPlacementAssessment(
                    placementId = "88888888-8888-4888-8888-888888888888",
                    environmentDigest = "sha256:${"b".repeat(64)}",
                    approvalDigest = "sha256:${"c".repeat(64)}",
                    approvalRequired = false,
                    violations = listOf(
                        RemoteManualPlacementViolation(
                            code = "immutable_overlap",
                            itemIds = listOf(TASK_ID),
                            occurrenceIds = emptyList(),
                            conflictingBlockIds = listOf(SECOND_BLOCK_ID),
                            conflictingBlocks = listOf(
                                RemoteManualPlacementConflict(
                                    blockId = SECOND_BLOCK_ID,
                                    externalBlockId = THIRD_BLOCK_ID,
                                    kind = "external_fixed",
                                    start = "2026-09-01T09:15:00+02:00",
                                    end = "2026-09-01T09:45:00+02:00",
                                ),
                            ),
                            start = "2026-09-01T09:00:00+02:00",
                            end = "2026-09-01T10:00:00+02:00",
                            message = "Forged external identity",
                        ),
                    ),
                ),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(invalid)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleRejectsPlannedBlockOverlappingAnyEarlierBlock() = runBlocking {
        val invalid = preview().copy(
            plan = preview().plan.copy(
                blocks = preview().plan.blocks + preview().plan.blocks.single().copy(
                    id = SECOND_BLOCK_ID,
                    start = "2026-09-01T09:30:00+02:00",
                    end = "2026-09-01T10:30:00+02:00",
                    sessionIndex = 1,
                ),
                score = RemotePlanScore(120, 0, 0uL, 0),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(invalid)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleProofSealsNonExecutableCalendarContextWithoutMintingAuthority() =
        runBlocking {
            val calendarItem = remoteItem(split = false, isSensitive = true).copy(
                id = CALENDAR_ITEM_ID,
                kind = "event",
                title = "Shared calendar context",
                durationSeconds = 1_800,
                isExecutable = false,
                revision = 3,
            )
            val replicated = preview().copy(
                sourceItemCount = 2,
                sourceItemRevisions = mapOf(TASK_ID to 7, CALENDAR_ITEM_ID to 3),
                acceptedItemCount = 2,
                plan = preview().plan.copy(
                    blocks = preview().plan.blocks + RemoteScheduleBlock(
                        id = CALENDAR_BLOCK_ID,
                        isSensitive = true,
                        itemId = CALENDAR_ITEM_ID,
                        title = calendarItem.title,
                        start = "2026-09-01T10:30:00+02:00",
                        end = "2026-09-01T11:00:00+02:00",
                        sessionIndex = 0,
                        kind = "calendar_event",
                        explanations = emptyList(),
                    ),
                ),
            )
            val transport = FakeCanonicalTransport().apply {
                currentScheduleResult = currentSchedule(replicated)
                pages[null] = RemoteItemDeltaPage(
                    listOf(
                        RemoteItemDeltaChange(type = "upsert", item = remoteItem()),
                        RemoteItemDeltaChange(type = "upsert", item = calendarItem),
                    ),
                    "cursor-1",
                    false,
                )
            }
            val store = PlannerStore(DayWeaveUiState())

            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(store, transport).refreshCurrentPublishedSchedule(),
            )

            val context = store.state.value.schedule.single { it.id == CALENDAR_BLOCK_ID }
            assertEquals(null, context.canonicalItemId)
            assertEquals(null, context.canonicalRevision)
            assertEquals(null, context.occurrenceId)
            assertFalse(store.state.value.hasPublishedExecutionAuthority(context))
            val proof = requireNotNull(store.state.value.publishedScheduleProof)
            assertEquals(null, proof.blocks.single { it.id == CALENDAR_BLOCK_ID }.itemId)
            assertTrue(proof.matchesPublishedPlan(store.state.value.schedule))
            assertFalse(
                proof.matchesPublishedPlan(
                    store.state.value.schedule.map { block ->
                        if (block.id == CALENDAR_BLOCK_ID) {
                            block.copy(title = "Tampered context")
                        } else {
                            block
                        }
                    },
                ),
            )
        }

    @Test
    fun currentScheduleRejectsViolationOccurrenceNotLinkedToItsItemEvidence() = runBlocking {
        val recurring = remoteItem().copy(recurrence = dailyRecurrence())
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val invalid = preview().copy(
            plan = preview().plan.copy(
                blocks = listOf(
                    preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                ),
                occurrences = listOf(occurrence),
                violations = listOf(
                    RemotePlanViolation(
                        kind = "soft_constraint",
                        severity = "warning",
                        itemIds = emptyList(),
                        occurrenceIds = listOf(OCCURRENCE_ID),
                        start = "2026-09-01T09:00:00+02:00",
                        end = "2026-09-01T10:00:00+02:00",
                        penalty = 1uL,
                        message = "Invalid unowned occurrence evidence",
                    ),
                ),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(invalid)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurring)),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.state.value)
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleRejectsManualBoundaryFieldsThatDoNotMatchViolationKind() = runBlocking {
        val invalid = preview().copy(
            manualPlacementAssessments = listOf(
                RemoteManualPlacementAssessment(
                    placementId = "88888888-8888-4888-8888-888888888888",
                    environmentDigest = "sha256:${"b".repeat(64)}",
                    approvalDigest = "sha256:${"c".repeat(64)}",
                    approvalRequired = true,
                    violations = listOf(
                        RemoteManualPlacementViolation(
                            code = "earliest_start",
                            itemIds = listOf(TASK_ID),
                            occurrenceIds = emptyList(),
                            conflictingBlockIds = emptyList(),
                            conflictingBlocks = emptyList(),
                            start = "2026-09-01T09:00:00+02:00",
                            end = "2026-09-01T10:00:00+02:00",
                            boundaryStart = "2026-09-01T09:30:00+02:00",
                            boundaryEnd = "2026-09-01T10:30:00+02:00",
                            message = "Earliest start must have only a start boundary",
                        ),
                    ),
                ),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(invalid)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleAcceptsAuthorizedManualPlacementWithRetainedViolations() = runBlocking {
        val authorized = preview().copy(
            manualPlacementAssessments = listOf(
                RemoteManualPlacementAssessment(
                    placementId = "88888888-8888-4888-8888-888888888888",
                    environmentDigest = "sha256:${"b".repeat(64)}",
                    approvalDigest = "sha256:${"c".repeat(64)}",
                    approvalRequired = false,
                    violations = listOf(
                        RemoteManualPlacementViolation(
                            code = "outside_availability",
                            itemIds = listOf(TASK_ID),
                            occurrenceIds = emptyList(),
                            conflictingBlockIds = emptyList(),
                            conflictingBlocks = emptyList(),
                            start = "2026-09-01T09:00:00+02:00",
                            end = "2026-09-01T10:00:00+02:00",
                            message = "Authorized placement remains outside availability",
                        ),
                    ),
                ),
            ),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(authorized)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(1uL, store.state.value.publishedScheduleRevision?.revisionNumber)
        assertTrue(
            requireNotNull(store.state.value.publishedScheduleProof)
                .matchesPublishedPlan(store.state.value.schedule),
        )
    }

    @Test
    fun currentScheduleRejectsMoreThanSixtyFourManualPlacementAssessments() = runBlocking {
        val invalid = preview().copy(
            manualPlacementAssessments = (0..64).map { index ->
                RemoteManualPlacementAssessment(
                    placementId = UUID.nameUUIDFromBytes(
                        "manual-placement-$index".toByteArray(),
                    ).toString(),
                    environmentDigest = "sha256:${"b".repeat(64)}",
                    approvalDigest = "sha256:${"c".repeat(64)}",
                    approvalRequired = false,
                    violations = emptyList(),
                )
            },
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(invalid)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(DayWeaveUiState(), store.durableState.value)
    }

    @Test
    fun currentScheduleRejectsAggregateManualEvidenceBeyondGlobalCaps() = runBlocking {
        val baseViolation = RemoteManualPlacementViolation(
            code = "outside_availability",
            itemIds = listOf(TASK_ID),
            occurrenceIds = emptyList(),
            conflictingBlockIds = emptyList(),
            conflictingBlocks = emptyList(),
            start = "2026-09-01T09:00:00+02:00",
            end = "2026-09-01T10:00:00+02:00",
            message = "Outside availability",
        )
        val aggregateViolationOverflow = preview().copy(
            manualPlacementAssessments = listOf(
                RemoteManualPlacementAssessment(
                    placementId = "88888888-8888-4888-8888-888888888888",
                    environmentDigest = "sha256:${"b".repeat(64)}",
                    approvalDigest = "sha256:${"c".repeat(64)}",
                    approvalRequired = true,
                    violations = List(4_096) { baseViolation },
                ),
                RemoteManualPlacementAssessment(
                    placementId = "99999999-9999-4999-8999-999999999999",
                    environmentDigest = "sha256:${"d".repeat(64)}",
                    approvalDigest = "sha256:${"e".repeat(64)}",
                    approvalRequired = true,
                    violations = listOf(baseViolation),
                ),
            ),
        )
        val conflicts = (0..4_096).map { index ->
            val id = UUID.nameUUIDFromBytes("manual-conflict-$index".toByteArray()).toString()
            RemoteManualPlacementConflict(
                blockId = id,
                externalBlockId = id,
                kind = "external_fixed",
                start = "2026-09-01T09:15:00+02:00",
                end = "2026-09-01T09:45:00+02:00",
            )
        }
        val aggregateConflictOverflow = preview().copy(
            manualPlacementAssessments = listOf(
                RemoteManualPlacementAssessment(
                    placementId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    environmentDigest = "sha256:${"b".repeat(64)}",
                    approvalDigest = "sha256:${"c".repeat(64)}",
                    approvalRequired = true,
                    violations = listOf(
                        baseViolation.copy(
                            code = "immutable_overlap",
                            conflictingBlockIds = conflicts.map { it.blockId },
                            conflictingBlocks = conflicts,
                        ),
                    ),
                ),
            ),
        )

        listOf(aggregateViolationOverflow, aggregateConflictOverflow).forEach { invalid ->
            val store = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                currentScheduleResult = currentSchedule(invalid)
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-1",
                    false,
                )
            }
            assertEquals(
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
                manager(store, transport).refreshCurrentPublishedSchedule(),
            )
            assertEquals(DayWeaveUiState(), store.durableState.value)
        }
    }

    @Test
    fun currentScheduleAcceptsMoreThanTwoThousandBoundedBlocks() = runBlocking {
        val externalBlocks = (0 until 2_000).map { index ->
            val id = UUID.nameUUIDFromBytes("external-fixed-$index".toByteArray()).toString()
            RemoteScheduleBlock(
                id = id,
                isSensitive = false,
                externalBlockId = id,
                title = "External hold $index",
                start = "2026-09-01T10:00:00+02:00",
                end = "2026-09-01T10:15:00+02:00",
                sessionIndex = 0,
                kind = "external_fixed",
                explanations = emptyList(),
            )
        }
        val replicated = preview().copy(
            plan = preview().plan.copy(blocks = preview().plan.blocks + externalBlocks),
        )
        val store = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(replicated)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-large-plan",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(store, transport).refreshCurrentPublishedSchedule(),
        )
        assertEquals(2_001, store.state.value.schedule.size)
        assertTrue(
            requireNotNull(store.state.value.publishedScheduleProof)
                .hasCurrentImmutablePlanSeal(),
        )
    }

    @Test
    fun failedPublicationKeepsOldPlanAndRestartReplaysExactJournal() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationError = IOException("synthetic lost publication response")
        }
        val first = manager(plannerStore, transport).refreshAndCompose()

        assertEquals(CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE, first)
        val journal = requireNotNull(plannerStore.state.value.pendingSchedulePublication)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
        assertEquals(null, plannerStore.state.value.scheduleInputDigest)
        assertFalse(plannerStore.state.value.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
        assertEquals(1, transport.publicationRequests.size)
        val exactRequest = transport.publicationRequests.single()
        val exactPublishedSchedule = Json.decodeFromString<SchedulePublishRequest>(
            exactRequest.bodyJson,
        ).schedule
        assertEquals("2026-08-31T22:00:00Z", exactPublishedSchedule.horizonStart)
        assertEquals("2026-09-07T22:00:00Z", exactPublishedSchedule.horizonEnd)
        assertEquals(7, exactPublishedSchedule.availability.size)

        transport.publicationError = null
        val restarted = PlannerStore(plannerStore.state.value)
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(restarted, transport).refreshAndCompose(),
        )

        assertEquals(3, transport.publicationRequests.size)
        assertEquals(exactRequest, transport.publicationRequests[1])
        assertTrue(transport.publicationRequests[2] != exactRequest)
        assertEquals(journal.idempotencyKey, Json.decodeFromString<SchedulePublishRequest>(
            transport.publicationRequests[1].bodyJson,
        ).idempotencyKey)
        assertTrue(
            Json.decodeFromString<SchedulePublishRequest>(transport.publicationRequests[2].bodyJson)
                .idempotencyKey != journal.idempotencyKey,
        )
        assertEquals(null, restarted.state.value.pendingSchedulePublication)
        assertEquals("cursor-1", restarted.state.value.canonicalDeltaCursor)
        assertEquals(preview().inputDigest, restarted.state.value.scheduleInputDigest)
        assertNotNull(restarted.state.value.publishedScheduleRevision)
        assertTrue(restarted.state.value.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
    }

    @Test
    fun publicationResponseMismatchRetainsExactJournalAndNeverAdvancesCursor() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationHandler = { request ->
                val decoded = Json.decodeFromString<SchedulePublishRequest>(request.bodyJson)
                RemoteSchedulePublishResponse(
                    revision = RemotePublishedScheduleRevision(
                        id = "77777777-7777-4777-8777-777777777777",
                        revision = "1:77777777-7777-4777-8777-777777777777",
                        revisionNumber = 1uL,
                        inputDigest = "sha256:${"f".repeat(64)}",
                        horizonStart = decoded.schedule.horizonStart,
                        horizonEnd = decoded.schedule.horizonEnd,
                        timezoneName = decoded.schedule.timezoneName,
                        publishedAt = decoded.schedule.asOf,
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        assertNotNull(plannerStore.state.value.pendingSchedulePublication)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
        assertEquals(null, plannerStore.state.value.publishedScheduleRevision)
    }

    @Test
    fun typedStalePublicationIsDurablyDiscardedThenFreshlyPublishedWithANewKey() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        var calls = 0
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationHandler = { request ->
                calls += 1
                if (calls == 1) throw PlannerApiException.SchedulePublicationStale()
                publicationResponse(request, replayed = false)
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshAndCompose(),
        )

        assertEquals(2, transport.publicationRequests.size)
        val first = Json.decodeFromString<SchedulePublishRequest>(
            transport.publicationRequests[0].bodyJson,
        )
        val second = Json.decodeFromString<SchedulePublishRequest>(
            transport.publicationRequests[1].bodyJson,
        )
        assertTrue(first.idempotencyKey != second.idempotencyKey)
        assertEquals(2, transport.previewRequests.size)
        assertEquals(null, plannerStore.state.value.pendingSchedulePublication)
        assertNotNull(plannerStore.state.value.publishedScheduleRevision)
        assertTrue(plannerStore.state.value.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
    }

    @Test
    fun genericPublicationConflictRetainsExactJournalForExplicitRecovery() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationError = PlannerApiException.Conflict()
        }

        assertEquals(
            CanonicalRefreshOutcome.STALE_REVISION,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        val journal = requireNotNull(plannerStore.state.value.pendingSchedulePublication)
        val restarted = PlannerStore(plannerStore.state.value)
        assertEquals(
            CanonicalRefreshOutcome.STALE_REVISION,
            manager(restarted, transport).refreshAndCompose(),
        )
        assertEquals(2, transport.publicationRequests.size)
        assertEquals(transport.publicationRequests[0], transport.publicationRequests[1])
        assertEquals(journal, restarted.state.value.pendingSchedulePublication)
    }

    @Test
    fun stalePublicationQuarantinePersistenceFailureNeverSendsFreshOrInstallsCandidate() =
        runBlocking {
            val initial = DayWeaveUiState()
            var durable = initial
            var failSaves = false
            val repository = object : PlannerStateRepository {
                override suspend fun load(): DayWeaveUiState = durable

                override suspend fun save(state: DayWeaveUiState) {
                    if (failSaves) throw IOException("synthetic stale-journal clear failure")
                    durable = state
                }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val plannerStore = PlannerStore(initial, repository, scope)
                withTimeout(3_000) {
                    plannerStore.loadState.first { it == PlannerLoadState.READY }
                }
                val transport = FakeCanonicalTransport().apply {
                    pages[null] = RemoteItemDeltaPage(
                        listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                        "cursor-1",
                        false,
                    )
                    previewResult = preview()
                    publicationHandler = {
                        failSaves = true
                        throw PlannerApiException.SchedulePublicationStale()
                    }
                }

                assertEquals(
                    CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
                    manager(plannerStore, transport).refreshAndCompose(),
                )
                assertEquals(1, transport.publicationRequests.size)
                assertNotNull(durable.pendingSchedulePublication)
                assertTrue(plannerStore.state.value.schedule.isEmpty())
                assertEquals(null, plannerStore.state.value.scheduleInputDigest)
                assertEquals(PlannerLoadState.PERSISTENCE_FAILED, plannerStore.loadState.value)
            } finally {
                scope.cancel()
            }
        }

    @Test
    fun supersededExactReplayNeverInstallsOldCandidateOrMarksItCurrent() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationError = IOException("synthetic lost first response")
        }
        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        val exact = transport.publicationRequests.single()
        val oldCandidate = requireNotNull(
            plannerStore.state.value.pendingSchedulePublication,
        ).candidate
        val newer = remoteItem().copy(
            title = "Newer canonical work",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.pages[null] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = newer)),
            "cursor-2",
            false,
        )
        transport.previewResult = scheduledPreview(newer).copy(
            inputDigest = "sha256:${"c".repeat(64)}",
        )
        transport.publicationError = null
        transport.publicationHandler = { request ->
            if (request == exact) {
                publicationResponse(
                    request,
                    replayed = true,
                    publishedAt = clock.minusSeconds(60).toString(),
                )
            } else {
                throw IOException("synthetic lost fresh publication response")
            }
        }

        val restarted = PlannerStore(plannerStore.state.value)
        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(restarted, transport).refreshAndCompose(),
        )

        assertEquals(3, transport.publicationRequests.size)
        assertEquals(exact, transport.publicationRequests[1])
        assertTrue(transport.publicationRequests[2] != exact)
        assertTrue(oldCandidate.schedule.isNotEmpty())
        assertTrue(restarted.state.value.schedule.isEmpty())
        assertEquals(null, restarted.state.value.publishedScheduleRevision)
        assertEquals(null, restarted.state.value.scheduleInputDigest)
        assertNotNull(restarted.state.value.pendingSchedulePublication)
        assertFalse(restarted.state.value.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
    }

    @Test
    fun replayResolutionPersistenceFailureKeepsDurableJournalAndNeverInstallsCandidate() =
        runBlocking {
            val initial = DayWeaveUiState()
            var durable = initial
            var failSaves = false
            val repository = object : PlannerStateRepository {
                override suspend fun load(): DayWeaveUiState = durable

                override suspend fun save(state: DayWeaveUiState) {
                    if (failSaves) throw IOException("synthetic replay-resolution save failure")
                    durable = state
                }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val plannerStore = PlannerStore(initial, repository, scope)
                withTimeout(3_000) {
                    plannerStore.loadState.first { it == PlannerLoadState.READY }
                }
                val transport = FakeCanonicalTransport().apply {
                    pages[null] = RemoteItemDeltaPage(
                        listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                        "cursor-1",
                        false,
                    )
                    previewResult = preview()
                    publicationError = IOException("synthetic lost first response")
                }
                assertEquals(
                    CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
                    manager(plannerStore, transport).refreshAndCompose(),
                )
                val durableJournal = requireNotNull(durable.pendingSchedulePublication)
                failSaves = true
                transport.publicationError = null

                assertEquals(
                    CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
                    manager(plannerStore, transport).refreshAndCompose(),
                )
                assertEquals(2, transport.publicationRequests.size)
                assertEquals(transport.publicationRequests[0], transport.publicationRequests[1])
                assertEquals(durableJournal, durable.pendingSchedulePublication)
                assertTrue(plannerStore.state.value.schedule.isEmpty())
                assertEquals(null, plannerStore.state.value.publishedScheduleRevision)
                assertEquals(null, plannerStore.state.value.scheduleInputDigest)
                assertEquals(PlannerLoadState.PERSISTENCE_FAILED, plannerStore.loadState.value)
            } finally {
                scope.cancel()
            }
        }

    @Test
    fun pendingPublicationCannotBeRetargetedToAnotherConfiguration() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationError = IOException("synthetic ambiguous publication")
        }
        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        val sent = transport.publicationRequests.single()
        transport.publicationError = null
        val replacement = object : ApiCredentialStore {
            override fun snapshot() = ApiConnectionSnapshot(
                baseUrl = "https://api.example.test/gateway-b/",
                hasBearerToken = true,
                lastSuccessfulSyncEpochMillis = null,
                configurationId = "connection-1",
            )

            override fun authenticatedConfiguration() = AuthenticatedApiConfiguration.createBound(
                "https://api.example.test/gateway-b/",
                "replacement-secret",
                "connection-1",
            )

            override fun update(baseUrl: String, bearerToken: String?) = Unit
            override fun clear() = Unit
            override fun recordSuccessfulSync(epochMillis: Long) = Unit
        }

        assertEquals(
            CanonicalRefreshOutcome.CONFIGURATION_ERROR,
            manager(plannerStore, transport, replacement).refreshAndCompose(),
        )
        assertEquals(listOf(sent), transport.publicationRequests)
        assertNotNull(plannerStore.state.value.pendingSchedulePublication)
    }

    @Test
    fun publicationIsNeverSentWhenExactJournalCannotBeSaved() = runBlocking {
        val initial = DayWeaveUiState()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                throw IOException("synthetic encrypted publication-journal failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val plannerStore = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { plannerStore.loadState.first { it == PlannerLoadState.READY } }
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
            }

            assertEquals(
                CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
                manager(plannerStore, transport).refreshAndCompose(),
            )
            assertTrue(transport.publicationRequests.isEmpty())
            assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
            assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
            assertNotNull(plannerStore.state.value.pendingSchedulePublication)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun non200PublicationStatusesRetainEncryptedJournalAndNeverCommit() = runBlocking {
        listOf(201, 202, 204).forEach { status ->
            val plannerStore = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
                publicationError = PlannerApiException.Http(status)
            }

            assertEquals(
                CanonicalRefreshOutcome.PERMANENT_SERVER_FAILURE,
                manager(plannerStore, transport).refreshAndCompose(),
            )
            assertNotNull(plannerStore.state.value.pendingSchedulePublication)
            assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
            assertEquals(null, plannerStore.state.value.publishedScheduleRevision)
        }
    }

    @Test
    fun retryablePublicationStatusesRetainExactJournalAndNeverCommit() = runBlocking {
        listOf(408, 425, 429, 500, 503).forEach { status ->
            val plannerStore = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
                publicationError = PlannerApiException.Http(status)
            }

            assertEquals(
                CanonicalRefreshOutcome.RETRYABLE_SERVER_FAILURE,
                manager(plannerStore, transport).refreshAndCompose(),
            )
            assertNotNull(plannerStore.state.value.pendingSchedulePublication)
            assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
            assertEquals(null, plannerStore.state.value.publishedScheduleRevision)
        }
    }

    @Test
    fun oldExactReplayReceiptIsResolvedWithoutReceiveTimeFreshness() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationError = IOException("synthetic lost old response")
        }
        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        val oldJournal = requireNotNull(plannerStore.state.value.pendingSchedulePublication)
        transport.publicationError = null
        transport.deltaError = IOException("synthetic fresh pull unavailable")
        val restarted = PlannerStore(plannerStore.state.value)
        val muchLater = CanonicalSyncManager(
            plannerStore = restarted,
            credentialStore = CanonicalCredentialStore(),
            transport = transport,
            now = { clock.plusSeconds(30L * 24L * 60L * 60L) },
            zoneId = { ZoneId.of("Europe/Madrid") },
        )

        assertEquals(CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE, muchLater.refreshAndCompose())
        assertEquals(2, transport.publicationRequests.size)
        assertEquals(transport.publicationRequests[0], transport.publicationRequests[1])
        assertEquals(null, restarted.state.value.pendingSchedulePublication)
        assertEquals(null, restarted.state.value.scheduleInputDigest)
        assertEquals(null, restarted.state.value.publishedScheduleRevision)
        assertTrue(restarted.state.value.schedule.isEmpty())
        assertTrue(oldJournal.candidate.schedule.isNotEmpty())
    }

    @Test
    fun newKeyCurrentRevisionDedupeAcceptsOldPublishedAtWhenNotMarkedReplay() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationHandler = { request ->
                val decoded = Json.decodeFromString<SchedulePublishRequest>(request.bodyJson)
                RemoteSchedulePublishResponse(
                    revision = RemotePublishedScheduleRevision(
                        id = "77777777-7777-4777-8777-777777777777",
                        revision = "7:77777777-7777-4777-8777-777777777777",
                        revisionNumber = 7uL,
                        inputDigest = decoded.expectedInputDigest,
                        horizonStart = decoded.schedule.horizonStart,
                        horizonEnd = decoded.schedule.horizonEnd,
                        timezoneName = decoded.schedule.timezoneName,
                        publishedAt = "2020-01-01T00:00:00Z",
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        assertEquals(null, plannerStore.state.value.pendingSchedulePublication)
        assertEquals(
            "2020-01-01T00:00:00Z",
            plannerStore.state.value.publishedScheduleRevision?.publishedAt,
        )
        assertEquals(preview().inputDigest, plannerStore.state.value.scheduleInputDigest)
    }

    @Test
    fun publicationTimestampBeyondBoundedFutureSkewKeepsExactJournal() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
            publicationHandler = { request ->
                val decoded = Json.decodeFromString<SchedulePublishRequest>(request.bodyJson)
                RemoteSchedulePublishResponse(
                    revision = RemotePublishedScheduleRevision(
                        id = "77777777-7777-4777-8777-777777777777",
                        revision = "8:77777777-7777-4777-8777-777777777777",
                        revisionNumber = 8uL,
                        inputDigest = decoded.expectedInputDigest,
                        horizonStart = decoded.schedule.horizonStart,
                        horizonEnd = decoded.schedule.horizonEnd,
                        timezoneName = decoded.schedule.timezoneName,
                        publishedAt = clock.plusSeconds(5L * 60L + 1L).toString(),
                    ),
                    replayed = false,
                )
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        assertNotNull(plannerStore.state.value.pendingSchedulePublication)
        assertEquals(null, plannerStore.state.value.publishedScheduleRevision)
        assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
    }

    @Test
    fun delayedOldBindingPlanCannotRepopulateAfterGenerationFence() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val credentials = GenerationBoundCredentialStore()
        val responseStarted = CompletableDeferred<Unit>()
        val releaseResponse = CompletableDeferred<Unit>()
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "old-binding-cursor",
                hasMore = false,
            )
            previewResult = preview()
            deltaStarted = responseStarted
            deltaGate = releaseResponse
        }
        val manager = manager(plannerStore, transport, credentials)

        val oldRequest = async { manager.refreshAndCompose() }
        withTimeout(3_000) { responseStarted.await() }
        val fence = async {
            credentials.invalidateBeforeQuarantine {
                val cleared = plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
                if (cleared) manager.quarantineBindingState()
                cleared
            }
        }
        yield()
        releaseResponse.complete(Unit)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, withTimeout(3_000) { oldRequest.await() })
        assertTrue(withTimeout(3_000) { fence.await() })
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        assertEquals(null, plannerStore.state.value.canonicalSyncOrigin)
        assertEquals(CanonicalSyncPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun readerCreatedDuringWriterCannotSendOrRestoreOldCanonicalPlan() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val credentials = GenerationBoundCredentialStore()
        val writerEntered = CompletableDeferred<Unit>()
        val releaseWriter = CompletableDeferred<Unit>()
        val configurationObserved = CompletableDeferred<Unit>()
        credentials.configurationObserved = { configurationObserved.complete(Unit) }
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "stale-cursor",
                hasMore = false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport, credentials)

        val fence = async {
            credentials.invalidateBeforeQuarantine {
                writerEntered.complete(Unit)
                releaseWriter.await()
                val cleared = plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
                if (cleared) manager.quarantineBindingState()
                cleared
            }
        }
        withTimeout(3_000) { writerEntered.await() }
        val refresh = async { manager.refreshAndCompose() }
        withTimeout(3_000) { configurationObserved.await() }

        assertTrue(credentials.enabled)
        assertTrue(transport.deltaCursors.isEmpty())
        releaseWriter.complete(Unit)

        assertTrue(withTimeout(3_000) { fence.await() })
        assertEquals(CanonicalRefreshOutcome.NOT_CONFIGURED, withTimeout(3_000) { refresh.await() })
        assertTrue(transport.deltaCursors.isEmpty())
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        assertEquals(CanonicalSyncPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun refreshPersistsLosslessCanonicalItemsAndComposedTimeline() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "cursor-1",
                hasMore = false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)

        val outcome = manager.refreshAndCompose()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
        assertEquals(CanonicalSyncPhase.CONNECTED, manager.state.value.phase)
        assertEquals("cursor-1", plannerStore.state.value.canonicalDeltaCursor)
        assertEquals("https://api.example.test/", plannerStore.state.value.canonicalSyncOrigin)
        val canonical = plannerStore.state.value.canonicalItems.single()
        assertEquals(TASK_ID, canonical.id)
        assertEquals(7L, canonical.revision)
        assertTrue(canonical.flexibleConstraintsJson.contains("required_contexts"))
        assertTrue(canonical.splitPolicyJson.contains("minimum_chunk_seconds"))

        val block = plannerStore.state.value.schedule.single()
        assertEquals(BLOCK_ID, block.id)
        assertEquals(TASK_ID, block.canonicalItemId)
        assertEquals(7L, block.canonicalRevision)
        assertEquals(9 * 60, block.startMinute)
        assertEquals(60, block.durationMinutes)
        assertEquals(ItemKind.TASK, block.kind)
        assertEquals(ItemStatus.SCHEDULED, block.status)
        assertTrue(block.isSplittable)
        assertEquals("", block.note)
        assertEquals(6_240, plannerStore.state.value.protectedFreeMinutes)
        assertEquals(100, plannerStore.state.value.dayScore)

        assertNotNull(transport.previewRequest)
        val request = requireNotNull(transport.previewRequest)
        assertEquals("Europe/Madrid", request.timezoneName)
        assertEquals("2026-08-31T22:00:00Z", request.horizonStart)
        assertEquals("2026-09-07T22:00:00Z", request.horizonEnd)
        assertEquals(7, request.availability.size)
        assertEquals("2026-09-01T05:00:00Z", request.availability.first().start)
        assertEquals("2026-09-01T20:00:00Z", request.availability.first().end)
        assertEquals("2026-09-07T05:00:00Z", request.availability.last().start)
        assertEquals("2026-09-07T20:00:00Z", request.availability.last().end)
    }

    @Test
    fun configuredFirmHorizonUsesExactCalendarBoundsAndOneWindowPerDate() = runBlocking {
        val profile = ScheduleCompositionProfileSnapshot(
            firmHorizonDays = 3,
            dayStartMinute = 8 * 60,
            dayEndMinute = 18 * 60,
        )
        val plannerStore = PlannerStore(DayWeaveUiState(scheduleCompositionProfile = profile))
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview().withWindow(
                asOf = clock,
                horizonStart = "2026-08-31T22:00:00Z",
                horizonEnd = "2026-09-03T22:00:00Z",
            )
        }

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager(plannerStore, transport).refreshAndCompose())

        val request = transport.previewRequests.single()
        assertEquals("2026-08-31T22:00:00Z", request.horizonStart)
        assertEquals("2026-09-03T22:00:00Z", request.horizonEnd)
        assertEquals(3, request.availability.size)
        assertEquals(
            listOf(
                "2026-09-01T06:00:00Z" to "2026-09-01T16:00:00Z",
                "2026-09-02T06:00:00Z" to "2026-09-02T16:00:00Z",
                "2026-09-03T06:00:00Z" to "2026-09-03T16:00:00Z",
            ),
            request.availability.map { it.start to it.end },
        )
        assertEquals(1_740, plannerStore.state.value.protectedFreeMinutes)
    }

    @Test
    fun weeklyProfileOverridesDeviceZoneAndSendsSleepAndProtectedBlocks() = runBlocking {
        val profile = requireNotNull(
            ScheduleCompositionProfileSnapshot().upgradedToWeeklySchedule("Europe/Paris"),
        )
        val plannerStore = PlannerStore(DayWeaveUiState(scheduleCompositionProfile = profile))
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-weekly-profile",
                false,
            )
            previewResult = preview()
        }

        val outcome = manager(
            plannerStore = plannerStore,
            transport = transport,
            zoneProvider = { ZoneId.of("America/Los_Angeles") },
        ).refreshAndCompose()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
        val request = transport.previewRequests.single()
        assertEquals("Europe/Paris", request.timezoneName)
        assertEquals(7, request.availability.size)
        assertEquals(15, request.fixedBlocks.size)
        assertEquals(8, request.fixedBlocks.count { it.source == "sleep" })
        assertEquals(7, request.fixedBlocks.count { it.source == "protected_time" })
        assertTrue(request.fixedBlocks.all { it.isSensitive })
    }

    @Test
    fun protectedMinutesSubtractExactSubminuteBusyRangesBeforeConservativeRounding() = runBlocking {
        data class Case(
            val name: String,
            val windows: List<Pair<String, String>>,
            val expectedMinutes: Int,
        )
        val cases = listOf(
            Case(
                "fifty-nine seconds",
                listOf("2026-09-01T09:00:00Z" to "2026-09-01T09:00:59Z"),
                59,
            ),
            Case(
                "separated subminute fragments",
                listOf(
                    "2026-09-01T09:00:00Z" to "2026-09-01T09:00:30Z",
                    "2026-09-01T09:30:00Z" to "2026-09-01T09:30:30Z",
                ),
                58,
            ),
            Case(
                "abutting fragments",
                listOf(
                    "2026-09-01T09:00:00Z" to "2026-09-01T09:00:30Z",
                    "2026-09-01T09:00:30Z" to "2026-09-01T09:01:00Z",
                ),
                59,
            ),
            Case(
                "overlapping nanosecond fragments",
                listOf(
                    "2026-09-01T09:00:00.000000001Z" to
                        "2026-09-01T09:00:45.000000001Z",
                    "2026-09-01T09:00:30.000000001Z" to
                        "2026-09-01T09:01:00.000000001Z",
                ),
                58,
            ),
        )
        val itemIds = listOf(
            "11111111-1111-4111-8111-111111111111",
            "33333333-3333-4333-8333-333333333333",
        )
        val blockIds = listOf(
            "22222222-2222-4222-8222-222222222222",
            "99999999-9999-4999-8999-999999999999",
        )

        cases.forEach { case ->
            val events = case.windows.indices.map { index ->
                remoteItem(split = false).copy(
                    id = itemIds[index],
                    kind = "event",
                    title = "Busy ${index + 1}",
                    notes = null,
                    timezoneName = "UTC",
                    durationSeconds = null,
                    deadlineAt = null,
                    flexibleConstraints = buildJsonObject { },
                    isExecutable = false,
                )
            }
            val basePreview = itemsPreview(events).withWindow(
                asOf = clock,
                horizonStart = "2026-09-01T00:00:00Z",
                horizonEnd = "2026-09-02T00:00:00Z",
            )
            val preview = basePreview.copy(
                plan = basePreview.plan.copy(
                    blocks = case.windows.mapIndexed { index, (start, end) ->
                        RemoteScheduleBlock(
                            id = blockIds[index],
                            isSensitive = false,
                            itemId = itemIds[index],
                            title = events[index].title,
                            start = start,
                            end = end,
                            sessionIndex = 0,
                            kind = "calendar_event",
                            explanations = emptyList(),
                        )
                    },
                    score = RemotePlanScore(0, 0, 0uL, 0),
                ),
            )
            val store = PlannerStore(
                DayWeaveUiState(
                    scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(
                        firmHorizonDays = 1,
                        dayStartMinute = 9 * 60,
                        dayEndMinute = 10 * 60,
                    ),
                ),
            )
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    events.map { RemoteItemDeltaChange(type = "upsert", item = it) },
                    "cursor-${case.name}",
                    false,
                )
                previewResult = preview
            }

            assertEquals(
                case.name,
                CanonicalRefreshOutcome.SUCCESS,
                manager(
                    store,
                    transport,
                    zoneProvider = { ZoneId.of("UTC") },
                ).refreshAndCompose(),
            )
            assertEquals(case.name, case.expectedMinutes, store.state.value.protectedFreeMinutes)
        }
    }

    @Test
    fun sevenCalendarDayHorizonHasDstAdjustedAbsoluteDuration() = runBlocking {
        data class Case(
            val asOf: Instant,
            val horizonStart: String,
            val horizonEnd: String,
            val expectedHours: Long,
            val transitionWindowStart: String,
            val transitionWindowEnd: String,
        )
        val cases = listOf(
            Case(
                asOf = Instant.parse("2026-03-27T12:00:00Z"),
                horizonStart = "2026-03-26T23:00:00Z",
                horizonEnd = "2026-04-02T22:00:00Z",
                expectedHours = 167,
                transitionWindowStart = "2026-03-29T05:00:00Z",
                transitionWindowEnd = "2026-03-29T20:00:00Z",
            ),
            Case(
                asOf = Instant.parse("2026-10-23T12:00:00Z"),
                horizonStart = "2026-10-22T22:00:00Z",
                horizonEnd = "2026-10-29T23:00:00Z",
                expectedHours = 169,
                transitionWindowStart = "2026-10-25T06:00:00Z",
                transitionWindowEnd = "2026-10-25T21:00:00Z",
            ),
        )

        cases.forEach { case ->
            val plannerStore = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-dst", false)
                previewResult = emptyPreview().withWindow(
                    asOf = case.asOf,
                    horizonStart = case.horizonStart,
                    horizonEnd = case.horizonEnd,
                )
            }

            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(plannerStore, transport, currentInstant = case.asOf).refreshAndCompose(),
            )

            val request = transport.previewRequests.single()
            assertEquals(case.horizonStart, request.horizonStart)
            assertEquals(case.horizonEnd, request.horizonEnd)
            assertEquals(
                case.expectedHours,
                Duration.between(
                    Instant.parse(request.horizonStart),
                    Instant.parse(request.horizonEnd),
                ).toHours(),
            )
            assertEquals(7, request.availability.size)
            assertTrue(
                request.availability.any {
                    it.start == case.transitionWindowStart && it.end == case.transitionWindowEnd
                },
            )
        }
    }

    @Test
    fun nonexistentLocalAvailabilityOrHorizonBoundaryFailsClosed() = runBlocking {
        data class Case(
            val asOf: Instant,
            val zone: ZoneId,
            val profile: ScheduleCompositionProfileSnapshot,
        )
        val cases = listOf(
            Case(
                asOf = Instant.parse("2026-03-29T10:00:00Z"),
                zone = ZoneId.of("Europe/Madrid"),
                profile = ScheduleCompositionProfileSnapshot(
                    firmHorizonDays = 1,
                    dayStartMinute = 2 * 60 + 30,
                    dayEndMinute = 4 * 60,
                ),
            ),
            Case(
                asOf = Instant.parse("2026-03-08T16:00:00Z"),
                zone = ZoneId.of("America/Havana"),
                profile = ScheduleCompositionProfileSnapshot(firmHorizonDays = 1),
            ),
        )

        cases.forEach { case ->
            val initial = DayWeaveUiState(scheduleCompositionProfile = case.profile)
            val plannerStore = PlannerStore(initial)
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-gap", false)
            }

            assertEquals(
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
                manager(
                    plannerStore,
                    transport,
                    currentInstant = case.asOf,
                    zoneProvider = { case.zone },
                ).refreshAndCompose(),
            )
            assertTrue(transport.previewRequests.isEmpty())
            assertTrue(transport.publicationRequests.isEmpty())
            assertEquals(initial, plannerStore.durableState.value)
        }
    }

    @Test
    fun ambiguousAvailabilityUsesConservativeBoundaryOffsets() = runBlocking {
        suspend fun requestFor(profile: ScheduleCompositionProfileSnapshot): SchedulePreviewRequest {
            val asOf = Instant.parse("2026-10-25T12:00:00Z")
            val plannerStore = PlannerStore(DayWeaveUiState(scheduleCompositionProfile = profile))
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-overlap", false)
                previewResult = emptyPreview().withWindow(
                    asOf = asOf,
                    horizonStart = "2026-10-24T22:00:00Z",
                    horizonEnd = "2026-10-25T23:00:00Z",
                )
            }
            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(plannerStore, transport, currentInstant = asOf).refreshAndCompose(),
            )
            return transport.previewRequests.single()
        }

        val ambiguousStart = requestFor(
            ScheduleCompositionProfileSnapshot(
                firmHorizonDays = 1,
                dayStartMinute = 2 * 60 + 30,
                dayEndMinute = 3 * 60,
            ),
        ).availability.single()
        assertEquals("2026-10-25T01:30:00Z", ambiguousStart.start)
        assertEquals("2026-10-25T02:00:00Z", ambiguousStart.end)

        val ambiguousEnd = requestFor(
            ScheduleCompositionProfileSnapshot(
                firmHorizonDays = 1,
                dayStartMinute = 60,
                dayEndMinute = 2 * 60 + 30,
            ),
        ).availability.single()
        assertEquals("2026-10-24T23:00:00Z", ambiguousEnd.start)
        assertEquals("2026-10-25T00:30:00Z", ambiguousEnd.end)
    }

    @Test
    fun twentyFourHundredIsClippedAtTheEarlierFirmHorizonEnd() = runBlocking {
        val asOf = Instant.parse("2026-10-31T16:00:00Z")
        val profile = ScheduleCompositionProfileSnapshot(
            firmHorizonDays = 1,
            dayStartMinute = 23 * 60,
            dayEndMinute = 24 * 60,
        )
        val plannerStore = PlannerStore(DayWeaveUiState(scheduleCompositionProfile = profile))
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-midnight-overlap", false)
            previewResult = emptyPreview().withWindow(
                asOf = asOf,
                horizonStart = "2026-10-31T04:00:00Z",
                horizonEnd = "2026-11-01T04:00:00Z",
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(
                plannerStore,
                transport,
                currentInstant = asOf,
                zoneProvider = { ZoneId.of("America/Havana") },
            ).refreshAndCompose(),
        )

        val request = transport.previewRequests.single()
        assertEquals("2026-11-01T04:00:00Z", request.horizonEnd)
        assertEquals("2026-11-01T03:00:00Z", request.availability.single().start)
        assertEquals(request.horizonEnd, request.availability.single().end)
    }

    @Test
    fun twentyFourHundredUsesTheNextStrictStartOnAnInteriorAmbiguousMidnight() = runBlocking {
        val asOf = Instant.parse("2026-10-31T16:00:00Z")
        val profile = ScheduleCompositionProfileSnapshot(
            firmHorizonDays = 2,
            dayStartMinute = 23 * 60,
            dayEndMinute = 24 * 60,
        )
        val plannerStore = PlannerStore(DayWeaveUiState(scheduleCompositionProfile = profile))
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(emptyList(), "cursor-interior-midnight", false)
            previewResult = emptyPreview().withWindow(
                asOf = asOf,
                horizonStart = "2026-10-31T04:00:00Z",
                horizonEnd = "2026-11-02T05:00:00Z",
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(
                plannerStore,
                transport,
                currentInstant = asOf,
                zoneProvider = { ZoneId.of("America/Havana") },
            ).refreshAndCompose(),
        )

        val request = transport.previewRequests.single()
        assertEquals(2, request.availability.size)
        assertEquals("2026-11-01T03:00:00Z", request.availability.first().start)
        assertEquals("2026-11-01T05:00:00Z", request.availability.first().end)
        assertEquals("2026-11-02T04:00:00Z", request.availability.last().start)
        assertEquals("2026-11-02T05:00:00Z", request.availability.last().end)
    }

    @Test
    fun blockIntersectingDaySevenKeepsExactGeometryAndClipsOnlyItsPresentationSlice() = runBlocking {
        val crossing = preview().plan.blocks.single().copy(
            start = "2026-09-07T21:30:00Z",
            end = "2026-09-07T22:30:00Z",
            kind = "pinned",
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-day-seven",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(blocks = listOf(crossing)),
            )
        }

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager(plannerStore, transport).refreshAndCompose())

        val block = plannerStore.state.value.schedule.single()
        assertEquals(23 * 60 + 30, block.startMinute)
        assertEquals(60, block.durationMinutes)
        assertEquals("2026-09-07T21:30:00Z", block.absoluteStartAt)
        assertEquals("2026-09-07T22:30:00Z", block.absoluteEndAt)
        val slice = plannerStore.state.value.visibleScheduleSlicesForFirmHorizon(
            reference = clock,
            currentZone = ZoneId.of("Europe/Madrid"),
        ).single()
        assertEquals(30, slice.durationMinutes)
        assertEquals(Instant.parse("2026-09-07T22:00:00Z"), slice.clippedEnd)
    }

    @Test
    fun crossingPinnedGeometryAndPublicationProofAreIdenticalAfterReplicaRefresh() = runBlocking {
        val crossing = preview().plan.blocks.single().copy(
            start = "2026-09-07T21:30:00Z",
            end = "2026-09-07T22:30:00Z",
            kind = "pinned",
        )
        val crossingPreview = preview().copy(
            plan = preview().plan.copy(blocks = listOf(crossing)),
        )
        val originStore = PlannerStore(DayWeaveUiState())
        val originTransport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-origin-crossing",
                false,
            )
            previewResult = crossingPreview
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(originStore, originTransport).refreshAndCompose(),
        )
        val origin = originStore.state.value
        val replicaStore = PlannerStore(DayWeaveUiState())
        val replicaTransport = FakeCanonicalTransport().apply {
            currentScheduleResult = currentSchedule(crossingPreview)
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-replica-crossing",
                false,
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(replicaStore, replicaTransport).refreshCurrentPublishedSchedule(),
        )
        val replica = replicaStore.state.value

        assertEquals(origin.schedule, replica.schedule)
        assertEquals(origin.publishedScheduleProof?.blocks, replica.publishedScheduleProof?.blocks)
        assertTrue(origin.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
        assertTrue(replica.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
    }

    @Test
    fun automaticallyPlannedBlockCannotCrossTheExactFirmHorizon() = runBlocking {
        val crossing = preview().plan.blocks.single().copy(
            start = "2026-09-07T21:30:00Z",
            end = "2026-09-07T22:30:00Z",
            kind = "planned",
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-invalid-planned-crossing",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(blocks = listOf(crossing)),
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        assertEquals(null, plannerStore.state.value.publishedScheduleProof)
    }

    @Test
    fun nextDayComposeReusesIntersectingExactAssignmentFromPriorGeneration() = runBlocking {
        var instant = clock
        val stableBlock = preview().plan.blocks.single().copy(
            start = "2026-09-02T09:00:00+02:00",
            end = "2026-09-02T10:00:00+02:00",
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(blocks = listOf(stableBlock)),
            )
        }
        val manager = manager(plannerStore, transport, nowProvider = { instant })
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        instant = clock.plus(Duration.ofDays(1))
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
        transport.previewResult = preview().copy(
            plan = preview().plan.copy(blocks = listOf(stableBlock)),
        ).withWindow(
            asOf = instant,
            horizonStart = "2026-09-01T22:00:00Z",
            horizonEnd = "2026-09-08T22:00:00Z",
        )

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        val assignment = transport.previewRequests.last().previousAssignments.single()
        assertEquals("2026-09-02T07:00:00Z", assignment.blocks.single().start)
        assertEquals("2026-09-02T08:00:00Z", assignment.blocks.single().end)
        assertTrue(assignment.pinned)
    }

    @Test
    fun sensitivitySurvivesWireCachePreviewAndFullReplacement() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val sensitiveItem = remoteItem(split = false, isSensitive = true).copy(
            title = "SYNTHETIC-ANDROID-SENSITIVE-CANARY",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = sensitiveItem)),
                nextCursor = "sensitive-cursor-1",
                hasMore = false,
            )
            previewResult = preview(isSensitive = true).copy(
                plan = preview(isSensitive = true).plan.copy(
                    blocks = listOf(
                        preview(isSensitive = true).plan.blocks.single().copy(
                            title = sensitiveItem.title,
                        ),
                    ),
                ),
            )
        }
        val manager = manager(plannerStore, transport)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertTrue(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertTrue(plannerStore.state.value.schedule.single().isSensitive)

        transport.replacementResult = sensitiveItem.copy(
            status = "in_progress",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.start(BLOCK_ID))
        assertTrue(requireNotNull(transport.replacementRequest).item.isSensitive)
        assertTrue(plannerStore.state.value.canonicalItems.single().isSensitive)
    }

    @Test
    fun sensitivityAuthoringUsesFullRevisionGuardAndUpdatesCachedBlocks() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false, isSensitive = false)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                nextCursor = "sensitivity-authoring-cursor",
                hasMore = false,
            )
            previewResult = preview(isSensitive = false)
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.replacementResult = initial.copy(
            isSensitive = true,
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager.setItemSensitivity(TASK_ID, expectedRevision = 7, isSensitive = true),
        )

        val promotion = requireNotNull(transport.replacementRequest)
        assertEquals(7L, promotion.expectedRevision)
        assertEquals(initial.status, promotion.item.status)
        assertTrue(promotion.item.isSensitive)
        assertEquals(initial.title, promotion.item.title)
        assertEquals(initial.flexibleConstraints, promotion.item.flexibleConstraints)
        assertEquals(initial.splitPolicy, promotion.item.splitPolicy)
        assertTrue(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertTrue(plannerStore.state.value.schedule.single().isSensitive)
        assertEquals(8L, plannerStore.state.value.schedule.single().canonicalRevision)
        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)

        transport.replacementResult = transport.replacementResult?.copy(
            isSensitive = false,
            revision = 9,
            updatedAt = "2026-09-01T07:02:00Z",
        )
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager.setItemSensitivity(TASK_ID, expectedRevision = 8, isSensitive = false),
        )
        assertFalse(requireNotNull(transport.replacementRequest).item.isSensitive)
        assertFalse(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertFalse(plannerStore.state.value.schedule.single().isSensitive)
    }

    @Test
    fun lostSensitivityResponseReplaysExactRequestAndNeverInventsLocalSuccess() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false, isSensitive = false)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                nextCursor = "sensitivity-lost-response-cursor",
                hasMore = false,
            )
            previewResult = preview(isSensitive = false)
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.replacementError = IOException("synthetic response loss")

        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.setItemSensitivity(TASK_ID, expectedRevision = 7, isSensitive = true),
        )
        val pending = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        val exactBody = pending.replacementRequestJson
        assertTrue(pending.targetIsSensitive)
        assertFalse(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertTrue(plannerStore.state.value.schedule.single().isSensitive)
        assertTrue(
            effectiveCanonicalSensitivity(
                plannerStore.state.value.canonicalItems,
                TASK_ID,
                pending,
            ),
        )

        val applied = initial.copy(
            isSensitive = true,
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.replacementError = PlannerApiException.Conflict()
        transport.queuedPages.getOrPut("sensitivity-lost-response-cursor", ::ArrayDeque).apply {
            add(
                RemoteItemDeltaPage(
                    changes = listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
                    nextCursor = "sensitivity-applied-cursor",
                    hasMore = false,
                ),
            )
            add(
                RemoteItemDeltaPage(
                    changes = listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
                    nextCursor = "sensitivity-applied-cursor",
                    hasMore = false,
                ),
            )
        }
        transport.previewResult = preview(isSensitive = true).copy(
            sourceItemRevisions = mapOf(TASK_ID to 8L),
        )

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
        assertTrue(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertTrue(plannerStore.state.value.schedule.single().isSensitive)
        assertTrue(transport.replacementIdempotencyKeys.size >= 2)
        assertTrue(
            transport.replacementIdempotencyKeys.all { it == pending.idempotencyKey },
        )
        assertEquals(transport.replacementRequests.first(), transport.replacementRequests.last())
        assertEquals(exactBody, pending.replacementRequestJson)
    }

    @Test
    fun ambiguousSensitivityRemovalNeverDeclassifiesBeforeConfirmation() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false, isSensitive = true)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                nextCursor = "sensitivity-removal-cursor",
                hasMore = false,
            )
            previewResult = preview(isSensitive = true)
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.replacementError = IOException("synthetic removal response loss")

        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.setItemSensitivity(TASK_ID, expectedRevision = 7, isSensitive = false),
        )

        val pending = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        assertFalse(pending.targetIsSensitive)
        assertTrue(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertTrue(plannerStore.state.value.schedule.single().isSensitive)
    }

    @Test
    fun staleReviewedRevisionCannotConstructSensitivityReplacement() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false, isSensitive = true)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                nextCursor = "sensitivity-reviewed-revision-cursor",
                hasMore = false,
            )
            previewResult = preview(isSensitive = true)
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.setItemSensitivity(TASK_ID, expectedRevision = 6, isSensitive = false),
        )
        assertEquals(null, transport.replacementRequest)
        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
        assertTrue(plannerStore.state.value.canonicalItems.single().isSensitive)
        assertTrue(plannerStore.state.value.schedule.single().isSensitive)
    }

    @Test
    fun previewSensitivityDowngradeIsRejectedWithoutReplacingEncryptedPlan() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(false))),
                nextCursor = "sensitivity-baseline-cursor",
                hasMore = false,
            )
            previewResult = preview(isSensitive = false)
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val baseline = plannerStore.state.value

        val promoted = remoteItem(split = false, isSensitive = true).copy(
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
            title = "SYNTHETIC-SENSITIVITY-DOWNGRADE-ANDROID",
        )
        transport.pages["sensitivity-baseline-cursor"] = RemoteItemDeltaPage(
            changes = listOf(RemoteItemDeltaChange(type = "upsert", item = promoted)),
            nextCursor = "sensitivity-promoted-cursor",
            hasMore = false,
        )
        transport.previewResult = preview(isSensitive = false).copy(
            sourceItemRevisions = mapOf(TASK_ID to 8L),
            plan = preview(isSensitive = false).plan.copy(
                blocks = listOf(
                    preview(isSensitive = false).plan.blocks.single().copy(title = promoted.title),
                ),
            ),
        )

        assertEquals(CanonicalRefreshOutcome.PROTOCOL_FAILURE, manager.refreshAndCompose())
        assertEquals(baseline.schedule, plannerStore.state.value.schedule)
        assertEquals(baseline.canonicalItems, plannerStore.state.value.canonicalItems)
        assertEquals("sensitivity-baseline-cursor", plannerStore.state.value.canonicalDeltaCursor)
    }

    @Test
    fun credentialRotationQuarantinesCanonicalTenantCacheBeforeChange() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "cursor-1",
                hasMore = false,
            )
            previewResult = preview()
        }
        val barrierEvents = mutableListOf<String>()
        val manager = manager(
            plannerStore = plannerStore,
            transport = transport,
            cancelTimedBreakNotification = {
                barrierEvents += "cancel"
                true
            },
            reconcileTimedBreakNotification = { barrierEvents += "reconcile" },
        )
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertTrue(plannerStore.state.value.canonicalItems.isNotEmpty())

        var changed = false
        manager.withConfigurationUpdateLock(
            requestedBaseUrl = "https://api.example.test/",
            bearerToken = "replacement-secret",
        ) { changed = true }

        assertTrue(changed)
        assertEquals(listOf("cancel", "reconcile"), barrierEvents)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
        assertEquals(null, plannerStore.state.value.canonicalExecutionSyncOrigin)
    }

    @Test
    fun failedNotificationCancellationBlocksCredentialRotationBeforeQuarantine() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "cursor-1",
                hasMore = false,
            )
            previewResult = preview()
        }
        val barrierEvents = mutableListOf<String>()
        val manager = manager(
            plannerStore = plannerStore,
            transport = transport,
            cancelTimedBreakNotification = {
                barrierEvents += "cancel-failed"
                false
            },
            reconcileTimedBreakNotification = { barrierEvents += "reconcile" },
        )
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val before = plannerStore.state.value
        var changed = false

        val failure = runCatching {
            manager.withConfigurationUpdateLock(
                requestedBaseUrl = "https://api.example.test/",
                bearerToken = "replacement-secret",
            ) { changed = true }
        }.exceptionOrNull()

        assertTrue(failure != null)
        assertFalse(changed)
        assertEquals(before, plannerStore.state.value)
        assertEquals(listOf("cancel-failed"), barrierEvents)
    }

    @Test
    fun invalidReplacementBearerCannotEraseTheBoundCanonicalCache() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "cursor-1",
                hasMore = false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val before = plannerStore.state.value
        var changed = false

        val failure = runCatching {
            manager.withConfigurationUpdateLock(
                requestedBaseUrl = "https://api.example.test/",
                bearerToken = "invalid token",
            ) { changed = true }
        }.exceptionOrNull()

        assertTrue(failure is InvalidApiConfigurationException)
        assertFalse(changed)
        assertEquals(before, plannerStore.state.value)
        assertEquals("connection-1", plannerStore.state.value.canonicalConfigurationId)
    }

    @Test
    fun blankTokenSaveIsNoOpButSameOriginReplacementCannotRebindPendingWrite() = runBlocking {
        val baseStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                changes = listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                nextCursor = "cursor-1",
                hasMore = false,
            )
            previewResult = preview()
        }
        val manager = manager(baseStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        var noOpRan = false
        manager.withConfigurationUpdateLock(
            requestedBaseUrl = "https://api.example.test/",
            bearerToken = null,
        ) { noOpRan = true }
        assertTrue(noOpRan)
        assertEquals("cursor-1", baseStore.state.value.canonicalDeltaCursor)

        val pending = PendingCanonicalMutation(
            idempotencyKey = "99999999-9999-4999-8999-999999999999",
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            itemId = TASK_ID,
            expectedRevision = 7,
            targetStatus = "completed",
            startedAt = clock.toString(),
            replacementRequestJson = "{}",
            focusedBlockId = BLOCK_ID,
            displayStatus = ItemStatus.COMPLETED,
        )
        val restarted = PlannerStore(
            baseStore.state.value.copy(pendingCanonicalMutation = pending),
        )
        var replacementRan = false
        val blocked = runCatching {
            manager(restarted, transport).withConfigurationUpdateLock(
                requestedBaseUrl = "https://api.example.test/",
                bearerToken = "different-workspace-token",
            ) { replacementRan = true }
        }.exceptionOrNull()
        assertTrue(blocked is CanonicalConfigurationChangeBlockedException)
        assertFalse(replacementRan)
        assertEquals("cursor-1", restarted.state.value.canonicalDeltaCursor)
        assertEquals(pending, restarted.state.value.pendingCanonicalMutation)
    }

    @Test
    fun replacementDoesNotRunWhenCanonicalQuarantineCannotBePersisted() = runBlocking {
        val failSaves = AtomicBoolean(false)
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState? = null

            override suspend fun save(state: DayWeaveUiState) {
                if (failSaves.get()) throw IOException("encrypted storage unavailable")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val plannerStore = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
            )
            withTimeout(3_000) {
                plannerStore.loadState.first { it == PlannerLoadState.READY }
            }
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
            }
            val manager = manager(plannerStore, transport)
            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            failSaves.set(true)
            var replacementRan = false

            val failure = runCatching {
                manager.withConfigurationUpdateLock(
                    requestedBaseUrl = "https://api.example.test/",
                    bearerToken = "replacement-token",
                ) { replacementRan = true }
            }.exceptionOrNull()

            assertTrue(failure is CanonicalAbandonmentPersistenceException)
            assertFalse(replacementRan)
            assertEquals(PlannerLoadState.PERSISTENCE_FAILED, plannerStore.loadState.value)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun incompatiblePreviewKeepsTheLastEncryptedPlanAndCursor() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val durable = plannerStore.state.value

        transport.pages.clear()
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
        transport.previewResult = preview().copy(sourceItemCount = 2)

        val outcome = manager.refreshAndCompose()

        assertEquals(CanonicalRefreshOutcome.PROTOCOL_FAILURE, outcome)
        assertEquals(CanonicalSyncPhase.ERROR, manager.state.value.phase)
        assertEquals(durable.schedule, plannerStore.state.value.schedule)
        assertEquals(durable.canonicalItems, plannerStore.state.value.canonicalItems)
        assertEquals("cursor-1", plannerStore.state.value.canonicalDeltaCursor)
    }

    @Test
    fun cancellationRestoresNonBusyStateAndDoesNotReplaceCache() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val started = CompletableDeferred<Unit>()
        val gate = CompletableDeferred<Unit>()
        val transport = FakeCanonicalTransport().apply {
            deltaStarted = started
            deltaGate = gate
        }
        val manager = manager(plannerStore, transport)

        val refresh = async { manager.refreshAndCompose() }
        withTimeout(3_000) { started.await() }
        assertEquals(CanonicalSyncPhase.SYNCING, manager.state.value.phase)

        refresh.cancelAndJoin()

        assertEquals(CanonicalSyncPhase.READY, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
    }

    @Test
    fun startUsesRevisionGuardAndReconcilesOnlyAfterCanonicalAcknowledgement() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.replacementResult = remoteItem(split = false).copy(
            status = "in_progress",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )

        val outcome = manager.start(BLOCK_ID)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
        assertEquals(TASK_ID, transport.replacementId)
        assertEquals(7L, transport.replacementRequest?.expectedRevision)
        assertEquals("in_progress", transport.replacementRequest?.item?.status)
        assertEquals(
            remoteItem(split = false).flexibleConstraints,
            transport.replacementRequest?.item?.flexibleConstraints,
        )
        assertEquals(
            remoteItem(split = false).splitPolicy,
            transport.replacementRequest?.item?.splitPolicy,
        )
        assertEquals(
            transport.replacementIdempotencyKey,
            UUID.fromString(requireNotNull(transport.replacementIdempotencyKey)).toString(),
        )
        val current = plannerStore.state.value
        assertEquals(8L, current.canonicalItems.single().revision)
        assertEquals(ItemStatus.ACTIVE, current.schedule.single().status)
        assertEquals(BLOCK_ID, current.activeSession?.itemId)
        assertEquals("cursor-1", current.canonicalDeltaCursor)
        assertEquals(null, current.scheduleInputDigest)
    }

    @Test
    fun scheduledOneShotLaterPersistsEarliestStartThenPublishesMovedPlan() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val moveStart = clock.plusSeconds(3 * 3_600)
        val moved = initial.copy(
            status = "scheduled",
            earliestStartAt = moveStart.toString(),
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.replacementResult = moved
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = moved)),
            "cursor-2",
            false,
        )
        transport.previewResult = scheduledPreview(moved).copy(
            plan = scheduledPreview(moved).plan.copy(
                blocks = listOf(
                    scheduledPreview(moved).plan.blocks.single().copy(
                        start = "2026-09-01T12:00:00+02:00",
                        end = "2026-09-01T13:00:00+02:00",
                    ),
                ),
            ),
        )

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.doLater(BLOCK_ID, moveStart))

        assertEquals("scheduled", transport.replacementRequest?.item?.status)
        assertEquals(moveStart.toString(), transport.replacementRequest?.item?.earliestStartAt)
        assertEquals("cursor-2", plannerStore.state.value.canonicalDeltaCursor)
        assertEquals("2026-09-01T10:00:00Z", plannerStore.state.value.schedule.single().absoluteStartAt)
        assertNotNull(plannerStore.state.value.publishedScheduleProof)
    }

    @Test
    fun scheduledLaterRequiresConflictApprovalAndDurablyExtendsCrossedDeadline() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val moveStart = Instant.parse("2026-09-01T12:00:00Z")

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.doLater(BLOCK_ID, moveStart),
        )
        assertEquals(null, transport.replacementRequest)
        val approval = requireNotNull(
            plannerStore.state.value.assessMoveLater(BLOCK_ID, moveStart, clock),
        ).toApprovalEnvelope()

        val relaxed = initial.copy(
            status = "scheduled",
            earliestStartAt = moveStart.toString(),
            deadlineAt = "2026-09-01T13:00:00Z",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.replacementResult = relaxed
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = relaxed)),
            "cursor-2",
            false,
        )
        transport.previewResult = scheduledPreview(relaxed).copy(
            plan = scheduledPreview(relaxed).plan.copy(
                blocks = listOf(
                    scheduledPreview(relaxed).plan.blocks.single().copy(
                        start = "2026-09-01T14:00:00+02:00",
                        end = "2026-09-01T15:00:00+02:00",
                    ),
                ),
            ),
        )

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager.doLater(BLOCK_ID, moveStart, approval),
        )
        assertEquals(moveStart.toString(), transport.replacementRequest?.item?.earliestStartAt)
        assertEquals("2026-09-01T13:00:00Z", transport.replacementRequest?.item?.deadlineAt)
        assertEquals("2026-09-01T13:00:00Z", plannerStore.state.value.canonicalItems.single().deadlineAt)
    }

    @Test
    fun scheduledLaterRechecksReviewedRevisionInsideTheOperationLock() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val secondNowCall = CompletableDeferred<Unit>()
        val nowCalls = AtomicInteger(0)
        val signalNowCalls = AtomicBoolean(false)
        val manager = CanonicalSyncManager(
            plannerStore = plannerStore,
            credentialStore = CanonicalCredentialStore(),
            transport = transport,
            now = {
                if (signalNowCalls.get() && nowCalls.incrementAndGet() == 2) {
                    secondNowCall.complete(Unit)
                }
                clock
            },
            zoneId = { ZoneId.of("Europe/Madrid") },
        )
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager.refreshAndCompose(),
        )
        val moveStart = Instant.parse("2026-09-01T10:00:00Z")
        assertFalse(
            requireNotNull(
                plannerStore.state.value.assessMoveLater(BLOCK_ID, moveStart, clock),
            ).requiresConfirmation,
        )

        val lockReady = CompletableDeferred<Unit>()
        val releaseLock = CompletableDeferred<Unit>()
        val lockHolder = async {
            manager.withConfigurationLock {
                lockReady.complete(Unit)
                releaseLock.await()
            }
        }
        lockReady.await()
        nowCalls.set(0)
        signalNowCalls.set(true)
        val move = async { manager.doLater(BLOCK_ID, moveStart) }
        secondNowCall.await()

        val beforeChange = plannerStore.state.value
        val changedItem = beforeChange.canonicalItems.single().copy(
            revision = 8,
            deadlineAt = "2026-09-01T12:30:00Z",
            updatedAt = "2026-09-01T07:01:00Z",
        )
        val changedBlock = beforeChange.schedule.single().copy(canonicalRevision = 8)
        val changeReceipt = requireNotNull(
            plannerStore.replaceCanonicalPlan(
                CanonicalPlanUpdate(
                    items = listOf(changedItem),
                    schedule = listOf(changedBlock),
                    syncOrigin = requireNotNull(beforeChange.canonicalSyncOrigin),
                    configurationId = beforeChange.canonicalConfigurationId,
                    deltaCursor = "cursor-2",
                    inputDigest = "sha256:${"b".repeat(64)}",
                    generatedAt = clock.toString(),
                    planningZoneId = "Europe/Madrid",
                    rejectedItemCount = 0,
                    unscheduledItemCount = 0,
                    protectedFreeMinutes = 840,
                    dayScore = 100,
                    violationMessages = emptyList(),
                    violationCount = 0,
                    errorViolationCount = 0,
                    unscheduledWork = emptyList(),
                    occurrenceSeriesItemIds = emptyMap(),
                    message = "New revision arrived during warning approval",
                ),
            ),
        )
        assertTrue(changeReceipt.awaitDurable())
        releaseLock.complete(Unit)

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            move.await(),
        )
        lockHolder.await()
        assertEquals(null, transport.replacementRequest)
        assertTrue(manager.state.value.message.contains("changed after review"))
    }

    @Test
    fun uncertainDeadlineRelaxationNeverClaimsAStatusOnlySupersedingRevision() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false)
        var replacementAttempt = 0
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                "cursor-1",
                false,
            )
            previewResult = preview()
            replacementHandler = { _, _, _ ->
                replacementAttempt += 1
                if (replacementAttempt == 1) {
                    throw IOException("synthetic response loss")
                }
                throw PlannerApiException.Conflict()
            }
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val superseding = initial.copy(
            status = "scheduled",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.queuedPages.getOrPut("cursor-1", ::ArrayDeque).apply {
            repeat(2) {
                add(
                    RemoteItemDeltaPage(
                        listOf(RemoteItemDeltaChange(type = "upsert", item = superseding)),
                        "cursor-2",
                        false,
                    ),
                )
            }
        }
        transport.previewResult = preview().copy(
            sourceItemRevisions = mapOf(TASK_ID to 8L),
        )

        val moveStart = Instant.parse("2026-09-01T12:00:00Z")
        val approval = requireNotNull(
            plannerStore.state.value.assessMoveLater(BLOCK_ID, moveStart, clock),
        ).toApprovalEnvelope()
        val outcome = manager.doLater(BLOCK_ID, moveStart, approval)

        assertEquals(CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE, outcome)
        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
        assertEquals(8L, plannerStore.state.value.canonicalItems.single().revision)
        assertEquals(initial.deadlineAt, plannerStore.state.value.canonicalItems.single().deadlineAt)
        assertEquals(2, transport.replacementRequests.size)
        assertTrue(transport.replacementRequests.all {
            it.item.deadlineAt == "2026-09-01T13:00:00Z"
        })
    }

    @Test
    fun scheduledSkipIsLimitedToAWholeOneShotItem() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val initial = remoteItem(split = false)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = initial)),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val skipped = initial.copy(
            status = "skipped",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.replacementResult = skipped
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = skipped)),
            "cursor-2",
            false,
        )
        transport.previewResult = terminalPreview(8)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.skipScheduled(BLOCK_ID))
        assertEquals("skipped", transport.replacementRequest?.item?.status)

        assertEquals(ItemStatus.SKIPPED, plannerStore.state.value.schedule.single().status)
    }

    @Test
    fun scheduledSkipRejectsSplitWorkWithoutWritingCanonicalStatus() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = true))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.skipScheduled(BLOCK_ID),
        )
        assertEquals(null, transport.replacementRequest)
        assertEquals(ItemStatus.SCHEDULED, plannerStore.state.value.schedule.single().status)
    }

    @Test
    fun scheduledLaterRejectsNonRecurringSplitWorkWithoutReplacingItsParent() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = true))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.doLater(BLOCK_ID, clock.plusSeconds(3_600)),
        )
        assertEquals(null, transport.replacementRequest)
        assertEquals(ItemStatus.SCHEDULED, plannerStore.state.value.schedule.single().status)
    }

    @Test
    fun anExpiredCanonicalBreakCanBeExtendedWithoutImplicitResume() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.replacementResult = remoteItem(split = false).copy(
            status = "in_progress",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.start(BLOCK_ID))
        transport.replacementResult = remoteItem(split = false).copy(
            status = "paused",
            revision = 9,
            updatedAt = "2026-09-01T07:02:00Z",
        )
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.pause(BLOCK_ID, 5))
        val firstDeadline = requireNotNull(
            plannerStore.state.value.activeSession?.pauseUntilEpochMillis,
        )
        transport.replacementResult = remoteItem(split = false).copy(
            status = "paused",
            revision = 10,
            updatedAt = "2026-09-01T07:03:00Z",
        )

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.pause(BLOCK_ID, 10))

        val extended = requireNotNull(plannerStore.state.value.activeSession)
        assertTrue(extended.isPaused)
        assertTrue(requireNotNull(extended.pauseUntilEpochMillis) > firstDeadline)
        assertFalse(extended.timedBreakEnded)
        assertEquals("paused", transport.replacementRequest?.item?.status)
        assertEquals(9L, transport.replacementRequest?.expectedRevision)
    }

    @Test
    fun staleMutationKeepsDurablePlanAndRequestsARefresh() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val durable = plannerStore.state.value
        transport.replacementError = PlannerApiException.Conflict()

        val outcome = manager.start(BLOCK_ID)

        assertEquals(CanonicalRefreshOutcome.STALE_REVISION, outcome)
        assertEquals(CanonicalSyncPhase.ERROR, manager.state.value.phase)
        val current = plannerStore.state.value
        assertNotNull(current.pendingCanonicalMutation)
        assertEquals(transport.replacementIdempotencyKey, current.pendingCanonicalMutation?.idempotencyKey)
        assertEquals(durable.schedule, current.schedule)
        assertEquals(durable.canonicalItems, current.canonicalItems)
        assertEquals(durable.canonicalDeltaCursor, current.canonicalDeltaCursor)
    }

    @Test
    fun concurrentPreviewSnapshotIsDiscardedAndWholeCycleIsRetried() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val revisionSeven = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
            "cursor-1",
            false,
        )
        val revisionEightItem = remoteItem().copy(
            revision = 8,
            updatedAt = "2026-09-01T07:00:30Z",
        )
        val revisionEight = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = revisionEightItem)),
            "cursor-2",
            false,
        )
        val transport = FakeCanonicalTransport().apply {
            queuedPages.getOrPut(null, ::ArrayDeque).apply {
                add(revisionSeven)
                add(revisionEight)
            }
            queuedPreviews.add(preview().copy(sourceItemRevisions = mapOf(TASK_ID to 8)))
            queuedPreviews.add(
                preview().copy(sourceItemRevisions = mapOf(TASK_ID to 8)),
            )
        }
        val manager = manager(plannerStore, transport)

        val outcome = manager.refreshAndCompose()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
        assertEquals(listOf(null, null), transport.deltaCursors)
        assertEquals(2, transport.previewRequests.size)
        assertEquals(8L, plannerStore.state.value.canonicalItems.single().revision)
        assertEquals(8L, plannerStore.state.value.schedule.single().canonicalRevision)
        assertEquals("cursor-2", plannerStore.state.value.canonicalDeltaCursor)
    }

    @Test
    fun secondComposePinsOnlyAnAssignmentWhollyInsideFreezeHorizon() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        val assignment = transport.previewRequests.last().previousAssignments.single()
        assertTrue(assignment.pinned)
        assertEquals("2026-09-01T07:00:00Z", assignment.blocks.single().start)
        assertEquals("2026-09-01T08:00:00Z", assignment.blocks.single().end)
    }

    @Test
    fun longAssignmentOverlappingFreezeBoundaryRemainsSoft() = runBlocking {
        val longBlock = preview().plan.blocks.single().copy(
            end = "2026-09-01T12:00:00+02:00",
        )
        val longPreview = preview().copy(
            plan = preview().plan.copy(
                blocks = listOf(longBlock),
                score = RemotePlanScore(180, 0, 0uL, 0),
            ),
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = longPreview
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertFalse(transport.previewRequests.last().previousAssignments.single().pinned)
    }

    @Test
    fun expiredIdempotencyConflictUsesAuthoritativeDeltaWithoutDroppingFence() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.replacementError = IOException("response lost")

        assertEquals(CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE, manager.start(BLOCK_ID))
        val pending = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        assertTrue(transport.replacementIdempotencyKeys.all { it == pending.idempotencyKey })

        val applied = remoteItem(split = false).copy(
            status = "in_progress",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        transport.replacementError = PlannerApiException.Conflict()
        transport.queuedPages.getOrPut("cursor-1", ::ArrayDeque).apply {
            add(
                RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
                    "cursor-2",
                    false,
                ),
            )
            add(
                RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
                    "cursor-2",
                    false,
                ),
            )
        }
        transport.previewResult = preview().copy(
            sourceItemRevisions = mapOf(TASK_ID to 8L),
        )

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
        assertEquals(8L, plannerStore.state.value.canonicalItems.single().revision)
        assertEquals(ItemStatus.ACTIVE, plannerStore.state.value.schedule.single().status)
        assertTrue(transport.replacementIdempotencyKeys.all { it == pending.idempotencyKey })
    }

    @Test
    fun completedExecutionImmediatelyProjectsOneShotLeafBeforeRecompose() = runBlocking {
        assertTerminalExecutionProjects("completed", ItemStatus.COMPLETED)
    }

    @Test
    fun skippedExecutionImmediatelyProjectsOneShotLeafBeforeRecompose() = runBlocking {
        assertTerminalExecutionProjects("skipped", ItemStatus.SKIPPED)
    }

    @Test
    fun correctedExecutionDurationSurvivesCanonicalParentProjectionExactly() = runBlocking {
        assertTerminalExecutionProjects(
            "completed",
            ItemStatus.COMPLETED,
            actualSeconds = 0,
            expectedActualMinutes = 0,
        )
        assertTerminalExecutionProjects(
            "completed",
            ItemStatus.COMPLETED,
            actualSeconds = 90 * 60,
            expectedActualMinutes = 90,
        )
    }

    @Test
    fun newerDeferredClosurePreventsAutomaticProjectionOfOlderTerminalOutcome() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        val olderCompleted = plannerStore.state.value.terminalExecutionOutcomes
            .getValue(EXECUTION_ID).session
        val newerDeferred = olderCompleted.copy(
            id = "88888888-8888-4888-8888-888888888888",
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 180,
            actualSeconds = 180,
            endedAt = "2026-09-01T07:03:00Z",
            moveStart = "2026-09-01T08:00:00Z",
            moveEnd = "2026-09-01T09:00:00Z",
            updatedAt = "2026-09-01T07:03:00Z",
            canonicalProjectionEligibleAtLeaseStart = null,
        )
        requireNotNull(
            plannerStore.reconcileCanonicalExecution(
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = 4,
                activeSession = null,
                changedSession = newerDeferred,
                message = "Newer defer",
            ),
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
        transport.previewResult = preview()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertTrue(transport.replacementRequests.isEmpty())
        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
        assertEquals("planned", plannerStore.state.value.canonicalItems.single().status)
        assertEquals(ItemStatus.SCHEDULED, plannerStore.state.value.schedule.single().status)
        assertTrue(
            plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .requiresCanonicalItemProjection,
        )
    }

    @Test
    fun lostTerminalProjectionResponseReplaysExactCanonicalFenceAfterRestart() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val firstManager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, firstManager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.replacementError = IOException("response lost")

        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            firstManager.refreshAndCompose(),
        )
        val uncertain = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        assertEquals(EXECUTION_ID, uncertain.terminalExecutionSessionId)
        assertEquals("completed", uncertain.targetStatus)
        val exactRequest = uncertain.replacementRequestJson
        val restartedStore = PlannerStore(plannerStore.state.value)

        val applied = remoteItem(split = false).copy(
            status = "completed",
            revision = 8,
            updatedAt = "2026-09-01T07:02:00Z",
            completedAt = "2026-09-01T07:02:00Z",
        )
        transport.replacementError = null
        transport.replacementResult = applied
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
            "cursor-2",
            false,
        )
        transport.previewResult = terminalPreview(8)

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(restartedStore, transport).refreshAndCompose(),
        )

        assertEquals(listOf(uncertain.idempotencyKey, uncertain.idempotencyKey), transport.replacementIdempotencyKeys)
        assertEquals(2, transport.replacementRequests.size)
        assertEquals(transport.replacementRequests.first(), transport.replacementRequests.last())
        assertEquals(exactRequest, uncertain.replacementRequestJson)
        assertEquals(null, restartedStore.state.value.pendingCanonicalMutation)
        assertEquals(
            8L,
            restartedStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionRevision,
        )
        assertEquals(ItemStatus.COMPLETED, restartedStore.state.value.schedule.single().status)
    }

    @Test
    fun notFoundReadFailureRetainsExactProjectionFenceUntilTombstoneCanBeProved() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val firstManager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, firstManager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.replacementError = PlannerApiException.Http(404)
        transport.deltaError = IOException("authoritative read unavailable")

        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            firstManager.refreshAndCompose(),
        )
        val pending = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        val exactBody = pending.replacementRequestJson
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))

        val restarted = PlannerStore(plannerStore.state.value)
        transport.deltaError = null
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(
                RemoteItemDeltaChange(
                    type = "tombstone",
                    tombstone = RemoteItemTombstone(
                        id = TASK_ID,
                        revision = 8,
                        deletedAt = "2026-09-01T07:03:00Z",
                    ),
                ),
            ),
            "cursor-2",
            false,
        )
        transport.previewResult = emptyPreview()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager(restarted, transport).refreshAndCompose())
        assertEquals(listOf(pending.idempotencyKey, pending.idempotencyKey), transport.replacementIdempotencyKeys)
        assertEquals(transport.replacementRequests[0], transport.replacementRequests[1])
        assertEquals(exactBody, pending.replacementRequestJson)
        assertEquals(
            "item_deleted",
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionResolution,
        )
        assertEquals(null, restarted.state.value.pendingCanonicalMutation)
    }

    @Test
    fun malformedSuccessfulProjectionResponseRetainsExactFenceForRestartReplay() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val firstManager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, firstManager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.replacementResult = remoteItem(split = false).copy(
            status = "completed",
            revision = 99,
            updatedAt = "2026-09-01T07:02:00Z",
            completedAt = "2026-09-01T07:02:00Z",
        )

        assertEquals(CanonicalRefreshOutcome.PROTOCOL_FAILURE, firstManager.refreshAndCompose())
        val pending = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))

        val restarted = PlannerStore(plannerStore.state.value)
        val applied = remoteItem(split = false).copy(
            status = "completed",
            revision = 8,
            updatedAt = "2026-09-01T07:02:00Z",
            completedAt = "2026-09-01T07:02:00Z",
        )
        transport.replacementResult = applied
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
            "cursor-2",
            false,
        )
        transport.previewResult = terminalPreview(8)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager(restarted, transport).refreshAndCompose())
        assertEquals(listOf(pending.idempotencyKey, pending.idempotencyKey), transport.replacementIdempotencyKeys)
        assertEquals(transport.replacementRequests[0], transport.replacementRequests[1])
        assertEquals(8L, restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
            .canonicalProjectionRevision)
    }

    @Test
    fun leaseProvenanceProjectsAfterNewerCompositionArrivesBeforeTerminalHistory() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val running = CanonicalExecutionSessionSnapshot(
            id = EXECUTION_ID,
            itemId = TASK_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            status = "active",
            revision = 1,
            accumulatedSeconds = 0,
            startedAt = "2026-09-01T07:00:00Z",
            runningSince = "2026-09-01T07:00:00Z",
            createdAt = "2026-09-01T07:00:00Z",
            updatedAt = "2026-09-01T07:00:00Z",
            canonicalProjectionEligibleAtLeaseStart = true,
        )
        requireNotNull(
            plannerStore.reconcileCanonicalExecution(
                "https://api.example.test/",
                "connection-1",
                1,
                running,
                message = "Running",
            ),
        )
        val edited = remoteItem(split = false).copy(
            title = "Latest title before history",
            notes = "Latest notes must survive",
            revision = 8,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        val cachedItem = plannerStore.state.value.canonicalItems.single().copy(
            title = edited.title,
            notes = edited.notes,
            revision = edited.revision,
            updatedAt = edited.updatedAt,
        )
        val cachedBlock = plannerStore.state.value.schedule.single().copy(
            title = edited.title,
            status = ItemStatus.SCHEDULED,
            canonicalRevision = edited.revision,
        )
        requireNotNull(
            plannerStore.replaceCanonicalPlan(
                CanonicalPlanUpdate(
                    items = listOf(cachedItem),
                    schedule = listOf(cachedBlock),
                    syncOrigin = "https://api.example.test/",
                    configurationId = "connection-1",
                    deltaCursor = "cursor-2",
                    inputDigest = "sha256:${"b".repeat(64)}",
                    generatedAt = clock.toString(),
                    planningZoneId = "Europe/Madrid",
                    rejectedItemCount = 0,
                    unscheduledItemCount = 0,
                    protectedFreeMinutes = 840,
                    dayScore = 100,
                    violationMessages = emptyList(),
                    violationCount = 0,
                    errorViolationCount = 0,
                    unscheduledWork = emptyList(),
                    occurrenceSeriesItemIds = emptyMap(),
                    message = "Newer composition arrived",
                ),
            ),
        )

        requireNotNull(
            plannerStore.reconcileCanonicalExecution(
                "https://api.example.test/",
                "connection-1",
                2,
                null,
                running.copy(
                    status = "completed",
                    revision = 2,
                    accumulatedSeconds = 120,
                    actualSeconds = 120,
                    runningSince = null,
                    endedAt = "2026-09-01T07:02:00Z",
                    updatedAt = "2026-09-01T07:02:00Z",
                ),
                message = "Ended",
            ),
        )
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))
        val terminal = edited.copy(
            status = "completed",
            revision = 9,
            updatedAt = "2026-09-01T07:03:00Z",
            completedAt = "2026-09-01T07:03:00Z",
        )
        transport.replacementResult = terminal
        transport.pages["cursor-2"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = terminal)),
            "cursor-3",
            false,
        )
        transport.previewResult = terminalPreview(9)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertEquals(8L, transport.replacementRequest?.expectedRevision)
        assertEquals("Latest title before history", transport.replacementRequest?.item?.title)
        assertEquals("Latest notes must survive", transport.replacementRequest?.item?.notes)
        val outcome = plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertEquals(7L, outcome.session.itemRevision)
        assertEquals(true, outcome.session.canonicalProjectionEligibleAtLeaseStart)
        assertEquals(9L, outcome.canonicalProjectionRevision)
    }

    @Test
    fun conflictRefreshNeverWritesUntilDurableApprovalThenLostResponseReplays() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.replacementError = PlannerApiException.Validation(422)
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-1", false)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val conflicted = plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertTrue(requireNotNull(conflicted.canonicalProjectionConflict).contains("HTTP 422"))
        assertEquals(null, conflicted.canonicalProjectionRetryAuthorizedAt)
        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
        assertEquals(1, transport.replacementRequests.size)

        val reopened = remoteItem(split = false).copy(
            title = "Reopened by another client",
            revision = 8,
            updatedAt = "2026-09-01T07:03:00Z",
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = reopened)),
            "cursor-2",
            false,
        )
        transport.previewResult = scheduledPreview(reopened)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertEquals(1, transport.replacementRequests.size)
        assertEquals(8L, plannerStore.state.value.canonicalItems.single().revision)
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))

        requireNotNull(plannerStore.authorizeTerminalProjectionRetry(EXECUTION_ID))
        val afterApprovalRestart = PlannerStore(plannerStore.state.value)
        assertNotNull(
            afterApprovalRestart.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionRetryAuthorizedAt,
        )
        transport.replacementError = IOException("approved response lost")
        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(afterApprovalRestart, transport).refreshAndCompose(),
        )
        val pending = requireNotNull(afterApprovalRestart.state.value.pendingCanonicalMutation)
        assertEquals(8L, pending.expectedRevision)
        assertEquals(EXECUTION_ID, pending.terminalExecutionSessionId)

        val afterWriteRestart = PlannerStore(afterApprovalRestart.state.value)
        val applied = reopened.copy(
            status = "completed",
            revision = 9,
            updatedAt = "2026-09-01T07:04:00Z",
            completedAt = "2026-09-01T07:04:00Z",
        )
        transport.replacementError = null
        transport.replacementResult = applied
        transport.pages["cursor-2"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
            "cursor-3",
            false,
        )
        transport.previewResult = terminalPreview(9)

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(afterWriteRestart, transport).refreshAndCompose(),
        )
        assertEquals(3, transport.replacementRequests.size)
        assertEquals(
            transport.replacementIdempotencyKeys[1],
            transport.replacementIdempotencyKeys[2],
        )
        assertEquals(transport.replacementRequests[1], transport.replacementRequests[2])
        val resolved = afterWriteRestart.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertEquals(9L, resolved.canonicalProjectionRevision)
        assertEquals(null, resolved.canonicalProjectionRetryAuthorizedAt)
        assertEquals(null, resolved.canonicalProjectionConflict)
    }

    @Test
    fun deterministicRejectionAtNewerPlannedRevisionPersistsConflictWithoutAutoRebase() =
        runBlocking {
            val plannerStore = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
            }
            val manager = manager(plannerStore, transport)
            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            recordTerminalExecution(plannerStore, "completed")
            val newerPlanned = remoteItem(split = false).copy(
                title = "Edited on another client",
                revision = 8,
                updatedAt = "2026-09-01T07:03:00Z",
            )
            transport.replacementError = PlannerApiException.Validation(400)
            transport.pages["cursor-1"] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = newerPlanned)),
                "cursor-2",
                false,
            )
            transport.previewResult = scheduledPreview(newerPlanned)

            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            val conflicted = plannerStore.state.value.terminalExecutionOutcomes
                .getValue(EXECUTION_ID)
            assertTrue(requireNotNull(conflicted.canonicalProjectionConflict).contains("HTTP 400"))
            assertEquals(null, conflicted.canonicalProjectionRetryAuthorizedAt)
            assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
            assertEquals(8L, plannerStore.state.value.canonicalItems.single().revision)
            assertEquals(1, transport.replacementRequests.size)

            transport.pages["cursor-2"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            assertEquals(1, transport.replacementRequests.size)
        }

    @Test
    fun deterministicRejectionResolvesOnlyAnExactAuthoritativeTerminalReplacement() =
        runBlocking {
            val plannerStore = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
            }
            val manager = manager(plannerStore, transport)
            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            recordTerminalExecution(plannerStore, "completed")
            val exactTerminal = remoteItem(split = false).copy(
                status = "completed",
                revision = 8,
                updatedAt = "2026-09-01T07:03:00Z",
                completedAt = "2026-09-01T07:03:00Z",
            )
            transport.replacementError = PlannerApiException.Validation(422)
            transport.pages["cursor-1"] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = exactTerminal)),
                "cursor-2",
                false,
            )
            transport.previewResult = terminalPreview(8)

            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            val resolved = plannerStore.state.value.terminalExecutionOutcomes
                .getValue(EXECUTION_ID)
            assertEquals(8L, resolved.canonicalProjectionRevision)
            assertEquals(null, resolved.canonicalProjectionConflict)
            assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
            assertEquals(1, transport.replacementRequests.size)
        }

    @Test
    fun staleEditedLeafRebasesLosslesslyAndReplaysExactResponseLossAfterRestart() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val firstManager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, firstManager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")

        val edited = remoteItem(split = false).copy(
            title = "Latest edited title",
            notes = "Latest lossless notes",
            importance = 91,
            urgency = 73,
            revision = 8,
            updatedAt = "2026-09-01T07:03:00Z",
        )
        val applied = edited.copy(
            status = "completed",
            revision = 9,
            updatedAt = "2026-09-01T07:04:00Z",
            completedAt = "2026-09-01T07:04:00Z",
        )
        var rebasedAttempts = 0
        transport.replacementHandler = { _, _, request ->
            when (request.expectedRevision) {
                7L -> throw PlannerApiException.Conflict()
                8L -> {
                    rebasedAttempts += 1
                    if (rebasedAttempts == 1) throw IOException("rebased response lost")
                    applied
                }
                else -> error("Unexpected revision ${request.expectedRevision}")
            }
        }
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-1", false)

        assertEquals(CanonicalRefreshOutcome.STALE_REVISION, firstManager.refreshAndCompose())
        val staleKey = requireNotNull(
            plannerStore.state.value.pendingCanonicalMutation?.idempotencyKey,
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = edited)),
            "cursor-2",
            false,
        )
        transport.queuedPreviews += scheduledPreview(edited)

        assertEquals(
            CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            firstManager.refreshAndCompose(),
        )
        val rebasedPending = requireNotNull(plannerStore.state.value.pendingCanonicalMutation)
        assertEquals(8L, rebasedPending.expectedRevision)
        assertEquals(EXECUTION_ID, rebasedPending.terminalExecutionSessionId)
        assertEquals(ItemStatus.COMPLETED, plannerStore.state.value.schedule.single().status)
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))
        val rebasedRequest = transport.replacementRequests.last()
        assertEquals("Latest edited title", rebasedRequest.item.title)
        assertEquals("Latest lossless notes", rebasedRequest.item.notes)
        assertEquals(91, rebasedRequest.item.importance)
        assertEquals(73, rebasedRequest.item.urgency)
        assertEquals("completed", rebasedRequest.item.status)
        val rebasedKey = rebasedPending.idempotencyKey

        val restartedStore = PlannerStore(plannerStore.state.value)
        transport.pages["cursor-2"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
            "cursor-3",
            false,
        )
        transport.queuedPreviews += terminalPreview(9)

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(restartedStore, transport).refreshAndCompose(),
        )

        assertEquals(listOf(staleKey, staleKey, rebasedKey, rebasedKey), transport.replacementIdempotencyKeys)
        assertEquals(transport.replacementRequests[2], transport.replacementRequests[3])
        assertEquals(7L, restartedStore.state.value.terminalExecutionOutcomes
            .getValue(EXECUTION_ID).session.itemRevision)
        assertEquals(9L, restartedStore.state.value.terminalExecutionOutcomes
            .getValue(EXECUTION_ID).canonicalProjectionRevision)
        assertEquals("Latest edited title", restartedStore.state.value.canonicalItems.single().title)
        assertEquals(ItemStatus.COMPLETED, restartedStore.state.value.schedule.single().status)
    }

    @Test
    fun notFoundLeafRequiresAuthoritativeTombstoneBeforeResolvingHistory() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
            replacementError = PlannerApiException.Http(404)
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(
                RemoteItemDeltaChange(
                    type = "tombstone",
                    tombstone = RemoteItemTombstone(
                        id = TASK_ID,
                        revision = 8,
                        deletedAt = "2026-09-01T07:03:00Z",
                    ),
                ),
            ),
            "cursor-2",
            false,
        )
        transport.previewResult = emptyPreview()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(1, transport.replacementIdempotencyKeys.size)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        val outcome = plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertEquals("item_deleted", outcome.canonicalProjectionResolution)
        assertEquals(null, outcome.canonicalProjectionConflict)
        assertEquals(null, plannerStore.state.value.pendingCanonicalMutation)
    }

    @Test
    fun staleAlreadyTerminalLeafResolvesWithoutDuplicateStatusWrite() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
            replacementError = PlannerApiException.Conflict()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-1", false)
        assertEquals(CanonicalRefreshOutcome.STALE_REVISION, manager.refreshAndCompose())
        val staleKey = plannerStore.state.value.pendingCanonicalMutation?.idempotencyKey
        val alreadyCompleted = remoteItem(split = false).copy(
            title = "Completed elsewhere after an edit",
            status = "completed",
            revision = 8,
            updatedAt = "2026-09-01T07:03:00Z",
            completedAt = "2026-09-01T07:03:00Z",
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = alreadyCompleted)),
            "cursor-2",
            false,
        )
        transport.previewResult = terminalPreview(8)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(listOf(staleKey, staleKey), transport.replacementIdempotencyKeys)
        val outcome = plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertEquals(8L, outcome.canonicalProjectionRevision)
        assertEquals(null, outcome.canonicalProjectionConflict)
        assertEquals("completed", plannerStore.state.value.canonicalItems.single().status)
        assertEquals(ItemStatus.COMPLETED, plannerStore.state.value.schedule.single().status)
    }

    @Test
    fun staleIneligibleLeafPersistsReviewConflictUntilUserKeepsLatestAsNewWork() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
            replacementError = PlannerApiException.Conflict()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        recordTerminalExecution(plannerStore, "completed")
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-1", false)
        assertEquals(CanonicalRefreshOutcome.STALE_REVISION, manager.refreshAndCompose())
        val latestSplit = remoteItem(split = true).copy(
            revision = 8,
            updatedAt = "2026-09-01T07:03:00Z",
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = latestSplit)),
            "cursor-2",
            false,
        )
        transport.previewResult = scheduledPreview(latestSplit)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        val conflicted = plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertTrue(requireNotNull(conflicted.canonicalProjectionConflict).contains("splittable"))
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))
        requireNotNull(plannerStore.keepLatestItemAfterTerminalConflict(EXECUTION_ID))
        val resolved = plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertEquals("user_kept_latest_item", resolved.canonicalProjectionResolution)
        assertEquals(null, resolved.canonicalProjectionConflict)
        // The projection conflict is resolved, but a fresh bounded execution-history poll still
        // gates admission before this latest work can start.
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(BLOCK_ID))
    }

    @Test
    fun nonRecurringSplitSessionsRejectMixedCompletedAndSkippedOutcomes() = runBlocking {
        val first = preview().plan.blocks.single().copy(
            end = "2026-09-01T09:30:00+02:00",
        )
        val second = first.copy(
            id = SECOND_BLOCK_ID,
            start = "2026-09-01T09:30:00+02:00",
            end = "2026-09-01T10:00:00+02:00",
            sessionIndex = 1,
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(blocks = listOf(first, second)),
            )
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.start(BLOCK_ID))
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.complete(BLOCK_ID))
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.start(SECOND_BLOCK_ID))

        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, manager.skip(SECOND_BLOCK_ID))

        assertEquals(
            ItemStatus.COMPLETED,
            plannerStore.state.value.schedule.first { it.id == BLOCK_ID }.status,
        )
        assertEquals(
            ItemStatus.ACTIVE,
            plannerStore.state.value.schedule.first { it.id == SECOND_BLOCK_ID }.status,
        )
    }

    @Test
    fun recurringSplitSessionsRejectMixedCompletedAndSkippedOutcomes() = runBlocking {
        val first = preview().plan.blocks.single().copy(
            occurrenceId = OCCURRENCE_ID,
            end = "2026-09-01T09:30:00+02:00",
        )
        val second = first.copy(
            id = SECOND_BLOCK_ID,
            start = "2026-09-01T09:30:00+02:00",
            end = "2026-09-01T10:00:00+02:00",
            sessionIndex = 1,
        )
        val recurringItem = remoteItem().copy(
            recurrence = dailyRecurrence(),
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(first, second),
                    occurrences = listOf(occurrence),
                ),
            )
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.start(BLOCK_ID))
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.complete(BLOCK_ID))
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.start(SECOND_BLOCK_ID))

        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, manager.skip(SECOND_BLOCK_ID))
        assertTrue(plannerStore.state.value.recurrenceOutcomes.isEmpty())
    }

    @Test
    fun occurrenceLocalDateUsesEmbeddedNominalOffsetAcrossNegativeSeriesZone() = runBlocking {
        val recurringItem = remoteItem(split = false).copy(
            kind = "habit",
            timezoneName = "America/Los_Angeles",
            recurrence = dailyRecurrence(),
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T00:00:00Z",
            nominalEnd = "2026-09-01T01:00:00Z",
            windowStart = "2026-09-01T00:00:00Z",
            windowEnd = "2026-09-01T12:00:00Z",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(
                        preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                    ),
                    occurrences = listOf(occurrence),
                ),
            )
        }
        val plannerStore = PlannerStore(DayWeaveUiState())

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshAndCompose(),
        )

        val source = plannerStore.state.value.recurrenceOccurrenceSources
            .getValue(OCCURRENCE_ID)
        assertEquals("2026-09-01", source.localDate)
        assertEquals("2026-09-01T00:00:00Z", source.nominalStart)
    }

    @Test
    fun futureRecurrenceMoveRemainsInPreviewContextUntilItsTargetWindow() = runBlocking {
        val recurringItem = remoteItem(split = false).copy(
            kind = "habit",
            recurrence = dailyRecurrence(),
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val recurringPreview = preview().copy(
            plan = preview().plan.copy(
                blocks = listOf(
                    preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                ),
                occurrences = listOf(occurrence),
            ),
        )
        val initialStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                "cursor-1",
                false,
            )
            previewResult = recurringPreview
        }
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(initialStore, transport).refreshAndCompose(),
        )
        val movedStore = PlannerStore(
            initialStore.state.value.copy(
                recurrenceMoves = mapOf(
                    OCCURRENCE_ID to RecurrenceMoveSnapshot(
                        itemId = TASK_ID,
                        startAt = "2026-09-01T10:00:00Z",
                        endAt = "2026-09-01T11:00:00Z",
                        movedAt = "2026-07-01T00:00:00Z",
                        source = RecurrenceOccurrenceSourceSnapshot(
                            itemId = TASK_ID,
                            itemRevision = 7,
                            identityJson = dailyOccurrenceIdentity().toString(),
                            nominalStart = "2026-09-01T09:00:00+02:00",
                            nominalEnd = "2026-09-01T10:00:00+02:00",
                            localDate = "2026-09-01",
                            ordinal = 0,
                        ),
                    ),
                ),
            ),
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(movedStore, transport).refreshAndCompose(),
        )

        val exception = requireNotNull(transport.previewRequests.last().recurrenceContext["exceptions"])
            .jsonArray.single().jsonObject
        assertEquals(TASK_ID, exception.getValue("item_id").jsonPrimitive.content)
        assertEquals(
            "move",
            exception.getValue("action").jsonObject.getValue("type").jsonPrimitive.content,
        )
        val source = exception.getValue("action").jsonObject.getValue("source").jsonObject
        assertEquals("7", source.getValue("item_revision").jsonPrimitive.content)
        assertEquals(
            "2026-09-01T09:00:00+02:00",
            source.getValue("nominal_start").jsonPrimitive.content,
        )
        assertEquals(dailyOccurrenceIdentity(), source.getValue("identity").jsonObject)
        assertEquals("2026-09-01", source.getValue("local_date").jsonPrimitive.content)
        assertEquals("0", source.getValue("ordinal").jsonPrimitive.content)
    }

    @Test
    fun authoritativeHabitPartialProgressUsesImmutableDurationAndIgnoresPendingCorrection() =
        runBlocking {
            val evidence = HabitOccurrenceEvidenceSnapshot(
                id = HABIT_LEDGER_OCCURRENCE_ID,
                habitId = TASK_ID,
                plannerOccurrenceId = OCCURRENCE_ID,
                sourceScheduleRevisionId = HABIT_SOURCE_SCHEDULE_ID,
                sourceItemRevision = 7,
                policyFingerprint = "sha256:${"a".repeat(64)}",
                identity = dailyOccurrenceIdentity(),
                nominalStart = "2026-09-01T07:00:00Z",
                nominalEnd = "2026-09-01T07:30:01Z",
                windowStart = "2026-09-01T06:00:00Z",
                windowEnd = "2026-09-01T10:00:00Z",
                localDate = "2026-09-01",
                timezoneName = "Europe/Madrid",
                expectedDurationSeconds = 1_801,
                expectedQuantity = null,
                expectedUnit = null,
            )
            val authoritative = HabitOutcomeSnapshot(
                revision = 1,
                status = HabitOutcomeStatusSnapshot.PARTIAL,
                progressBasisPoints = 3_500,
                quantity = null,
                unit = null,
                actualSeconds = 600,
                note = null,
                occurredAt = "2026-09-01T07:10:00Z",
                updatedAt = "2026-09-01T07:11:00Z",
            )
            val pendingCommand = HabitOutcomeCommandSnapshot(
                operationId = HABIT_OPERATION_ID,
                expectedRevision = 1,
                outcome = HabitOutcomeInputSnapshot(
                    status = HabitOutcomeStatusSnapshot.PARTIAL,
                    progressBasisPoints = 8_000,
                    quantity = null,
                    unit = null,
                    actualSeconds = 1_400,
                    note = null,
                    occurredAt = "2026-09-01T07:20:00Z",
                ),
            )
            val ledger = HabitLedgerSnapshot(
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                occurrences = mapOf(
                    evidence.id to HabitOccurrenceSnapshot(evidence, authoritative),
                ),
                pendingMutations = listOf(
                    PendingHabitMutation(
                        schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
                        kind = PendingHabitMutationKind.OUTCOME,
                        habitId = TASK_ID,
                        targetId = evidence.id,
                        expectedRevision = 1,
                        idempotencyKey = HABIT_OPERATION_ID,
                        requestJson = pendingCommand.encoded(),
                        createdAt = "2026-09-01T07:20:00Z",
                        syncOrigin = "https://api.example.test/",
                        configurationId = "connection-1",
                    ),
                ),
            ).also(HabitLedgerSnapshot::requireValid)
            val plannerStore = PlannerStore(
                DayWeaveUiState(
                    canonicalSyncOrigin = "https://api.example.test/",
                    canonicalConfigurationId = "connection-1",
                    habitLedger = ledger,
                ),
            )
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(
                        RemoteItemDeltaChange(
                            type = "upsert",
                            item = remoteItem(split = false).copy(
                                kind = "habit",
                                title = "Renamed without changing recurrence",
                                durationSeconds = 1_801,
                                deadlineAt = null,
                                recurrence = dailyRecurrence(),
                                revision = 8,
                                updatedAt = "2026-08-29T11:00:00Z",
                            ),
                        ),
                    ),
                    "cursor-1",
                    false,
                )
                previewResult = preview().copy(
                    sourceItemRevisions = mapOf(TASK_ID to 8),
                    plan = preview().plan.copy(
                        blocks = listOf(
                            preview().plan.blocks.single().copy(
                                title = "Renamed without changing recurrence",
                            ),
                        ),
                    ),
                )
            }

            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(plannerStore, transport).refreshAndCompose(),
            )

            val progress = requireNotNull(
                transport.previewRequests.single().recurrenceContext["partial_progress"],
            ).jsonObject
            assertEquals(setOf(OCCURRENCE_ID), progress.keys)
            val occurrenceProgress = progress.getValue(OCCURRENCE_ID).jsonObject
            assertEquals(
                "3500",
                occurrenceProgress.getValue("progress_basis_points").jsonPrimitive.content,
            )
            assertEquals(
                "31",
                occurrenceProgress.getValue("expected_duration_minutes").jsonPrimitive.content,
            )
            assertEquals(null, occurrenceProgress["remaining_duration_minutes"])
        }

    @Test
    fun recurrenceMoveAcceptsLaterHorizonDaysAndRejectsOutsideTheExactHorizon() = runBlocking {
        suspend fun verify(dayOffset: Long, expectedSuccess: Boolean) {
            val recurringItem = remoteItem(split = false).copy(
                kind = "habit",
                deadlineAt = null,
                recurrence = dailyRecurrence(),
            )
            val sourceOccurrence = RemotePlanOccurrence(
                id = OCCURRENCE_ID,
                seriesItemId = TASK_ID,
                identity = dailyOccurrenceIdentity(),
                nominalStart = "2026-09-01T09:00:00+02:00",
                nominalEnd = "2026-09-01T10:00:00+02:00",
                windowStart = "2026-09-01T07:00:00+02:00",
                windowEnd = "2026-09-01T12:00:00+02:00",
                localDate = "2026-09-01",
                ordinal = 0,
                state = "generated",
            )
            val sourcePreview = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(
                        preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                    ),
                    occurrences = listOf(sourceOccurrence),
                ),
            )
            val plannerStore = PlannerStore(
                DayWeaveUiState(),
                nowEpochMillis = { clock.toEpochMilli() },
            )
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                    "cursor-1",
                    false,
                )
                previewResult = sourcePreview
            }
            val sourceManager = manager(plannerStore, transport)
            assertEquals(CanonicalRefreshOutcome.SUCCESS, sourceManager.refreshAndCompose())
            val previewCount = transport.previewRequests.size
            val moveStart = clock.plusSeconds(dayOffset * 86_400L + 3 * 3_600L)
            transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)

            val outcome = sourceManager.doLater(BLOCK_ID, moveStart)
            if (expectedSuccess) {
                assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
                assertEquals(previewCount + 1, transport.previewRequests.size)
                assertEquals(
                    moveStart.toString(),
                    plannerStore.state.value.recurrenceMoves.getValue(OCCURRENCE_ID).startAt,
                )
                val action = requireNotNull(
                    transport.previewRequests.last().recurrenceContext["exceptions"],
                ).jsonArray.single().jsonObject.getValue("action").jsonObject
                assertEquals("move", action.getValue("type").jsonPrimitive.content)
                assertEquals(moveStart.toString(), action.getValue("start").jsonPrimitive.content)
            } else {
                assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, outcome)
                assertTrue(plannerStore.state.value.recurrenceMoves.isEmpty())
                assertEquals(previewCount, transport.previewRequests.size)
                assertTrue(sourceManager.state.value.message.contains("exact firm horizon"))
            }
        }

        verify(dayOffset = 1, expectedSuccess = true)
        verify(dayOffset = 6, expectedSuccess = true)
        verify(dayOffset = 7, expectedSuccess = false)
    }

    @Test
    fun recurrenceMoveCrossingPlanningMidnightIsRejectedBeforeDurableMutation() = runBlocking {
        val recurringItem = remoteItem(split = false).copy(
            kind = "habit",
            recurrence = dailyRecurrence(),
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(
                        preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                    ),
                    occurrences = listOf(occurrence),
                ),
            )
        }
        val plannerStore = PlannerStore(DayWeaveUiState())
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val previewCount = transport.previewRequests.size

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.doLater(
                BLOCK_ID,
                Instant.parse("2026-09-01T21:30:00Z"),
            ),
        )

        assertTrue(plannerStore.state.value.recurrenceMoves.isEmpty())
        assertEquals(previewCount, transport.previewRequests.size)
        assertTrue(manager.state.value.message.contains("planning-day boundary"))
    }

    @Test
    fun customRecurrenceOccurrenceCannotBeMovedWithoutARealInstanceDiscriminator() = runBlocking {
        val recurringItem = remoteItem(split = false).copy(
            kind = "habit",
            recurrence = buildJsonObject {
                put("type", "custom")
                put("rrule", "FREQ=DAILY;COUNT=10")
            },
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = buildJsonObject { put("type", "custom") },
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = null,
            ordinal = 0,
            state = "generated",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(
                        preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                    ),
                    occurrences = listOf(occurrence),
                ),
            )
        }
        val plannerStore = PlannerStore(DayWeaveUiState())
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val previewCount = transport.previewRequests.size

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.doLater(BLOCK_ID, clock.plusSeconds(3 * 3_600L)),
        )

        assertTrue(plannerStore.state.value.recurrenceMoves.isEmpty())
        assertEquals(previewCount, transport.previewRequests.size)
        assertTrue(manager.state.value.message.contains("per-occurrence identity"))
    }

    @Test
    fun recurrenceMoveCannotShiftScheduledSiblingOfAuthoritativeOpenLease() = runBlocking {
        val first = preview().plan.blocks.single().copy(
            occurrenceId = OCCURRENCE_ID,
            end = "2026-09-01T09:30:00+02:00",
        )
        val second = first.copy(
            id = SECOND_BLOCK_ID,
            start = "2026-09-01T09:30:00+02:00",
            end = "2026-09-01T10:00:00+02:00",
            sessionIndex = 1,
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(
                    RemoteItemDeltaChange(
                        type = "upsert",
                        item = remoteItem().copy(recurrence = dailyRecurrence()),
                    ),
                ),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(first, second),
                    occurrences = listOf(occurrence),
                ),
            )
        }
        val initialStore = PlannerStore(DayWeaveUiState())
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(initialStore, transport).refreshAndCompose(),
        )
        val openLease = CanonicalExecutionSessionSnapshot(
            id = "99999999-9999-4999-8999-999999999999",
            itemId = TASK_ID,
            itemRevision = 7,
            occurrenceId = OCCURRENCE_ID,
            sessionIndex = 1,
            plannedBlockId = SECOND_BLOCK_ID,
            sourceDeviceId = "88888888-8888-4888-8888-888888888888",
            status = "active",
            revision = 1,
            accumulatedSeconds = 0,
            startedAt = clock.toString(),
            runningSince = clock.toString(),
            createdAt = clock.toString(),
            updatedAt = clock.toString(),
        )
        val guardedStore = PlannerStore(
            initialStore.state.value.copy(
                canonicalExecutionSyncOrigin = initialStore.state.value.canonicalSyncOrigin,
                canonicalExecutionConfigurationId =
                    initialStore.state.value.canonicalConfigurationId,
                canonicalExecutionSession = openLease,
            ),
        )
        val guardedManager = manager(guardedStore, transport)
        val previewCount = transport.previewRequests.size

        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            guardedManager.doLater(BLOCK_ID, clock.plusSeconds(4 * 3_600L)),
        )

        assertTrue(guardedStore.state.value.recurrenceMoves.isEmpty())
        assertEquals(openLease, guardedStore.state.value.canonicalExecutionSession)
        assertEquals(previewCount, transport.previewRequests.size)
        assertTrue(guardedManager.state.value.message.contains("active occurrence session"))
    }

    @Test
    fun recurrenceMoveRejectsPinnedSiblingAndPartialOccurrenceCapacity() = runBlocking {
        val recurringItem = remoteItem(split = false).copy(
            kind = "habit",
            recurrence = dailyRecurrence(),
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurringItem)),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(
                    blocks = listOf(
                        preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                    ),
                    occurrences = listOf(occurrence),
                ),
            )
        }
        val initialStore = PlannerStore(DayWeaveUiState())
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(initialStore, transport).refreshAndCompose(),
        )
        val focused = initialStore.state.value.schedule.single()
        val pinnedSibling = focused.copy(
            id = SECOND_BLOCK_ID,
            sessionIndex = 1,
            absoluteStartAt = "2026-09-01T08:00:00Z",
            absoluteEndAt = "2026-09-01T08:30:00Z",
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        val publishedProof = requireNotNull(initialStore.state.value.publishedScheduleProof)
        val pinnedSiblingProof = PublishedScheduleBlockProofSnapshot.from(pinnedSibling)
        val unsafeStates = listOf(
            initialStore.state.value.copy(
                schedule = initialStore.state.value.schedule + pinnedSibling,
                publishedScheduleProof = publishedProof.copy(
                    blocks = publishedProof.blocks + pinnedSiblingProof,
                ),
            ),
            initialStore.state.value.copy(
                unscheduledWork = listOf(
                    UnscheduledWorkSnapshot(
                        itemId = TASK_ID,
                        occurrenceId = OCCURRENCE_ID,
                        remainingMinutes = 15,
                        reason = "no_capacity",
                    ),
                ),
            ),
        )
        val previewCount = transport.previewRequests.size

        unsafeStates.forEach { unsafe ->
            val unsafeStore = PlannerStore(unsafe)
            val unsafeManager = manager(unsafeStore, transport)
            assertEquals(
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
                unsafeManager.doLater(BLOCK_ID, clock.plusSeconds(4 * 3_600L)),
            )
            assertTrue(unsafeStore.state.value.recurrenceMoves.isEmpty())
            assertTrue(
                unsafeManager.state.value.message,
                unsafeManager.state.value.message.contains("fully scheduled and flexible"),
            )
        }
        assertEquals(previewCount, transport.previewRequests.size)
    }

    @Test
    fun recurrenceMoveWithStaleSeriesRevisionIsNotSentAndIsClearedAfterRefresh() = runBlocking {
        val recurring = remoteItem(split = false).copy(
            kind = "habit",
            recurrence = dailyRecurrence(),
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            windowStart = "2026-09-01T07:00:00+02:00",
            windowEnd = "2026-09-01T12:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
            state = "generated",
        )
        fun occurrencePreview(revision: Long) = preview().copy(
            sourceItemRevisions = mapOf(TASK_ID to revision),
            plan = preview().plan.copy(
                blocks = listOf(
                    preview().plan.blocks.single().copy(occurrenceId = OCCURRENCE_ID),
                ),
                occurrences = listOf(occurrence),
            ),
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = recurring)),
                "cursor-1",
                false,
            )
            previewResult = occurrencePreview(7)
        }
        val initialStore = PlannerStore(DayWeaveUiState())
        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(initialStore, transport).refreshAndCompose(),
        )
        val staleMove = RecurrenceMoveSnapshot(
            itemId = TASK_ID,
            startAt = "2026-09-01T10:00:00Z",
            endAt = "2026-09-01T11:00:00Z",
            movedAt = "2026-09-01T07:00:00Z",
            source = requireNotNull(
                initialStore.state.value.recurrenceOccurrenceSources[OCCURRENCE_ID],
            ),
        )
        val movedStore = PlannerStore(
            initialStore.state.value.copy(recurrenceMoves = mapOf(OCCURRENCE_ID to staleMove)),
        )
        val revised = recurring.copy(revision = 8, updatedAt = "2026-09-01T07:01:00Z")
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = revised)),
            "cursor-2",
            false,
        )
        transport.previewResult = occurrencePreview(8)

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(movedStore, transport).refreshAndCompose(),
        )

        assertTrue(
            requireNotNull(transport.previewRequests.last().recurrenceContext["exceptions"])
                .jsonArray.isEmpty(),
        )
        assertTrue(movedStore.state.value.recurrenceMoves.isEmpty())
        assertEquals(
            8L,
            movedStore.state.value.recurrenceOccurrenceSources
                .getValue(OCCURRENCE_ID).itemRevision,
        )
    }

    @Test
    fun previewWithMultipleActiveSessionsForOneSplitItemIsRejected() = runBlocking {
        val first = preview().plan.blocks.single().copy(
            end = "2026-09-01T09:30:00+02:00",
        )
        val second = first.copy(
            id = SECOND_BLOCK_ID,
            start = "2026-09-01T09:30:00+02:00",
            end = "2026-09-01T10:00:00+02:00",
            sessionIndex = 1,
        )
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(
                    RemoteItemDeltaChange(
                        type = "upsert",
                        item = remoteItem().copy(status = "in_progress"),
                    ),
                ),
                "cursor-1",
                false,
            )
            previewResult = preview().copy(
                plan = preview().plan.copy(blocks = listOf(first, second)),
            )
        }

        assertEquals(
            CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        assertEquals(null, plannerStore.state.value.activeSession)
    }

    @Test
    fun equalRevisionTombstoneCannotEraseTheCachedCanonicalItem() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(
                RemoteItemDeltaChange(
                    type = "tombstone",
                    tombstone = RemoteItemTombstone(
                        id = TASK_ID,
                        revision = 7,
                        deletedAt = "2026-09-01T07:05:00Z",
                    ),
                ),
            ),
            "cursor-2",
            false,
        )

        assertEquals(CanonicalRefreshOutcome.PROTOCOL_FAILURE, manager.refreshAndCompose())
        assertEquals(7L, plannerStore.state.value.canonicalItems.single().revision)
        assertEquals("cursor-1", plannerStore.state.value.canonicalDeltaCursor)
    }

    @Test
    fun queuedCreateIsDurablySubmittedBeforeSendAndReconcilesExactResponse() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val mutationId = "99999999-9999-4999-8999-999999999999"
        val queued = requireNotNull(
            plannerStore.enqueueCanonicalCreate(authoredDraft(), TASK_ID, mutationId),
        )
        assertTrue(queued.persistence.awaitDurable())
        val created = authoredRemote(TASK_ID, revision = 1)
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(emptyList(), "authoring-empty", false)
            pages["authoring-empty"] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = created)),
                "authoring-created",
                false,
            )
            queuedPreviews += itemsPreview(emptyList())
            queuedPreviews += itemsPreview(listOf(created))
            createHandler = { idempotencyKey, request ->
                val durable = requireNotNull(plannerStore.canonicalAuthoringMutation(mutationId))
                assertTrue(durable.isSubmitted)
                assertEquals("https://api.example.test/", durable.syncOrigin)
                assertEquals("connection-1", durable.configurationId)
                assertEquals(durable.idempotencyKey, idempotencyKey)
                assertEquals(TASK_ID, request.id)
                created
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshAndCompose(),
        )

        assertTrue(plannerStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertEquals(created.id, plannerStore.state.value.canonicalItems.single().id)
        assertEquals(1, transport.createRequests.size)
        assertEquals("planned", transport.createRequests.single().second.status)
        assertNotNull(plannerStore.state.value.publishedScheduleRevision)
    }

    @Test
    fun queuedCreateSendsAndReconcilesTheExactRangedDurationContract() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val mutationId = "99999999-9999-4999-8999-999999999997"
        val draft = authoredDraft().copy(
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 2_400,
            durationSeconds = 3_600,
            durationMaxSeconds = 5_400,
            durationSource = CanonicalDurationSource.ASSISTANT,
        )
        assertTrue(
            requireNotNull(plannerStore.enqueueCanonicalCreate(draft, TASK_ID, mutationId))
                .persistence.awaitDurable(),
        )
        val created = authoredRemote(TASK_ID, revision = 1).copy(
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 2_400,
            durationMaxSeconds = 5_400,
            durationSource = CanonicalDurationSource.ASSISTANT,
            deadlineKind = CanonicalDeadlineKind.DATE_TIME,
            deadlineStrength = CanonicalDeadlineStrength.HARD,
            hasOwnEffort = false,
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(emptyList(), "range-empty", false)
            pages["range-empty"] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = created)),
                "range-created",
                false,
            )
            queuedPreviews += itemsPreview(emptyList())
            queuedPreviews += itemsPreview(listOf(created))
            createHandler = { _, request ->
                assertEquals(CanonicalDurationKind.RANGE, request.durationKind)
                assertEquals(2_400L, request.durationMinSeconds)
                assertEquals(3_600L, request.durationSeconds)
                assertEquals(5_400L, request.durationMaxSeconds)
                assertEquals(CanonicalDurationSource.ASSISTANT, request.durationSource)
                created
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshAndCompose(),
        )
        val cached = plannerStore.state.value.canonicalItems.single()
        assertEquals(CanonicalDurationKind.RANGE, cached.durationKind)
        assertEquals(CanonicalDurationSource.ASSISTANT, cached.durationSource)
        assertEquals(CanonicalDurationKind.RANGE, cached.toCanonicalDraft().durationKind)
    }

    @Test
    fun submittedLegacyCreateAndReplaceKeepTheirFrozenDurationRequestShape() = runBlocking {
        val createMutation = PendingCanonicalAuthoringMutation(
            id = "99999999-9999-4999-8999-999999999991",
            itemId = TASK_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = authoredDraft(),
            createdAt = "2026-09-01T06:00:00Z",
            durationRequestShapeVersion = PendingCanonicalAuthoringMutation
                .LEGACY_DURATION_REQUEST_SHAPE_VERSION,
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            submittedAt = "2026-09-01T07:00:00Z",
        ).also(PendingCanonicalAuthoringMutation::requireValid)
        val created = authoredRemote(TASK_ID, revision = 1)
        val createStore = PlannerStore(
            DayWeaveUiState(
                canonicalSyncOrigin = "https://api.example.test/",
                canonicalConfigurationId = "connection-1",
                pendingCanonicalAuthoringMutations = listOf(createMutation),
            ),
        )
        val createTransport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(emptyList(), "legacy-create-empty", false)
            pages["legacy-create-empty"] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = created)),
                "legacy-create-applied",
                false,
            )
            queuedPreviews += itemsPreview(emptyList())
            queuedPreviews += itemsPreview(listOf(created))
            createHandler = { _, request ->
                assertEquals(3_600L, request.durationSeconds)
                assertEquals(null, request.durationKind)
                assertEquals(null, request.durationMinSeconds)
                assertEquals(null, request.durationMaxSeconds)
                assertEquals(null, request.durationSource)
                created
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(createStore, createTransport).refreshAndCompose(),
        )
        assertTrue(createStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())

        val base = CanonicalItemSnapshot(
            id = TASK_ID,
            kind = "task",
            status = "planned",
            title = "Compose Android timeline",
            timezoneName = "Europe/Madrid",
            durationSeconds = 3_600,
            deadlineAt = "2026-09-01T12:00:00Z",
            flexibleConstraintsJson = "{}",
            splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 80,
            urgency = 60,
            siblingOrder = 0,
            isExecutable = true,
            revision = 7,
            createdAt = "2026-09-01T07:00:00Z",
            updatedAt = "2026-09-01T07:00:00Z",
        )
        val replacementDraft = base.requireCanonicalReplacementSupport().copy(
            title = "Legacy replacement",
        )
        val replaceMutation = PendingCanonicalAuthoringMutation(
            id = "99999999-9999-4999-8999-999999999992",
            itemId = TASK_ID,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = replacementDraft,
            expectedRevision = base.revision,
            baseItem = base,
            createdAt = "2026-09-01T06:00:00Z",
            durationRequestShapeVersion = PendingCanonicalAuthoringMutation
                .LEGACY_DURATION_REQUEST_SHAPE_VERSION,
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            submittedAt = "2026-09-01T07:00:00Z",
        ).also(PendingCanonicalAuthoringMutation::requireValid)
        val remoteBase = authoredRemote(TASK_ID, revision = base.revision)
        val replaced = authoredRemote(
            TASK_ID,
            revision = base.revision + 1,
            title = replacementDraft.title,
        )
        val replaceStore = PlannerStore(
            DayWeaveUiState(
                canonicalSyncOrigin = "https://api.example.test/",
                canonicalConfigurationId = "connection-1",
                canonicalItems = listOf(base),
                pendingCanonicalAuthoringMutations = listOf(replaceMutation),
            ),
        )
        val replaceTransport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteBase)),
                "legacy-replace-base",
                false,
            )
            pages["legacy-replace-base"] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = replaced)),
                "legacy-replace-applied",
                false,
            )
            queuedPreviews += itemsPreview(listOf(remoteBase))
            queuedPreviews += itemsPreview(listOf(replaced))
            replacementHandler = { _, _, request ->
                assertEquals(3_600L, request.item.durationSeconds)
                assertEquals(null, request.item.durationKind)
                assertEquals(null, request.item.durationMinSeconds)
                assertEquals(null, request.item.durationMaxSeconds)
                assertEquals(null, request.item.durationSource)
                replaced
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(replaceStore, replaceTransport).refreshAndCompose(),
        )
        assertTrue(replaceStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertEquals("Legacy replacement", replaceStore.state.value.canonicalItems.single().title)
    }

    @Test
    fun authoringSubmissionPersistenceFailurePreventsEveryNetworkSend() = runBlocking {
        val mutationId = "99999999-9999-4999-8999-999999999998"
        val seed = PlannerStore(DayWeaveUiState())
        requireNotNull(seed.enqueueCanonicalCreate(authoredDraft(), TASK_ID, mutationId))
        var durable = seed.state.value
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = durable

            override suspend fun save(state: DayWeaveUiState) {
                if (state.pendingCanonicalAuthoringMutations.any { it.isSubmitted }) {
                    throw IOException("synthetic authoring submission persistence failure")
                }
                durable = state
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val plannerStore = PlannerStore(DayWeaveUiState(), repository, scope)
            withTimeout(3_000) { plannerStore.loadState.first { it == PlannerLoadState.READY } }
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(emptyList(), "authoring-empty", false)
                previewResult = itemsPreview(emptyList())
            }

            assertEquals(
                CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
                manager(plannerStore, transport).refreshAndCompose(),
            )

            assertTrue(transport.createRequests.isEmpty())
            assertTrue(transport.authoringOperationIds.isEmpty())
            val durableMutation = durable.pendingCanonicalAuthoringMutations.single()
            assertFalse(durableMutation.isSubmitted)
            assertEquals("https://api.example.test/", durableMutation.syncOrigin)
            assertEquals(PlannerLoadState.PERSISTENCE_FAILED, plannerStore.loadState.value)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun parentCreateIsSentBeforeLexicallyEarlierChildMutation() = runBlocking {
        val parentId = "88888888-8888-4888-8888-888888888888"
        val childId = "99999999-9999-4999-8999-999999999999"
        val parentMutationId = "ffffffff-ffff-4fff-8fff-ffffffffffff"
        val childMutationId = "00000000-0000-4000-8000-000000000001"
        val plannerStore = PlannerStore(DayWeaveUiState())
        requireNotNull(
            plannerStore.enqueueCanonicalCreate(
                authoredDraft(title = "Parent"),
                parentId,
                parentMutationId,
            ),
        )
        requireNotNull(
            plannerStore.enqueueCanonicalCreate(
                authoredDraft(title = "Child").copy(parentId = parentId),
                childId,
                childMutationId,
            ),
        )
        val parentCreated = authoredRemote(parentId, revision = 1, title = "Parent")
        val childCreated = authoredRemote(
            childId,
            revision = 1,
            title = "Child",
            parentId = parentId,
        )
        val parentRefreshed = parentCreated.copy(
            revision = 2,
            isExecutable = false,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(emptyList(), "authoring-empty", false)
            pages["authoring-empty"] = RemoteItemDeltaPage(
                listOf(
                    RemoteItemDeltaChange(type = "upsert", item = parentRefreshed),
                    RemoteItemDeltaChange(type = "upsert", item = childCreated),
                ),
                "authoring-tree",
                false,
            )
            queuedPreviews += itemsPreview(emptyList())
            queuedPreviews += itemsPreview(listOf(parentRefreshed, childCreated))
            createHandler = { _, request ->
                when (request.id) {
                    parentId -> parentCreated
                    childId -> childCreated
                    else -> error("unexpected create")
                }
            }
        }

        assertEquals(
            CanonicalRefreshOutcome.SUCCESS,
            manager(plannerStore, transport).refreshAndCompose(),
        )

        assertEquals(listOf(parentId, childId), transport.authoringOperationIds)
        assertTrue(plannerStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertEquals(2L, plannerStore.state.value.canonicalItems.first { it.id == parentId }.revision)
    }

    @Test
    fun childTrashRefreshesAndRebasesQueuedParentTrashBeforeItsFirstSend() = runBlocking {
        val parentId = "88888888-8888-4888-8888-888888888888"
        val childId = "99999999-9999-4999-8999-999999999999"
        val parent = authoredRemote(
            id = parentId,
            revision = 1,
            title = "Parent",
            isExecutable = false,
        )
        val child = authoredRemote(
            id = childId,
            revision = 1,
            title = "Child",
            parentId = parentId,
        )
        val parentRefreshed = parent.copy(
            revision = 2,
            isExecutable = true,
            updatedAt = "2026-09-01T07:01:00Z",
        )
        val childDeleted = child.copy(
            revision = 2,
            isExecutable = false,
            updatedAt = "2026-09-01T07:01:00Z",
            deletedAt = "2026-09-01T07:01:00Z",
        )
        val parentDeleted = parentRefreshed.copy(
            revision = 3,
            isExecutable = false,
            updatedAt = "2026-09-01T07:02:00Z",
            deletedAt = "2026-09-01T07:02:00Z",
        )
        val transport = FakeCanonicalTransport().apply {
            queuedPages[null] = ArrayDeque(
                listOf(
                    RemoteItemDeltaPage(
                        listOf(
                            RemoteItemDeltaChange(type = "upsert", item = parent),
                            RemoteItemDeltaChange(type = "upsert", item = child),
                        ),
                        "hierarchy-cursor-1",
                        false,
                    ),
                    RemoteItemDeltaPage(
                        listOf(RemoteItemDeltaChange(type = "upsert", item = parentRefreshed)),
                        "hierarchy-cursor-3",
                        false,
                    ),
                ),
            )
            pages["hierarchy-cursor-1"] = RemoteItemDeltaPage(
                emptyList(),
                "hierarchy-cursor-2",
                false,
            )
            pages["hierarchy-cursor-3"] = RemoteItemDeltaPage(
                listOf(
                    RemoteItemDeltaChange(
                        type = "tombstone",
                        tombstone = RemoteItemTombstone(
                            id = parentId,
                            revision = 3,
                            deletedAt = "2026-09-01T07:02:00Z",
                        ),
                    ),
                ),
                "hierarchy-cursor-4",
                false,
            )
            queuedPreviews += itemsPreview(listOf(parent, child))
            queuedPreviews += itemsPreview(listOf(parent, child))
            queuedPreviews += itemsPreview(listOf(parentRefreshed))
            queuedPreviews += itemsPreview(emptyList())
            trashHandler = { request ->
                when (request.id) {
                    childId -> {
                        assertEquals(1L, request.expectedRevision)
                        childDeleted
                    }
                    parentId -> {
                        assertEquals(2L, request.expectedRevision)
                        parentDeleted
                    }
                    else -> error("unexpected trash request")
                }
            }
        }
        val plannerStore = PlannerStore(DayWeaveUiState())
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        requireNotNull(
            plannerStore.enqueueCanonicalTrash(
                childId,
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            ),
        ).persistence.awaitDurable()
        requireNotNull(
            plannerStore.enqueueCanonicalTrash(
                parentId,
                "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            ),
        ).persistence.awaitDurable()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(listOf(childId, parentId), transport.authoringOperationIds)
        assertEquals(listOf(1L, 2L), transport.trashRequests.map { it.expectedRevision })
        assertTrue(plannerStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
    }

    @Test
    fun trustedServerRejectionBecomesDurableVisibleConflictAndRebuildsCanonicalCache() =
        runBlocking {
            val plannerStore = PlannerStore(DayWeaveUiState())
            val transport = FakeCanonicalTransport().apply {
                pages[null] = RemoteItemDeltaPage(
                    listOf(
                        RemoteItemDeltaChange(
                            type = "upsert",
                            item = remoteItem().copy(
                                flexibleConstraints = buildJsonObject { put("energy", "deep") },
                            ),
                        ),
                    ),
                    "cursor-1",
                    false,
                )
                previewResult = preview()
            }
            val manager = manager(plannerStore, transport)
            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
            val mutationId = "99999999-9999-4999-8999-999999999997"
            requireNotNull(
                plannerStore.enqueueCanonicalReplace(
                    TASK_ID,
                    authoredDraft(title = "Retained local edit"),
                    mutationId,
                ),
            )
            transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
            transport.replacementError = PlannerApiException.CanonicalMutationRejected()
            transport.queuedPreviews += preview()
            transport.queuedPreviews += preview()

            assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

            val conflict = requireNotNull(plannerStore.canonicalAuthoringMutation(mutationId))
            assertTrue(conflict.isSubmitted)
            assertEquals(CanonicalAuthoringDisposition.CONFLICTED, conflict.disposition)
            assertTrue(requireNotNull(conflict.diagnostic).contains("Review"))
            assertEquals(7L, plannerStore.state.value.canonicalItems.single().revision)
            assertEquals("cursor-1", plannerStore.state.value.canonicalDeltaCursor)
            assertNotNull(plannerStore.state.value.publishedScheduleRevision)
        }

    @Test
    fun customAnchorValidationPersistsAClientOwnedRruleRepairDiagnostic() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(
                    RemoteItemDeltaChange(
                        type = "upsert",
                        item = authoredRemote(TASK_ID, revision = 7),
                    ),
                ),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        val mutationId = "99999999-9999-4999-8999-999999999995"
        val customDraft = authoredDraft().copy(
            recurrence = CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.CUSTOM,
                rrule = "FREQ=DAILY;UNTIL=20260905",
            ),
        )
        requireNotNull(
            plannerStore.enqueueCanonicalReplace(TASK_ID, customDraft, mutationId),
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
        transport.replacementError = PlannerApiException.Validation(
            422,
            PlannerValidationReason.CUSTOM_RECURRENCE_ANCHOR,
        )
        transport.queuedPreviews += preview()
        transport.queuedPreviews += preview()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        val conflict = requireNotNull(plannerStore.canonicalAuthoringMutation(mutationId))
        assertEquals(CanonicalAuthoringDisposition.CONFLICTED, conflict.disposition)
        assertEquals(
            "This custom RRULE cannot produce an occurrence from the server-assigned creation " +
                "date under every week-start setting. Extend UNTIL or adjust INTERVAL, BYDAY, " +
                "or BYMONTHDAY, then retry or discard the retained change.",
            conflict.diagnostic,
        )
    }

    @Test
    fun queuedTrashAndRestoreReconcileDeletedAndActiveResponses() = runBlocking {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem())),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        val trashMutationId = "99999999-9999-4999-8999-999999999996"
        requireNotNull(
            plannerStore.enqueueCanonicalTrash(TASK_ID, trashMutationId),
        ).persistence.awaitDurable()
        val deleted = remoteItem().copy(
            revision = 8,
            updatedAt = "2026-09-01T07:05:00Z",
            deletedAt = "2026-09-01T07:05:00Z",
        )
        transport.pages["cursor-1"] = RemoteItemDeltaPage(emptyList(), "cursor-2", false)
        transport.pages["cursor-2"] = RemoteItemDeltaPage(
            listOf(
                RemoteItemDeltaChange(
                    type = "tombstone",
                    tombstone = RemoteItemTombstone(
                        id = TASK_ID,
                        revision = 8,
                        deletedAt = "2026-09-01T07:05:00Z",
                    ),
                ),
            ),
            "cursor-3",
            false,
        )
        transport.queuedPreviews += preview()
        transport.queuedPreviews += itemsPreview(emptyList())
        transport.trashHandler = { request ->
            val durable = requireNotNull(plannerStore.canonicalAuthoringMutation(trashMutationId))
            assertTrue(durable.isSubmitted)
            assertEquals(durable.idempotencyKey, request.idempotencyKey)
            assertEquals(7L, request.expectedRevision)
            deleted
        }

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertEquals(8L, plannerStore.state.value.canonicalRecentlyDeleted.single().revision)
        assertEquals(1, transport.trashRequests.size)

        val restoreMutationId = "99999999-9999-4999-8999-999999999995"
        requireNotNull(
            plannerStore.enqueueCanonicalRestore(TASK_ID, restoreMutationId),
        ).persistence.awaitDurable()
        val restored = remoteItem().copy(
            revision = 9,
            updatedAt = "2026-09-01T07:06:00Z",
        )
        transport.pages["cursor-3"] = RemoteItemDeltaPage(emptyList(), "cursor-4", false)
        transport.pages["cursor-4"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = restored)),
            "cursor-5",
            false,
        )
        transport.queuedPreviews += itemsPreview(emptyList())
        transport.queuedPreviews += scheduledPreview(restored)
        transport.restoreHandler = { request ->
            val durable = requireNotNull(plannerStore.canonicalAuthoringMutation(restoreMutationId))
            assertTrue(durable.isSubmitted)
            assertEquals(durable.idempotencyKey, request.idempotencyKey)
            assertEquals(8L, request.request.expectedRevision)
            restored
        }

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertEquals(9L, plannerStore.state.value.canonicalItems.single().revision)
        assertTrue(plannerStore.state.value.canonicalRecentlyDeleted.isEmpty())
        assertTrue(plannerStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertEquals(1, transport.restoreRequests.size)
    }

    @Test
    fun localCompositionInstallsOnlyEncryptedDisplayProvenanceWithoutNetworkAuthority() =
        runBlocking {
            val initial = localCompositionReadyState()
            val plannerStore = PlannerStore(initial)
            val transport = FakeCanonicalTransport()
            var request: SchedulePreviewRequest? = null
            val composer = LocalScheduleComposer { _, incoming ->
                request = incoming
                emptyLocalComposition(incoming)
            }

            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(
                    plannerStore,
                    transport,
                    localScheduleComposer = composer,
                ).composeLocally(),
            )

            val installed = plannerStore.state.value
            val provenance = requireNotNull(installed.localScheduleCompositionProvenance)
            assertTrue(provenance.matchesState(installed))
            assertTrue(installed.isScheduleDisplayCurrent(clock, ZoneId.of("Europe/Madrid")))
            assertFalse(installed.isCanonicalPlanCurrent(clock, ZoneId.of("Europe/Madrid")))
            assertEquals(initial.canonicalDeltaCursor, installed.canonicalDeltaCursor)
            assertEquals(initial.canonicalExecutionRevision, installed.canonicalExecutionRevision)
            assertEquals(null, installed.scheduleInputDigest)
            assertEquals(null, installed.publishedScheduleRevision)
            assertEquals(null, installed.publishedScheduleProof)
            assertEquals(7 * 60, installed.scheduleCompositionProfile.dayStartMinute)
            assertEquals(7, request?.availability?.size)
            assertEquals("2026-09-01T05:00:00Z", request?.availability?.first()?.start)
            assertTrue(transport.deltaCursors.isEmpty())
            assertTrue(transport.previewRequests.isEmpty())
            assertTrue(transport.publicationRequests.isEmpty())
        }

    @Test
    fun localCompositionKeepsConfiguredProfileZoneAcrossDeviceZoneFence() = runBlocking {
        val weekly = requireNotNull(
            ScheduleCompositionProfileSnapshot().upgradedToWeeklySchedule("Europe/Paris"),
        )
        val initial = localCompositionReadyState().copy(scheduleCompositionProfile = weekly)
        val plannerStore = PlannerStore(initial)
        var captured: SchedulePreviewRequest? = null
        val composer = LocalScheduleComposer { _, request ->
            captured = request
            emptyLocalComposition(request)
        }

        val outcome = manager(
            plannerStore = plannerStore,
            transport = FakeCanonicalTransport(),
            zoneProvider = { ZoneId.of("America/Los_Angeles") },
            localScheduleComposer = composer,
        ).composeLocally()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
        assertEquals("Europe/Paris", captured?.timezoneName)
        assertEquals(15, captured?.fixedBlocks?.size)
        assertTrue(
            plannerStore.state.value.isScheduleDisplayCurrent(
                clock,
                ZoneId.of("America/Los_Angeles"),
            ),
        )
    }

    @Test
    fun localAdapterRequestFailuresUseFixedProtocolOutcomeWithoutMutationOrNetwork() = runBlocking {
        listOf<() -> RuntimeException>(
            { LocalScheduleCompositionRequestTooLargeException() },
            { LocalScheduleCompositionRequestException() },
        ).forEach { failure ->
            val initial = localCompositionReadyState()
            val plannerStore = PlannerStore(initial)
            val transport = FakeCanonicalTransport()
            val manager = manager(
                plannerStore,
                transport,
                localScheduleComposer = LocalScheduleComposer { _, _ -> throw failure() },
            )

            assertEquals(CanonicalRefreshOutcome.PROTOCOL_FAILURE, manager.composeLocally())
            assertEquals(
                "The bundled scheduler rejected or returned an invalid local composition.",
                manager.state.value.message,
            )
            assertEquals(initial, plannerStore.state.value)
            assertEquals(initial, plannerStore.durableState.value)
            assertTrue(transport.deltaCursors.isEmpty())
            assertTrue(transport.previewRequests.isEmpty())
            assertTrue(transport.publicationRequests.isEmpty())
        }
    }

    @Test
    fun nonEmptyLocalCompositionMapsCanonicalBlockButKeepsEveryActionFailClosed() = runBlocking {
        val item = localCanonicalItem()
        val initial = localCompositionReadyState().copy(canonicalItems = listOf(item))
        val plannerStore = PlannerStore(initial)
        val manager = manager(
            plannerStore,
            FakeCanonicalTransport(),
            localScheduleComposer = LocalScheduleComposer { _, request ->
                LocalScheduleComposition(
                    localInputFingerprint = "local-sha256:${"c".repeat(64)}",
                    scheduleRequestFingerprint = "sha256:${"d".repeat(64)}",
                    sourceItemCount = 1,
                    sourceItemRevisions = mapOf(item.id to item.revision),
                    acceptedItemCount = 1,
                    rejectedItems = emptyList(),
                    ignoredPreviousAssignments = emptyList(),
                    plan = RemoteSchedulePlan(
                        asOf = request.asOf,
                        horizonStart = request.horizonStart,
                        horizonEnd = request.horizonEnd,
                        blocks = listOf(
                            RemoteScheduleBlock(
                                id = BLOCK_ID,
                                isSensitive = false,
                                itemId = item.id,
                                title = item.title,
                                start = "2026-09-01T08:00:00Z",
                                end = "2026-09-01T08:30:00Z",
                                sessionIndex = 0,
                                kind = "planned",
                                explanations = emptyList(),
                            ),
                        ),
                        unscheduled = emptyList(),
                        decisions = emptyList(),
                        violations = emptyList(),
                        score = RemotePlanScore(30, 0, 0uL, 0),
                        occurrences = emptyList(),
                    ),
                )
            },
        )

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.composeLocally())
        val block = plannerStore.state.value.schedule.single()
        assertEquals(item.id, block.canonicalItemId)
        assertEquals(item.revision, block.canonicalRevision)
        assertEquals(0, block.sessionIndex)
        assertFalse(plannerStore.state.value.hasPublishedExecutionAuthority(block))
        assertTrue(plannerStore.isCanonicalExecutionStartBlocked(block.id))
        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            manager.skipScheduled(block.id),
        )
        assertEquals(ItemStatus.SCHEDULED, plannerStore.state.value.schedule.single().status)
    }

    @Test
    fun localCompositionRequiresExactVerifiedExecutionBaselineBeforeNativeCall() = runBlocking {
        val invalidStates = listOf(
            localCompositionReadyState().copy(canonicalExecutionSyncOrigin = null),
            localCompositionReadyState().copy(
                canonicalExecutionConfigurationId = "other-binding",
            ),
            localCompositionReadyState().copy(canonicalExecutionHistoryVerified = false),
            localCompositionReadyState().copy(
                canonicalExecutionHistoryContinuityEstablished = false,
            ),
        )
        invalidStates.forEach { initial ->
            val plannerStore = PlannerStore(initial)
            var composeCalls = 0
            val outcome = manager(
                plannerStore,
                FakeCanonicalTransport(),
                localScheduleComposer = LocalScheduleComposer { _, request ->
                    composeCalls += 1
                    emptyLocalComposition(request)
                },
            ).composeLocally()

            assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, outcome)
            assertEquals(0, composeCalls)
            assertEquals(initial, plannerStore.state.value)
        }
    }

    @Test
    fun localHabitCompositionSerializesMissedSkipAndReductionAsDemandExceptions() = runBlocking {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val habit = localCanonicalItem().copy(
            kind = "habit",
            title = "Renamed without changing policy",
            recurrenceJson = """{"type":"daily","times_per_day":1}""",
            revision = 8,
        )
        val sourceEvidence = HabitOccurrenceEvidenceSnapshot(
            id = HABIT_LEDGER_OCCURRENCE_ID,
            habitId = habit.id,
            plannerOccurrenceId = OCCURRENCE_ID,
            sourceScheduleRevisionId = HABIT_SOURCE_SCHEDULE_ID,
            sourceItemRevision = 7,
            policyFingerprint = requireNotNull(habit.habitPolicyFingerprintOrNull()),
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T05:00:00Z",
            nominalEnd = "2026-09-01T05:30:00Z",
            windowStart = "2026-09-01T04:00:00Z",
            windowEnd = "2026-09-01T06:00:00Z",
            localDate = "2026-09-01",
            timezoneName = "Europe/Madrid",
            expectedDurationSeconds = 1_800,
            expectedQuantity = null,
            expectedUnit = null,
        )
        val partial = HabitOutcomeSnapshot(
            revision = 1,
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressBasisPoints = 5_000,
            quantity = null,
            unit = null,
            actualSeconds = 900,
            note = null,
            occurredAt = "2026-09-01T05:30:00Z",
            updatedAt = "2026-09-01T05:31:00Z",
        )
        fun resolution(action: HabitMissedResolutionActionSnapshot) =
            HabitMissedResolutionSnapshot(
                occurrenceEvidenceId = sourceEvidence.id,
                habitId = habit.id,
                sourcePlannerOccurrenceId = sourceEvidence.plannerOccurrenceId,
                revision = 2,
                configuredPolicy = HabitMissedPolicySnapshot.ASK,
                action = action,
                createdAt = "2026-09-01T06:01:00Z",
                updatedAt = "2026-09-01T06:02:00Z",
            )
        val targetEvidence = sourceEvidence.copy(
            id = REDUCED_HABIT_LEDGER_OCCURRENCE_ID,
            plannerOccurrenceId = REDUCED_HABIT_PLANNER_OCCURRENCE_ID,
            identity = buildJsonObject {
                put("type", "calendar_day")
                put("date", "2026-09-02")
                put("bucket_ordinal", 0)
            },
            nominalStart = "2026-09-02T05:00:00Z",
            nominalEnd = "2026-09-02T05:30:00Z",
            windowStart = "2026-09-02T04:00:00Z",
            windowEnd = "2026-09-02T06:00:00Z",
            localDate = "2026-09-02",
        )
        val targetPartial = partial.copy(
            occurredAt = "2026-09-02T05:30:00Z",
            updatedAt = "2026-09-02T05:31:00Z",
        )
        val targetCarryResolution = HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = targetEvidence.id,
            habitId = habit.id,
            sourcePlannerOccurrenceId = targetEvidence.plannerOccurrenceId,
            revision = 2,
            configuredPolicy = HabitMissedPolicySnapshot.ASK,
            action = HabitMissedResolutionActionSnapshot.Carry(
                windowStart = "2026-09-02T07:00:00Z",
                windowEnd = "2026-09-02T07:30:00Z",
            ),
            createdAt = "2026-09-02T06:59:00Z",
            updatedAt = "2026-09-02T07:00:00Z",
        )
        val membershipRevision = PublishedScheduleRevisionSnapshot(
            id = HABIT_SOURCE_SCHEDULE_ID,
            revisionNumber = 1uL,
            revision = "1:$HABIT_SOURCE_SCHEDULE_ID",
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = "2026-09-01T00:00:00Z",
            horizonEnd = "2026-09-08T00:00:00Z",
            timezoneName = "Europe/Madrid",
            publishedAt = "2026-09-01T00:00:00Z",
        )

        fun storedMove(
            evidence: HabitOccurrenceEvidenceSnapshot,
            startAt: String,
            endAt: String,
            movedAt: String,
        ) = RecurrenceMoveSnapshot(
            itemId = habit.id,
            startAt = startAt,
            endAt = endAt,
            movedAt = movedAt,
            source = RecurrenceOccurrenceSourceSnapshot(
                itemId = habit.id,
                itemRevision = habit.revision,
                identityJson = evidence.identity.toString(),
                nominalStart = evidence.nominalStart,
                nominalEnd = evidence.nominalEnd,
                localDate = evidence.localDate,
                ordinal = 0,
            ),
        )

        suspend fun capturedRequest(
            occurrences: List<HabitOccurrenceSnapshot>,
            recurrenceMoves: Map<String, RecurrenceMoveSnapshot> = emptyMap(),
            recurrenceOutcomes: Map<String, RecurrenceOutcomeSnapshot> = emptyMap(),
            pauses: Map<String, HabitPauseSnapshot> = emptyMap(),
            canonicalHabit: CanonicalItemSnapshot = habit,
        ): SchedulePreviewRequest {
            val membership = occurrences
                .distinctBy { it.evidence.plannerOccurrenceId }
                .map { occurrence ->
                    PublishedOccurrenceMembershipSnapshot(
                        plannerOccurrenceId = occurrence.evidence.plannerOccurrenceId,
                        seriesItemId = occurrence.evidence.habitId,
                        state = PublishedOccurrenceStateSnapshot.GENERATED,
                    )
                }
                .sortedBy(PublishedOccurrenceMembershipSnapshot::plannerOccurrenceId)
            val state = localCompositionReadyState().copy(
                canonicalItems = listOf(canonicalHabit),
                recurrenceMoves = recurrenceMoves,
                recurrenceOutcomes = recurrenceOutcomes,
                habitLedger = HabitLedgerSnapshot(
                    syncOrigin = origin,
                    configurationId = configurationId,
                    deltaCursor = "habit-cursor",
                    deltaCaughtUp = true,
                    occurrences = occurrences.associateBy { it.evidence.id },
                    pauses = pauses,
                ).also(HabitLedgerSnapshot::requireValid),
                publishedOccurrenceMembershipProof =
                    PublishedOccurrenceMembershipProofSnapshot(
                        schemaVersion =
                            PublishedOccurrenceMembershipProofSnapshot.CURRENT_SCHEMA_VERSION,
                        syncOrigin = origin,
                        configurationId = configurationId,
                        revision = membershipRevision,
                        occurrences = membership,
                    ),
                publishedScheduleRevisionHint = PublishedScheduleRevisionHintSnapshot(
                    syncOrigin = origin,
                    configurationId = configurationId,
                    revisionNumber = membershipRevision.revisionNumber,
                ),
            )
            var captured: SchedulePreviewRequest? = null
            val outcome = manager(
                PlannerStore(state),
                FakeCanonicalTransport(),
                localScheduleComposer = LocalScheduleComposer { _, request ->
                    captured = request
                    emptyLocalComposition(request).copy(
                        sourceItemCount = 1,
                        sourceItemRevisions = mapOf(habit.id to habit.revision),
                        rejectedItems = listOf(
                            RemoteRejectedScheduleItem(
                                itemId = habit.id,
                                isSensitive = habit.isSensitive,
                                title = habit.title,
                                reason = "Synthetic unsupported habit",
                            ),
                        ),
                    )
                },
            ).composeLocally()
            assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
            return requireNotNull(captured)
        }

        fun exceptionActions(request: SchedulePreviewRequest): List<Pair<String, String>> =
            request.recurrenceContext["exceptions"]?.jsonArray.orEmpty().map { exception ->
                val value = exception.jsonObject
                value.getValue("selector").jsonObject.getValue("id").jsonPrimitive.content to
                    value.getValue("action").jsonObject.getValue("type").jsonPrimitive.content
            }

        val reduction = resolution(
            HabitMissedResolutionActionSnapshot.ReduceFrequency(
                listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
            ),
        )
        val targetWithOwnCarry = HabitOccurrenceSnapshot(
            targetEvidence,
            outcome = null,
            missedResolution = targetCarryResolution,
        )
        val completedSource = partial.copy(
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressBasisPoints = 10_000,
        )
        val sourcePause = HabitPauseSnapshot(
            id = HABIT_PAUSE_ID,
            habitId = habit.id,
            revision = 1,
            startedAt = "2026-09-01T04:30:00Z",
            endedAt = "2026-09-01T06:30:00Z",
            preservesStreak = true,
            createdAt = "2026-09-01T04:30:00Z",
            updatedAt = "2026-09-01T06:30:00Z",
        )
        val sourceSpecificInvalidations = listOf(
            capturedRequest(
                listOf(
                    HabitOccurrenceSnapshot(sourceEvidence, completedSource, reduction),
                    targetWithOwnCarry,
                ),
            ),
            capturedRequest(
                listOf(
                    HabitOccurrenceSnapshot(
                        sourceEvidence,
                        outcome = null,
                        missedResolution = reduction,
                    ),
                    targetWithOwnCarry,
                ),
                pauses = mapOf(sourcePause.id to sourcePause),
            ),
            capturedRequest(
                listOf(
                    HabitOccurrenceSnapshot(
                        sourceEvidence.copy(policyFingerprint = "sha256:${"f".repeat(64)}"),
                        outcome = null,
                        missedResolution = reduction,
                    ),
                    targetWithOwnCarry,
                ),
            ),
            capturedRequest(
                listOf(
                    HabitOccurrenceSnapshot(
                        sourceEvidence.copy(sourceItemRevision = habit.revision + 1),
                        outcome = null,
                        missedResolution = reduction,
                    ),
                    targetWithOwnCarry,
                ),
            ),
        )
        sourceSpecificInvalidations.forEach { request ->
            assertEquals(
                listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to "move"),
                exceptionActions(request),
            )
        }
        val activeReductionOverridesTargetsOwnCarry = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = reduction,
                ),
                targetWithOwnCarry,
            ),
        )
        assertEquals(
            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to "skip"),
            exceptionActions(activeReductionOverridesTargetsOwnCarry),
        )
        listOf(
            habit.copy(status = "blocked"),
            habit.copy(status = "future_status"),
            habit.copy(isExecutable = false),
        ).forEach { inactiveHabit ->
            val inactiveRequest = capturedRequest(
                listOf(
                    HabitOccurrenceSnapshot(
                        sourceEvidence,
                        outcome = null,
                        missedResolution = reduction,
                    ),
                    targetWithOwnCarry,
                ),
                canonicalHabit = inactiveHabit,
            )
            assertTrue(exceptionActions(inactiveRequest).isEmpty())
        }

        listOf(
            targetEvidence.copy(policyFingerprint = "sha256:${"e".repeat(64)}"),
            targetEvidence.copy(sourceItemRevision = habit.revision + 1),
        ).forEach { invalidTargetEvidence ->
            val request = capturedRequest(
                listOf(
                    HabitOccurrenceSnapshot(
                        sourceEvidence,
                        outcome = null,
                        missedResolution = reduction,
                    ),
                    HabitOccurrenceSnapshot(
                        invalidTargetEvidence,
                        outcome = null,
                        missedResolution = null,
                    ),
                ),
            )
            assertTrue(exceptionActions(request).isEmpty())
        }

        val skipRequest = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    partial,
                    resolution(HabitMissedResolutionActionSnapshot.Skip),
                ),
            ),
        )
        assertEquals(
            setOf(OCCURRENCE_ID),
            requireNotNull(skipRequest.recurrenceContext["exceptions"]).jsonArray
                .mapTo(mutableSetOf()) {
                    it.jsonObject.getValue("selector").jsonObject.getValue("id")
                        .jsonPrimitive.content
                },
        )
        assertNull(skipRequest.recurrenceContext["partial_progress"])

        fun carryResolution(windowEnd: String) = HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = sourceEvidence.id,
            habitId = habit.id,
            sourcePlannerOccurrenceId = sourceEvidence.plannerOccurrenceId,
            revision = 2,
            configuredPolicy = HabitMissedPolicySnapshot.ASK,
            action = HabitMissedResolutionActionSnapshot.Carry(
                windowStart = clock.toString(),
                windowEnd = windowEnd,
            ),
            createdAt = "2026-09-01T06:01:00Z",
            updatedAt = clock.toString(),
        )
        val containedCarry = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    partial,
                    carryResolution("2026-09-01T07:30:00Z"),
                ),
            ),
        )
        assertEquals(
            "move",
            requireNotNull(containedCarry.recurrenceContext["exceptions"]).jsonArray.single()
                .jsonObject.getValue("action").jsonObject.getValue("type").jsonPrimitive.content,
        )
        val containedMoveSource = requireNotNull(
            containedCarry.recurrenceContext["exceptions"],
        ).jsonArray.single().jsonObject.getValue("action").jsonObject
            .getValue("source").jsonObject
        assertEquals(
            habit.revision.toString(),
            containedMoveSource.getValue("item_revision").jsonPrimitive.content,
        )
        assertNotNull(containedCarry.recurrenceContext["partial_progress"])

        val outsideCarry = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    partial,
                    carryResolution("2026-09-08T00:00:00Z"),
                ),
            ),
        )
        val outsideException = requireNotNull(
            outsideCarry.recurrenceContext["exceptions"],
        ).jsonArray.single().jsonObject
        assertEquals(
            OCCURRENCE_ID,
            outsideException.getValue("selector").jsonObject.getValue("id")
                .jsonPrimitive.content,
        )
        assertEquals(
            "skip",
            outsideException.getValue("action").jsonObject.getValue("type")
                .jsonPrimitive.content,
        )
        assertNull(outsideCarry.recurrenceContext["partial_progress"])

        val reductionRequest = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = resolution(
                        HabitMissedResolutionActionSnapshot.ReduceFrequency(
                            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
                        ),
                    ),
                ),
                HabitOccurrenceSnapshot(targetEvidence, outcome = null),
            ),
        )
        assertEquals(
            setOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
            requireNotNull(reductionRequest.recurrenceContext["exceptions"]).jsonArray
                .mapTo(mutableSetOf()) {
                    it.jsonObject.getValue("selector").jsonObject.getValue("id")
                        .jsonPrimitive.content
                },
        )
        assertNull(reductionRequest.recurrenceContext["partial_progress"])

        val chainTargetEvidence = targetEvidence.copy(
            id = "12121212-1212-4212-8212-121212121212",
            plannerOccurrenceId = "13131313-1313-5313-8313-131313131313",
            identity = buildJsonObject {
                put("type", "calendar_day")
                put("date", "2026-09-03")
                put("bucket_ordinal", 0)
            },
            nominalStart = "2026-09-03T05:00:00Z",
            nominalEnd = "2026-09-03T05:30:00Z",
            windowStart = "2026-09-03T04:00:00Z",
            windowEnd = "2026-09-03T06:00:00Z",
            localDate = "2026-09-03",
        )
        fun reductionFor(
            evidence: HabitOccurrenceEvidenceSnapshot,
            targetPlannerId: String,
        ) = HabitMissedResolutionSnapshot(
            occurrenceEvidenceId = evidence.id,
            habitId = evidence.habitId,
            sourcePlannerOccurrenceId = evidence.plannerOccurrenceId,
            revision = 2,
            configuredPolicy = HabitMissedPolicySnapshot.ASK,
            action = HabitMissedResolutionActionSnapshot.ReduceFrequency(listOf(targetPlannerId)),
            createdAt = evidence.windowEnd,
            updatedAt = evidence.windowEnd,
        )
        val chainSource = HabitOccurrenceSnapshot(
            sourceEvidence,
            outcome = null,
            missedResolution = reductionFor(
                sourceEvidence,
                targetEvidence.plannerOccurrenceId,
            ),
        )
        val chainMiddle = HabitOccurrenceSnapshot(
            targetEvidence,
            outcome = null,
            missedResolution = reductionFor(
                targetEvidence,
                chainTargetEvidence.plannerOccurrenceId,
            ),
        )
        val chainTarget = HabitOccurrenceSnapshot(chainTargetEvidence, outcome = null)
        assertEquals(
            listOf(targetEvidence.plannerOccurrenceId to "skip"),
            exceptionActions(capturedRequest(listOf(chainTarget, chainMiddle, chainSource))),
        )
        assertEquals(
            listOf(chainTargetEvidence.plannerOccurrenceId to "skip"),
            exceptionActions(capturedRequest(listOf(
                chainTarget,
                chainMiddle,
                chainSource.copy(outcome = completedSource),
            ))),
        )

        val missingTargetRequest = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = resolution(
                        HabitMissedResolutionActionSnapshot.ReduceFrequency(
                            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
                        ),
                    ),
                ),
            ),
        )
        assertTrue(
            missingTargetRequest.recurrenceContext["exceptions"] == null ||
                requireNotNull(missingTargetRequest.recurrenceContext["exceptions"])
                    .jsonArray.isEmpty(),
        )

        val targetPrecedenceRequest = capturedRequest(
            listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = resolution(
                        HabitMissedResolutionActionSnapshot.ReduceFrequency(
                            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
                        ),
                    ),
                ),
                HabitOccurrenceSnapshot(targetEvidence, targetPartial),
            ),
        )
        assertTrue(
            targetPrecedenceRequest.recurrenceContext["exceptions"] == null ||
                requireNotNull(targetPrecedenceRequest.recurrenceContext["exceptions"])
                    .jsonArray.isEmpty(),
        )
        assertEquals(
            REDUCED_HABIT_PLANNER_OCCURRENCE_ID,
            requireNotNull(targetPrecedenceRequest.recurrenceContext["partial_progress"])
                .jsonObject.keys.single(),
        )

        listOf(
            "2026-09-01T06:00:00Z",
            "2026-09-01T06:03:00Z",
        ).forEach { movedAt ->
            val collision = capturedRequest(
                occurrences = listOf(
                    HabitOccurrenceSnapshot(
                        sourceEvidence,
                        partial,
                        resolution(HabitMissedResolutionActionSnapshot.Skip),
                    ),
                ),
                recurrenceMoves = mapOf(
                    OCCURRENCE_ID to storedMove(
                        sourceEvidence,
                        startAt = "2026-09-01T07:30:00Z",
                        endAt = "2026-09-01T08:00:00Z",
                        movedAt = movedAt,
                    ),
                ),
            )
            assertEquals(listOf(OCCURRENCE_ID to "skip"), exceptionActions(collision))
            assertNull(collision.recurrenceContext["partial_progress"])
        }

        val targetMove = storedMove(
            targetEvidence,
            startAt = "2026-09-02T07:00:00Z",
            endAt = "2026-09-02T07:30:00Z",
            movedAt = "2026-09-01T06:03:00Z",
        )
        val reductionCollision = capturedRequest(
            occurrences = listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = resolution(
                        HabitMissedResolutionActionSnapshot.ReduceFrequency(
                            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
                        ),
                    ),
                ),
                HabitOccurrenceSnapshot(targetEvidence, outcome = null),
            ),
            recurrenceMoves = mapOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to targetMove),
        )
        assertEquals(
            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to "skip"),
            exceptionActions(reductionCollision),
        )

        val partialTargetKeepsMove = capturedRequest(
            occurrences = listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = resolution(
                        HabitMissedResolutionActionSnapshot.ReduceFrequency(
                            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
                        ),
                    ),
                ),
                HabitOccurrenceSnapshot(targetEvidence, targetPartial),
            ),
            recurrenceMoves = mapOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to targetMove),
        )
        assertEquals(
            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to "move"),
            exceptionActions(partialTargetKeepsMove),
        )
        assertEquals(
            REDUCED_HABIT_PLANNER_OCCURRENCE_ID,
            requireNotNull(partialTargetKeepsMove.recurrenceContext["partial_progress"])
                .jsonObject.keys.single(),
        )

        val targetPause = HabitPauseSnapshot(
            id = HABIT_PAUSE_ID,
            habitId = habit.id,
            revision = 1,
            startedAt = "2026-09-02T04:00:00Z",
            endedAt = "2026-09-02T08:00:00Z",
            preservesStreak = true,
            createdAt = "2026-09-02T04:00:00Z",
            updatedAt = "2026-09-02T08:00:00Z",
        )
        val pausedTargetKeepsPauseAuthority = capturedRequest(
            occurrences = listOf(
                HabitOccurrenceSnapshot(
                    sourceEvidence,
                    outcome = null,
                    missedResolution = resolution(
                        HabitMissedResolutionActionSnapshot.ReduceFrequency(
                            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID),
                        ),
                    ),
                ),
                HabitOccurrenceSnapshot(targetEvidence, outcome = null),
            ),
            recurrenceMoves = mapOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to targetMove),
            pauses = mapOf(HABIT_PAUSE_ID to targetPause),
        )
        assertEquals(
            listOf(REDUCED_HABIT_PLANNER_OCCURRENCE_ID to "move"),
            exceptionActions(pausedTargetKeepsPauseAuthority),
        )
        assertEquals(
            habit.id,
            requireNotNull(pausedTargetKeepsPauseAuthority.recurrenceContext["pauses"])
                .jsonArray.single().jsonObject.getValue("item_id").jsonPrimitive.content,
        )

        val ordinaryOutcomeCollision = capturedRequest(
            occurrences = emptyList(),
            recurrenceMoves = mapOf(
                OCCURRENCE_ID to storedMove(
                    sourceEvidence,
                    startAt = "2026-09-01T07:30:00Z",
                    endAt = "2026-09-01T08:00:00Z",
                    movedAt = "2026-09-01T06:59:00Z",
                ),
            ),
            recurrenceOutcomes = mapOf(
                OCCURRENCE_ID to RecurrenceOutcomeSnapshot(
                    itemId = habit.id,
                    status = ItemStatus.SKIPPED,
                    resolvedAt = clock.toString(),
                ),
            ),
        )
        assertEquals(listOf(OCCURRENCE_ID to "skip"), exceptionActions(ordinaryOutcomeCollision))
    }

    @Test
    fun localHabitCompositionRequiresACompleteDurableHabitCheckpoint() = runBlocking {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val habit = localCanonicalItem().copy(kind = "habit")
        val evidence = HabitOccurrenceEvidenceSnapshot(
            id = HABIT_LEDGER_OCCURRENCE_ID,
            habitId = habit.id,
            plannerOccurrenceId = OCCURRENCE_ID,
            sourceScheduleRevisionId = HABIT_SOURCE_SCHEDULE_ID,
            sourceItemRevision = habit.revision,
            policyFingerprint = "sha256:${"a".repeat(64)}",
            identity = dailyOccurrenceIdentity(),
            nominalStart = "2026-09-01T07:00:00Z",
            nominalEnd = "2026-09-01T07:30:00Z",
            windowStart = "2026-09-01T06:00:00Z",
            windowEnd = "2026-09-01T10:00:00Z",
            localDate = "2026-09-01",
            timezoneName = "Europe/Madrid",
            expectedDurationSeconds = 1_800,
            expectedQuantity = null,
            expectedUnit = null,
        )
        val occurrence = HabitOccurrenceSnapshot(evidence, outcome = null)
        val pendingCommand = HabitOutcomeCommandSnapshot(
            operationId = HABIT_OPERATION_ID,
            expectedRevision = 0,
            outcome = HabitOutcomeInputSnapshot(
                status = HabitOutcomeStatusSnapshot.PARTIAL,
                progressBasisPoints = 5_000,
                quantity = null,
                unit = null,
                actualSeconds = 900,
                note = null,
                occurredAt = "2026-09-01T07:20:00Z",
            ),
        )
        val pending = PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.OUTCOME,
            habitId = habit.id,
            targetId = evidence.id,
            expectedRevision = 0,
            idempotencyKey = HABIT_OPERATION_ID,
            requestJson = pendingCommand.encoded(),
            createdAt = "2026-09-01T07:20:00Z",
            syncOrigin = origin,
            configurationId = configurationId,
        )
        val incomplete = localCompositionReadyState().copy(
            canonicalItems = listOf(habit),
            habitLedger = HabitLedgerSnapshot(
                syncOrigin = origin,
                configurationId = configurationId,
                deltaCursor = null,
                occurrences = mapOf(evidence.id to occurrence),
            ),
        )
        var composeCalls = 0
        suspend fun compose(state: DayWeaveUiState): CanonicalRefreshOutcome =
            manager(
                PlannerStore(state),
                FakeCanonicalTransport(),
                localScheduleComposer = LocalScheduleComposer { _, request ->
                    composeCalls += 1
                    emptyLocalComposition(request).copy(
                        sourceItemCount = 1,
                        sourceItemRevisions = mapOf(habit.id to habit.revision),
                        rejectedItems = listOf(
                            RemoteRejectedScheduleItem(
                                itemId = habit.id,
                                isSensitive = habit.isSensitive,
                                title = habit.title,
                                reason = "Synthetic unsupported habit",
                            ),
                        ),
                    )
                },
            ).composeLocally()

        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, compose(incomplete))
        assertEquals(0, composeCalls)

        val intermediate = incomplete.copy(
            habitLedger = incomplete.habitLedger.copy(deltaCursor = "habit-cursor"),
        )
        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, compose(intermediate))
        assertEquals(0, composeCalls)

        val complete = intermediate.copy(
            habitLedger = intermediate.habitLedger.copy(deltaCaughtUp = true),
        )
        val pendingState = complete.copy(
            habitLedger = complete.habitLedger.copy(pendingMutations = listOf(pending))
                .also(HabitLedgerSnapshot::requireValid),
        )
        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, compose(pendingState))
        assertEquals(0, composeCalls)

        val discardedConflictStore = PlannerStore(complete)
        assertNotNull(discardedConflictStore.stageHabitMutation(pending))
        assertNotNull(
            discardedConflictStore.markHabitMutationForReview(
                HABIT_OPERATION_ID,
                PendingHabitMutationDisposition.CONFLICT,
            ),
        )
        assertNotNull(discardedConflictStore.discardReviewedHabitMutation(HABIT_OPERATION_ID))
        assertFalse(discardedConflictStore.state.value.habitLedger.deltaCaughtUp)
        assertEquals(
            CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            compose(discardedConflictStore.state.value),
        )
        assertEquals(0, composeCalls)

        discardedConflictStore.applyHabitDeltaPage(
            origin,
            configurationId,
            occurrences = emptyList(),
            pauses = emptyList(),
            nextCursor = "habit-cursor",
            hasMore = false,
        )
        assertTrue(discardedConflictStore.state.value.habitLedger.deltaCaughtUp)

        val completeStore = PlannerStore(complete)
        val completeOutcome = manager(
            completeStore,
            FakeCanonicalTransport(),
            localScheduleComposer = LocalScheduleComposer { _, request ->
                composeCalls += 1
                emptyLocalComposition(request).copy(
                    sourceItemCount = 1,
                    sourceItemRevisions = mapOf(habit.id to habit.revision),
                    rejectedItems = listOf(
                        RemoteRejectedScheduleItem(
                            itemId = habit.id,
                            isSensitive = habit.isSensitive,
                            title = habit.title,
                            reason = "Synthetic unsupported habit",
                        ),
                    ),
                )
            },
        ).composeLocally()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, completeOutcome)
        assertEquals(1, composeCalls)
        assertNotNull(completeStore.state.value.localScheduleCompositionProvenance)

        completeStore.applyHabitDeltaPage(
            origin,
            configurationId,
            occurrences = emptyList(),
            pauses = emptyList(),
            nextCursor = "habit-cursor-next",
            hasMore = true,
        )
        assertFalse(completeStore.state.value.habitLedger.deltaCaughtUp)
        assertNull(completeStore.state.value.localScheduleCompositionProvenance)
    }

    @Test
    fun lifecycleInvalidationImmediatelyBeforeNativeCallNeverInvokesComposer() = runBlocking {
        val plannerStore = PlannerStore(localCompositionReadyState())
        val fence = SequencedLocalCompositionFence(listOf(true, false))
        var composeCalls = 0

        val outcome = manager(
            plannerStore,
            FakeCanonicalTransport(),
            localScheduleComposer = LocalScheduleComposer { _, request ->
                composeCalls += 1
                emptyLocalComposition(request)
            },
            localCompositionLifecycleFence = fence,
        ).composeLocally(fence.captureGeneration())

        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, outcome)
        assertEquals(0, composeCalls)
        assertEquals(null, plannerStore.state.value.localScheduleCompositionProvenance)
    }

    @Test
    fun finalPreinstallFenceDiscardsBackgroundedResultAfterMapping() = runBlocking {
        val initial = localCompositionReadyState()
        val plannerStore = PlannerStore(initial)
        // Initial admission, immediately before JNI, and post-JNI pass. The exact final check
        // immediately before the encrypted install observes the privacy boundary.
        val fence = SequencedLocalCompositionFence(listOf(true, true, true, false))

        val outcome = manager(
            plannerStore,
            FakeCanonicalTransport(),
            localScheduleComposer = LocalScheduleComposer { _, request ->
                emptyLocalComposition(request)
            },
            localCompositionLifecycleFence = fence,
        ).composeLocally(fence.captureGeneration())

        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, outcome)
        assertEquals(initial, plannerStore.state.value)
        assertEquals(initial, plannerStore.durableState.value)
    }

    @Test
    fun localRequestUsesCapturedSnapshotAcrossProfileABA() = runBlocking {
        val initial = localCompositionReadyState().copy(
            scheduleMessage = "Scheduling profile changed · recompose to refresh the firm horizon",
        )
        val originalProfile = initial.scheduleCompositionProfile
        val plannerStore = PlannerStore(initial)
        var zoneCalls = 0
        var requestedAvailabilityStart: String? = null
        val outcome = manager(
            plannerStore,
            FakeCanonicalTransport(),
            zoneProvider = {
                zoneCalls += 1
                if (zoneCalls == 1) {
                    assertTrue(
                        plannerStore.updateScheduleCompositionProfile(
                            originalProfile.copy(dayStartMinute = 8 * 60),
                        ),
                    )
                }
                ZoneId.of("Europe/Madrid")
            },
            localScheduleComposer = LocalScheduleComposer { _, request ->
                requestedAvailabilityStart = request.availability.first().start
                assertTrue(plannerStore.updateScheduleCompositionProfile(originalProfile))
                emptyLocalComposition(request)
            },
        ).composeLocally()

        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
        assertEquals("2026-09-01T05:00:00Z", requestedAvailabilityStart)
        assertEquals(originalProfile, plannerStore.state.value.scheduleCompositionProfile)
        assertTrue(
            requireNotNull(plannerStore.state.value.localScheduleCompositionProvenance)
                .matchesState(plannerStore.state.value),
        )
    }

    @Test
    fun zoneChangeAndMidnightCrossingDiscardNonPreemptibleNativeResults() = runBlocking {
        suspend fun assertDiscarded(
            mutateDuringNative: () -> Unit,
            nowProvider: () -> Instant,
            zoneProvider: () -> ZoneId,
        ) {
            val initial = localCompositionReadyState()
            val plannerStore = PlannerStore(initial)
            val outcome = manager(
                plannerStore,
                FakeCanonicalTransport(),
                nowProvider = nowProvider,
                zoneProvider = zoneProvider,
                localScheduleComposer = LocalScheduleComposer { _, request ->
                    mutateDuringNative()
                    emptyLocalComposition(request)
                },
            ).composeLocally()
            assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, outcome)
            assertEquals(initial, plannerStore.state.value)
            assertEquals(initial, plannerStore.durableState.value)
        }

        var zone = ZoneId.of("Europe/Madrid")
        assertDiscarded(
            mutateDuringNative = { zone = ZoneId.of("UTC") },
            nowProvider = { clock },
            zoneProvider = { zone },
        )
        var instant = Instant.parse("2026-09-01T21:59:59Z")
        assertDiscarded(
            mutateDuringNative = { instant = Instant.parse("2026-09-01T22:00:00Z") },
            nowProvider = { instant },
            zoneProvider = { ZoneId.of("Europe/Madrid") },
        )
    }

    @Test
    fun credentialReplacementAndPlannerMutationDuringNativeDiscardWithoutInstall() = runBlocking {
        val credential = MutableCanonicalCredentialStore()
        val credentialInitial = localCompositionReadyState()
        val credentialStore = PlannerStore(credentialInitial)
        val credentialOutcome = manager(
            credentialStore,
            FakeCanonicalTransport(),
            credentialStore = credential,
            localScheduleComposer = LocalScheduleComposer { _, request ->
                credential.configurationId = "replacement-binding"
                emptyLocalComposition(request)
            },
        ).composeLocally()
        assertEquals(CanonicalRefreshOutcome.CONFIGURATION_ERROR, credentialOutcome)
        assertEquals(credentialInitial, credentialStore.state.value)
        assertEquals(null, credentialStore.state.value.localScheduleCompositionProvenance)

        val mutationInitial = localCompositionReadyState()
        val mutationStore = PlannerStore(mutationInitial)
        val mutationOutcome = manager(
            mutationStore,
            FakeCanonicalTransport(),
            localScheduleComposer = LocalScheduleComposer { _, request ->
                mutationStore.toggleCompleted()
                emptyLocalComposition(request)
            },
        ).composeLocally()
        assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, mutationOutcome)
        assertFalse(mutationStore.state.value.showCompleted)
        assertEquals(mutationInitial.canonicalDeltaCursor, mutationStore.state.value.canonicalDeltaCursor)
        assertEquals(null, mutationStore.state.value.localScheduleCompositionProvenance)
        assertEquals(null, mutationStore.state.value.publishedScheduleProof)
    }

    @Test
    fun liveDurableMismatchPreventsNativeComposition() = runBlocking {
        val initial = localCompositionReadyState()
        val saveGate = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveGate.await()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val plannerStore = PlannerStore(initial, repository, scope)
            withTimeout(3_000) {
                plannerStore.loadState.first { it == PlannerLoadState.READY }
            }
            assertTrue(
                plannerStore.updateScheduleCompositionProfile(
                    initial.scheduleCompositionProfile.copy(dayStartMinute = 8 * 60),
                ),
            )
            var composeCalls = 0

            val outcome = manager(
                plannerStore,
                FakeCanonicalTransport(),
                localScheduleComposer = LocalScheduleComposer { _, request ->
                    composeCalls += 1
                    emptyLocalComposition(request)
                },
            ).composeLocally()

            assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, outcome)
            assertEquals(0, composeCalls)
            assertEquals(initial, plannerStore.durableState.value)
        } finally {
            saveGate.complete(Unit)
            scope.cancel()
        }
    }

    @Test
    fun stableExecutionVerificationRefreshKeepsLocalPlanButSemanticInputsInvalidateIt() =
        runBlocking {
            val plannerStore = PlannerStore(localCompositionReadyState())
            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(
                    plannerStore,
                    FakeCanonicalTransport(),
                    localScheduleComposer = LocalScheduleComposer { _, request ->
                        emptyLocalComposition(request)
                    },
                ).composeLocally(),
            )
            requireNotNull(
                plannerStore.markCanonicalExecutionHistoryUnverified(
                    "https://api.example.test/",
                    "connection-1",
                ),
            ).awaitDurable()
            assertNotNull(plannerStore.state.value.localScheduleCompositionProvenance)
            requireNotNull(
                plannerStore.recordCanonicalExecutionHistoryWindow(
                    syncOrigin = "https://api.example.test/",
                    configurationId = "connection-1",
                    revision = 0,
                    history = emptyList(),
                    continuityVerified = true,
                    message = "Execution history verified",
                ),
            ).awaitDurable()
            val installed = plannerStore.state.value
            val provenance = requireNotNull(installed.localScheduleCompositionProvenance)
            assertTrue(provenance.matchesState(installed))

            assertFalse(provenance.matchesState(installed.copy(canonicalExecutionRevision = 1)))
            assertFalse(
                provenance.matchesState(
                    installed.copy(
                        scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(
                            dayStartMinute = 8 * 60,
                        ),
                    ),
                ),
            )
            assertFalse(
                provenance.matchesState(
                    installed.copy(
                        recurrenceOutcomes = mapOf(
                            "33333333-3333-5333-8333-333333333333" to
                                RecurrenceOutcomeSnapshot(
                                    itemId = TASK_ID,
                                    status = ItemStatus.SKIPPED,
                                    resolvedAt = clock.toString(),
                                ),
                        ),
                    ),
                ),
            )
            assertFalse(
                provenance.matchesState(
                    installed.copy(
                        recurrenceMoves = mapOf(
                            "33333333-3333-5333-8333-333333333333" to
                                RecurrenceMoveSnapshot(
                                    itemId = TASK_ID,
                                    startAt = "2026-09-01T10:00:00Z",
                                    endAt = "2026-09-01T10:30:00Z",
                                    movedAt = clock.toString(),
                                ),
                        ),
                    ),
                ),
            )
            assertFalse(
                provenance.matchesState(
                    installed.copy(
                        schedule = listOf(
                            ScheduleItem(
                                id = "external-fixed",
                                title = "Calendar",
                                kind = ItemKind.EVENT,
                                startMinute = 600,
                                durationMinutes = 30,
                                status = ItemStatus.SCHEDULED,
                                isFlexible = false,
                                isHardConstraint = true,
                            ),
                        ),
                    ),
                ),
            )
        }

    @Test
    fun localProvenanceMemoSurvivesUnrelatedUiCopiesAndRecomputesForExecutionChange() =
        runBlocking {
            val plannerStore = PlannerStore(localCompositionReadyState())
            assertEquals(
                CanonicalRefreshOutcome.SUCCESS,
                manager(
                    plannerStore,
                    FakeCanonicalTransport(),
                    localScheduleComposer = LocalScheduleComposer { _, request ->
                        emptyLocalComposition(request)
                    },
                ).composeLocally(),
            )
            val before = localScheduleCompositionFingerprintComputationCount()
            repeat(3) {
                assertTrue(
                    plannerStore.state.value.isScheduleDisplayCurrent(
                        clock,
                        ZoneId.of("Europe/Madrid"),
                    ),
                )
            }
            plannerStore.navigate(com.greengolddog.dayweave.model.AppDestination.ASSISTANT)
            plannerStore.toggleCompleted()
            assertTrue(plannerStore.sendAssistantMessage("Do not invalidate the local plan"))
            assertNotNull(plannerStore.state.value.localScheduleCompositionProvenance)
            assertEquals(before, localScheduleCompositionFingerprintComputationCount())

            requireNotNull(
                plannerStore.reconcileCanonicalExecution(
                    syncOrigin = "https://api.example.test/",
                    configurationId = "connection-1",
                    revision = 1,
                    activeSession = null,
                    message = "Execution revision changed",
                ),
            ).awaitDurable()
            assertEquals(null, plannerStore.state.value.localScheduleCompositionProvenance)
            assertEquals(before + 1, localScheduleCompositionFingerprintComputationCount())
        }

    private fun localCompositionReadyState() = DayWeaveUiState(
        canonicalSyncOrigin = "https://api.example.test/",
        canonicalConfigurationId = "connection-1",
        canonicalDeltaCursor = "cursor-1",
        canonicalExecutionSyncOrigin = "https://api.example.test/",
        canonicalExecutionConfigurationId = "connection-1",
        canonicalExecutionRevision = 0,
        canonicalExecutionHistoryWindow = emptyList(),
        canonicalExecutionHistoryWindowRevision = 0,
        canonicalExecutionHistoryContinuityEstablished = true,
        canonicalExecutionHistoryVerified = true,
    )

    private fun localCanonicalItem() = com.greengolddog.dayweave.model.CanonicalItemSnapshot(
        id = TASK_ID,
        kind = "task",
        status = "planned",
        title = "Local deterministic task",
        timezoneName = "Europe/Madrid",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = clock.toString(),
        updatedAt = clock.toString(),
    )

    private fun emptyLocalComposition(request: SchedulePreviewRequest) =
        LocalScheduleComposition(
            localInputFingerprint = "local-sha256:${"a".repeat(64)}",
            scheduleRequestFingerprint = "sha256:${"b".repeat(64)}",
            sourceItemCount = 0,
            sourceItemRevisions = emptyMap(),
            acceptedItemCount = 0,
            rejectedItems = emptyList(),
            ignoredPreviousAssignments = emptyList(),
            plan = RemoteSchedulePlan(
                asOf = request.asOf,
                horizonStart = request.horizonStart,
                horizonEnd = request.horizonEnd,
                blocks = emptyList(),
                unscheduled = emptyList(),
                decisions = emptyList(),
                violations = emptyList(),
                score = RemotePlanScore(0, 0, 0uL, 0),
                occurrences = emptyList(),
            ),
        )

    private fun manager(
        plannerStore: PlannerStore,
        transport: FakeCanonicalTransport,
        credentialStore: ApiCredentialStore = CanonicalCredentialStore(),
        currentInstant: Instant = clock,
        nowProvider: () -> Instant = { currentInstant },
        zoneProvider: () -> ZoneId = { ZoneId.of("Europe/Madrid") },
        localScheduleComposer: LocalScheduleComposer? = null,
        localCompositionLifecycleFence: LocalCompositionLifecycleFence =
            UnfencedLocalCompositionLifecycle,
        cancelTimedBreakNotification: suspend () -> Boolean = { true },
        reconcileTimedBreakNotification: suspend () -> Unit = {},
    ) = CanonicalSyncManager(
        plannerStore = plannerStore,
        credentialStore = credentialStore,
        transport = transport,
        now = nowProvider,
        zoneId = zoneProvider,
        localScheduleComposer = localScheduleComposer,
        localCompositionLifecycleFence = localCompositionLifecycleFence,
        cancelTimedBreakNotification = cancelTimedBreakNotification,
        reconcileTimedBreakNotification = reconcileTimedBreakNotification,
    )

    private suspend fun assertTerminalExecutionProjects(
        wireStatus: String,
        displayStatus: ItemStatus,
        actualSeconds: Long = 120,
        expectedActualMinutes: Int = 2,
    ) {
        val plannerStore = PlannerStore(DayWeaveUiState())
        val transport = FakeCanonicalTransport().apply {
            pages[null] = RemoteItemDeltaPage(
                listOf(RemoteItemDeltaChange(type = "upsert", item = remoteItem(split = false))),
                "cursor-1",
                false,
            )
            previewResult = preview()
        }
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        recordTerminalExecution(plannerStore, wireStatus, actualSeconds)
        val applied = remoteItem(split = false).copy(
            status = wireStatus,
            revision = 8,
            updatedAt = "2026-09-01T07:02:00Z",
            completedAt = if (wireStatus == "completed") "2026-09-01T07:02:00Z" else null,
        )
        transport.replacementResult = applied
        transport.pages["cursor-1"] = RemoteItemDeltaPage(
            listOf(RemoteItemDeltaChange(type = "upsert", item = applied)),
            "cursor-2",
            false,
        )
        transport.previewResult = terminalPreview(8)

        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())

        assertEquals(wireStatus, transport.replacementRequest?.item?.status)
        assertEquals(7L, transport.replacementRequest?.expectedRevision)
        assertEquals(1, transport.replacementIdempotencyKeys.size)
        val state = plannerStore.state.value
        assertEquals(null, state.pendingCanonicalMutation)
        assertEquals(wireStatus, state.canonicalItems.single().status)
        assertEquals(displayStatus, state.schedule.single().status)
        assertEquals(expectedActualMinutes, state.schedule.single().actualMinutes)
        assertEquals(
            8L,
            state.terminalExecutionOutcomes.getValue(EXECUTION_ID).canonicalProjectionRevision,
        )
    }

    private fun recordTerminalExecution(
        plannerStore: PlannerStore,
        status: String,
        actualSeconds: Long = 120,
    ) {
        val running = CanonicalExecutionSessionSnapshot(
            id = EXECUTION_ID,
            itemId = TASK_ID,
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = BLOCK_ID,
            sourceDeviceId = DEVICE_ID,
            status = "active",
            revision = 1,
            accumulatedSeconds = 0,
            startedAt = "2026-09-01T07:00:00Z",
            runningSince = "2026-09-01T07:00:00Z",
            createdAt = "2026-09-01T07:00:00Z",
            updatedAt = "2026-09-01T07:00:00Z",
            canonicalProjectionEligibleAtLeaseStart = true,
        )
        requireNotNull(
            plannerStore.reconcileCanonicalExecution(
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = 1,
                activeSession = running,
                message = "Running",
            ),
        )
        requireNotNull(
            plannerStore.reconcileCanonicalExecution(
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = running.copy(
                    status = status,
                    revision = 2,
                    accumulatedSeconds = 120,
                    actualSeconds = actualSeconds,
                    runningSince = null,
                    endedAt = "2026-09-01T07:02:00Z",
                    updatedAt = "2026-09-01T07:02:00Z",
                ),
                message = "Ended",
            ),
        )
        assertTrue(
            plannerStore.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .requiresCanonicalItemProjection,
        )
    }

    private fun remoteItem(
        split: Boolean = true,
        isSensitive: Boolean = false,
    ) = RemoteCanonicalItem(
        id = TASK_ID,
        isSensitive = isSensitive,
        kind = "task",
        status = "planned",
        title = "Compose Android timeline",
        notes = "Server-owned canonical task",
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        deadlineAt = "2026-09-01T12:00:00Z",
        earliestStartAt = null,
        recurrence = null,
        flexibleConstraints = buildJsonObject {
            put("energy", "deep")
            put(
                "required_contexts",
                kotlinx.serialization.json.buildJsonArray {
                    add(buildJsonObject { put("value", "computer") })
                },
            )
        },
        splitPolicy = if (split) {
            buildJsonObject {
                put("type", "splittable")
                put("minimum_chunk_seconds", 1_200)
                put("maximum_chunk_seconds", 3_600)
            }
        } else {
            buildJsonObject { put("type", "indivisible") }
        },
        importance = 80,
        urgency = 60,
        parentId = null,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = "2026-08-29T09:00:00Z",
        updatedAt = "2026-08-29T10:00:00Z",
    )

    private fun authoredDraft(
        title: String = "Compose Android timeline",
    ) = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        title = title,
        notes = null,
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        deadlineAt = "2026-09-01T12:00:00Z",
        importance = 80,
        urgency = 60,
    )

    private fun authoredRemote(
        id: String,
        revision: Long,
        title: String = "Compose Android timeline",
        parentId: String? = null,
        isExecutable: Boolean = true,
    ) = RemoteCanonicalItem(
        id = id,
        isSensitive = false,
        kind = "task",
        status = "planned",
        title = title,
        notes = null,
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        deadlineAt = "2026-09-01T12:00:00Z",
        flexibleConstraints = buildJsonObject { },
        splitPolicy = buildJsonObject { put("type", "indivisible") },
        importance = 80,
        urgency = 60,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = isExecutable,
        revision = revision,
        createdAt = "2026-09-01T07:00:00Z",
        updatedAt = "2026-09-01T07:00:00Z",
    )

    private fun itemsPreview(items: List<RemoteCanonicalItem>): RemoteSchedulePreview =
        emptyPreview().copy(
            sourceItemCount = items.size,
            sourceItemRevisions = items.associate { it.id to it.revision },
            acceptedItemCount = items.size,
        )

    private fun RemoteSchedulePreview.withWindow(
        asOf: Instant,
        horizonStart: String,
        horizonEnd: String,
    ): RemoteSchedulePreview = copy(
        plan = plan.copy(
            asOf = asOf.toString(),
            horizonStart = horizonStart,
            horizonEnd = horizonEnd,
        ),
    )

    private fun preview(isSensitive: Boolean = false) = RemoteSchedulePreview(
        inputDigest = "sha256:${"a".repeat(64)}",
        sourceItemCount = 1,
        sourceItemRevisions = mapOf(TASK_ID to 7),
        acceptedItemCount = 1,
        rejectedItems = emptyList(),
        ignoredPreviousAssignments = emptyList(),
        plan = RemoteSchedulePlan(
            asOf = clock.toString(),
            horizonStart = "2026-08-31T22:00:00Z",
            horizonEnd = "2026-09-07T22:00:00Z",
            blocks = listOf(
                RemoteScheduleBlock(
                    id = BLOCK_ID,
                    isSensitive = isSensitive,
                    itemId = TASK_ID,
                    title = "Compose Android timeline",
                    start = "2026-09-01T09:00:00+02:00",
                    end = "2026-09-01T10:00:00+02:00",
                    sessionIndex = 0,
                    kind = "planned",
                    explanations = emptyList(),
                ),
            ),
            unscheduled = emptyList(),
            decisions = emptyList(),
            violations = emptyList(),
            score = RemotePlanScore(
                scheduledMinutes = 60,
                unscheduledMinutes = 0,
                softPenalty = 0uL,
                movedMinutes = 0,
            ),
            occurrences = emptyList(),
        ),
    )

    private fun currentSchedule(schedule: RemoteSchedulePreview) = RemoteCurrentPublishedSchedule(
        revision = RemotePublishedScheduleRevision(
            id = "77777777-7777-4777-8777-777777777777",
            revision = "1:77777777-7777-4777-8777-777777777777",
            revisionNumber = 1uL,
            inputDigest = schedule.inputDigest,
            horizonStart = schedule.plan.horizonStart,
            horizonEnd = schedule.plan.horizonEnd,
            timezoneName = "Europe/Madrid",
            publishedAt = clock.toString(),
        ),
        schedule = schedule,
    )

    private fun terminalPreview(revision: Long) = preview().copy(
        sourceItemRevisions = mapOf(TASK_ID to revision),
        plan = preview().plan.copy(
            blocks = emptyList(),
            score = RemotePlanScore(
                scheduledMinutes = 0,
                unscheduledMinutes = 0,
                softPenalty = 0uL,
                movedMinutes = 0,
            ),
        ),
    )

    private fun scheduledPreview(item: RemoteCanonicalItem) = preview().copy(
        sourceItemRevisions = mapOf(item.id to item.revision),
        plan = preview().plan.copy(
            blocks = listOf(
                preview().plan.blocks.single().copy(title = item.title),
            ),
        ),
    )

    private fun emptyPreview() = preview().copy(
        sourceItemCount = 0,
        sourceItemRevisions = emptyMap(),
        acceptedItemCount = 0,
        plan = preview().plan.copy(
            blocks = emptyList(),
            score = RemotePlanScore(
                scheduledMinutes = 0,
                unscheduledMinutes = 0,
                softPenalty = 0uL,
                movedMinutes = 0,
            ),
        ),
    )

    private fun publicationResponse(
        request: SchedulePublishHttpRequest,
        replayed: Boolean,
        publishedAt: String? = null,
    ): RemoteSchedulePublishResponse {
        val decoded = Json.decodeFromString<SchedulePublishRequest>(request.bodyJson)
        val revisionId = "77777777-7777-4777-8777-777777777777"
        return RemoteSchedulePublishResponse(
            revision = RemotePublishedScheduleRevision(
                id = revisionId,
                revision = "1:$revisionId",
                revisionNumber = 1uL,
                inputDigest = decoded.expectedInputDigest,
                horizonStart = decoded.schedule.horizonStart,
                horizonEnd = decoded.schedule.horizonEnd,
                timezoneName = decoded.schedule.timezoneName,
                publishedAt = publishedAt ?: decoded.schedule.asOf,
            ),
            replayed = replayed,
        )
    }

    private fun dailyOccurrenceIdentity() = buildJsonObject {
        put("type", "calendar_day")
        put("date", "2026-09-01")
        put("bucket_ordinal", 0)
    }

    private fun dailyRecurrence() = buildJsonObject {
        put("type", "daily")
        put("times_per_day", 1)
    }

    private companion object {
        const val TASK_ID = "11111111-1111-4111-8111-111111111111"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_BLOCK_ID = "33333333-3333-4333-8333-333333333333"
        const val THIRD_BLOCK_ID = "99999999-9999-4999-8999-999999999999"
        const val OCCURRENCE_ID = "44444444-4444-5444-8444-444444444444"
        const val HABIT_LEDGER_OCCURRENCE_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val REDUCED_HABIT_LEDGER_OCCURRENCE_ID =
            "abababab-abab-4bab-8bab-abababababab"
        const val REDUCED_HABIT_PLANNER_OCCURRENCE_ID =
            "fafafafa-fafa-5afa-8afa-fafafafafafa"
        const val HABIT_SOURCE_SCHEDULE_ID = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        const val HABIT_PAUSE_ID = "12121212-1212-4212-8212-121212121212"
        const val HABIT_OPERATION_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        const val EXECUTION_ID = "55555555-5555-4555-8555-555555555555"
        const val DEVICE_ID = "66666666-6666-4666-8666-666666666666"
        const val CALENDAR_ITEM_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val CALENDAR_BLOCK_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    }
}

private class SequencedLocalCompositionFence(results: List<Boolean>) :
    LocalCompositionLifecycleFence {
    private val remaining = ArrayDeque(results)

    override fun captureGeneration(): Long = 17L

    override fun isCurrent(generation: Long): Boolean {
        check(generation == 17L)
        return if (remaining.size > 1) remaining.removeFirst() else remaining.first()
    }
}

private class CanonicalCredentialStore : ApiCredentialStore {
    private var lastSync: Long? = null

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = lastSync,
        configurationId = "connection-1",
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "test-secret",
            "connection-1",
        )

    override fun update(baseUrl: String, bearerToken: String?) = Unit

    override fun clear() = Unit

    override fun recordSuccessfulSync(epochMillis: Long) {
        lastSync = epochMillis
    }
}

private class MutableCanonicalCredentialStore : ApiCredentialStore {
    var configurationId: String = "connection-1"

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "test-secret",
            configurationId,
        )

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeCanonicalTransport : CanonicalPlannerTransport {
    val pages = mutableMapOf<String?, RemoteItemDeltaPage>()
    val queuedPages = mutableMapOf<String?, ArrayDeque<RemoteItemDeltaPage>>()
    val deltaCursors = mutableListOf<String?>()
    var previewResult: RemoteSchedulePreview? = null
    var previewRequest: SchedulePreviewRequest? = null
    val queuedPreviews = ArrayDeque<RemoteSchedulePreview>()
    val previewRequests = mutableListOf<SchedulePreviewRequest>()
    val publicationRequests = mutableListOf<SchedulePublishHttpRequest>()
    private val publicationAttemptsByKey = mutableMapOf<String, Int>()
    var publicationResult: RemoteSchedulePublishResponse? = null
    var publicationError: Throwable? = null
    var publicationStarted: CompletableDeferred<Unit>? = null
    var publicationGate: CompletableDeferred<Unit>? = null
    var publicationHandler: (suspend (
        request: SchedulePublishHttpRequest,
    ) -> RemoteSchedulePublishResponse)? = null
    var deltaStarted: CompletableDeferred<Unit>? = null
    var deltaGate: CompletableDeferred<Unit>? = null
    var deltaError: Throwable? = null
    var currentScheduleResult: RemoteCurrentPublishedSchedule? = null
    var currentScheduleError: Throwable? = null
    var currentScheduleHandler: (suspend () -> RemoteCurrentPublishedSchedule?)? = null
    val currentScheduleConfigurations = mutableListOf<String?>()
    var replacementResult: RemoteCanonicalItem? = null
    var replacementRequest: ReplaceCanonicalItemRequest? = null
    val replacementRequests = mutableListOf<ReplaceCanonicalItemRequest>()
    var replacementId: String? = null
    var replacementIdempotencyKey: String? = null
    val replacementIdempotencyKeys = mutableListOf<String>()
    var replacementError: Throwable? = null
    var replacementHandler: (suspend (
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ) -> RemoteCanonicalItem)? = null
    val authoringOperationIds = mutableListOf<String>()
    val createRequests = mutableListOf<Pair<String, CreateCanonicalItemRequest>>()
    var createResult: RemoteCanonicalItem? = null
    var createError: Throwable? = null
    var createHandler: (suspend (
        idempotencyKey: String,
        request: CreateCanonicalItemRequest,
    ) -> RemoteCanonicalItem)? = null
    data class TrashRequest(
        val id: String,
        val idempotencyKey: String,
        val expectedRevision: Long,
    )
    val trashRequests = mutableListOf<TrashRequest>()
    var trashResult: RemoteCanonicalItem? = null
    var trashError: Throwable? = null
    var trashHandler: (suspend (TrashRequest) -> RemoteCanonicalItem)? = null
    data class RestoreRequest(
        val id: String,
        val idempotencyKey: String,
        val request: CanonicalItemRevisionRequest,
    )
    val restoreRequests = mutableListOf<RestoreRequest>()
    var restoreResult: RemoteCanonicalItem? = null
    var restoreError: Throwable? = null
    var restoreHandler: (suspend (RestoreRequest) -> RemoteCanonicalItem)? = null

    override suspend fun itemDelta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
    ): RemoteItemDeltaPage {
        deltaCursors.add(cursor)
        deltaStarted?.complete(Unit)
        deltaGate?.await()
        deltaError?.let { throw it }
        return queuedPages[cursor]?.removeFirstOrNull() ?: requireNotNull(pages[cursor])
    }

    override suspend fun currentSchedule(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteCurrentPublishedSchedule? {
        currentScheduleConfigurations += configuration.configurationId
        currentScheduleError?.let { throw it }
        return currentScheduleHandler?.invoke() ?: currentScheduleResult
    }

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePreviewRequest,
    ): RemoteSchedulePreview {
        previewRequest = request
        previewRequests.add(request)
        return queuedPreviews.removeFirstOrNull() ?: requireNotNull(previewResult)
    }

    override suspend fun publish(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePublishHttpRequest,
    ): RemoteSchedulePublishResponse {
        publicationRequests += request
        val decoded = Json.decodeFromString<SchedulePublishRequest>(request.bodyJson)
        val keyAttempt = (publicationAttemptsByKey[decoded.idempotencyKey] ?: 0) + 1
        publicationAttemptsByKey[decoded.idempotencyKey] = keyAttempt
        publicationStarted?.complete(Unit)
        publicationGate?.await()
        publicationHandler?.let { return it(request) }
        publicationError?.let { throw it }
        publicationResult?.let { return it }
        val revisionNumber = publicationRequests.size.toULong()
        return RemoteSchedulePublishResponse(
            revision = RemotePublishedScheduleRevision(
                id = PUBLISHED_REVISION_ID,
                revision = "$revisionNumber:$PUBLISHED_REVISION_ID",
                revisionNumber = revisionNumber,
                inputDigest = decoded.expectedInputDigest,
                horizonStart = decoded.schedule.horizonStart,
                horizonEnd = decoded.schedule.horizonEnd,
                timezoneName = decoded.schedule.timezoneName,
                publishedAt = decoded.schedule.asOf,
            ),
            replayed = keyAttempt > 1,
        )
    }

    override suspend fun createItem(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: CreateCanonicalItemRequest,
    ): RemoteCanonicalItem {
        authoringOperationIds += request.id
        createRequests += idempotencyKey to request
        createHandler?.let { return it(idempotencyKey, request) }
        createError?.let { throw it }
        return requireNotNull(createResult)
    }

    override suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ): RemoteCanonicalItem {
        authoringOperationIds += id
        replacementId = id
        replacementIdempotencyKey = idempotencyKey
        replacementIdempotencyKeys += idempotencyKey
        replacementRequest = request
        replacementRequests += request
        replacementHandler?.let { return it(id, idempotencyKey, request) }
        replacementError?.let { throw it }
        return requireNotNull(replacementResult)
    }

    override suspend fun trashItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        expectedRevision: Long,
    ): RemoteCanonicalItem {
        authoringOperationIds += id
        val request = TrashRequest(id, idempotencyKey, expectedRevision)
        trashRequests += request
        trashHandler?.let { return it(request) }
        trashError?.let { throw it }
        return requireNotNull(trashResult)
    }

    override suspend fun restoreItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: CanonicalItemRevisionRequest,
    ): RemoteCanonicalItem {
        authoringOperationIds += id
        val recorded = RestoreRequest(id, idempotencyKey, request)
        restoreRequests += recorded
        restoreHandler?.let { return it(recorded) }
        restoreError?.let { throw it }
        return requireNotNull(restoreResult)
    }

    private companion object {
        const val PUBLISHED_REVISION_ID = "77777777-7777-4777-8777-777777777777"
    }
}
