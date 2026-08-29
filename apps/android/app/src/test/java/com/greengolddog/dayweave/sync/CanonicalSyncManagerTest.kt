package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.PlannerApiException
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.network.RemoteItemDeltaChange
import com.greengolddog.dayweave.network.RemoteItemDeltaPage
import com.greengolddog.dayweave.network.RemoteItemTombstone
import com.greengolddog.dayweave.network.RemotePlanScore
import com.greengolddog.dayweave.network.RemotePlanOccurrence
import com.greengolddog.dayweave.network.RemoteScheduleBlock
import com.greengolddog.dayweave.network.RemoteSchedulePlan
import com.greengolddog.dayweave.network.RemoteSchedulePreview
import com.greengolddog.dayweave.network.ReplaceCanonicalItemRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.time.ZoneId
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalSyncManagerTest {
    private val clock = Instant.parse("2026-09-01T07:00:00Z")

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
        assertEquals(840, plannerStore.state.value.protectedFreeMinutes)
        assertEquals(100, plannerStore.state.value.dayScore)

        assertNotNull(transport.previewRequest)
        val request = requireNotNull(transport.previewRequest)
        assertEquals("Europe/Madrid", request.timezoneName)
        assertEquals("2026-08-31T22:00:00Z", request.horizonStart)
        assertEquals("2026-09-01T22:00:00Z", request.horizonEnd)
        assertEquals("2026-09-01T05:00:00Z", request.availability.single().start)
        assertEquals("2026-09-01T20:00:00Z", request.availability.single().end)
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
                score = RemotePlanScore(180, 0, 0, 0),
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
            recurrence = buildJsonObject { put("rrule", "FREQ=DAILY") },
        )
        val occurrence = RemotePlanOccurrence(
            id = OCCURRENCE_ID,
            seriesItemId = TASK_ID,
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

    private fun manager(
        plannerStore: PlannerStore,
        transport: FakeCanonicalTransport,
    ) = CanonicalSyncManager(
        plannerStore = plannerStore,
        credentialStore = CanonicalCredentialStore(),
        transport = transport,
        now = { clock },
        zoneId = { ZoneId.of("Europe/Madrid") },
    )

    private fun remoteItem(split: Boolean = true) = RemoteCanonicalItem(
        id = TASK_ID,
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

    private fun preview() = RemoteSchedulePreview(
        inputDigest = "sha256:${"a".repeat(64)}",
        sourceItemCount = 1,
        sourceItemRevisions = mapOf(TASK_ID to 7),
        acceptedItemCount = 1,
        rejectedItems = emptyList(),
        ignoredPreviousAssignments = emptyList(),
        plan = RemoteSchedulePlan(
            asOf = clock.toString(),
            horizonStart = "2026-08-31T22:00:00Z",
            horizonEnd = "2026-09-01T22:00:00Z",
            blocks = listOf(
                RemoteScheduleBlock(
                    id = BLOCK_ID,
                    itemId = TASK_ID,
                    title = "Compose Android timeline",
                    start = "2026-09-01T09:00:00+02:00",
                    end = "2026-09-01T10:00:00+02:00",
                    sessionIndex = 0,
                    kind = "planned",
                ),
            ),
            unscheduled = emptyList(),
            score = RemotePlanScore(
                scheduledMinutes = 60,
                unscheduledMinutes = 0,
                softPenalty = 0,
                movedMinutes = 0,
            ),
        ),
    )

    private companion object {
        const val TASK_ID = "11111111-1111-4111-8111-111111111111"
        const val BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_BLOCK_ID = "33333333-3333-4333-8333-333333333333"
        const val OCCURRENCE_ID = "44444444-4444-4444-8444-444444444444"
    }
}

private class CanonicalCredentialStore : ApiCredentialStore {
    private var lastSync: Long? = null

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = lastSync,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration =
        AuthenticatedApiConfiguration.create("https://api.example.test/", "test-secret")

    override fun update(baseUrl: String, bearerToken: String?) = Unit

    override fun clear() = Unit

    override fun recordSuccessfulSync(epochMillis: Long) {
        lastSync = epochMillis
    }
}

private class FakeCanonicalTransport : CanonicalPlannerTransport {
    val pages = mutableMapOf<String?, RemoteItemDeltaPage>()
    val queuedPages = mutableMapOf<String?, ArrayDeque<RemoteItemDeltaPage>>()
    val deltaCursors = mutableListOf<String?>()
    var previewResult: RemoteSchedulePreview? = null
    var previewRequest: SchedulePreviewRequest? = null
    val queuedPreviews = ArrayDeque<RemoteSchedulePreview>()
    val previewRequests = mutableListOf<SchedulePreviewRequest>()
    var deltaStarted: CompletableDeferred<Unit>? = null
    var deltaGate: CompletableDeferred<Unit>? = null
    var replacementResult: RemoteCanonicalItem? = null
    var replacementRequest: ReplaceCanonicalItemRequest? = null
    var replacementId: String? = null
    var replacementIdempotencyKey: String? = null
    val replacementIdempotencyKeys = mutableListOf<String>()
    var replacementError: Throwable? = null

    override suspend fun itemDelta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
    ): RemoteItemDeltaPage {
        deltaCursors.add(cursor)
        deltaStarted?.complete(Unit)
        deltaGate?.await()
        return queuedPages[cursor]?.removeFirstOrNull() ?: requireNotNull(pages[cursor])
    }

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePreviewRequest,
    ): RemoteSchedulePreview {
        previewRequest = request
        previewRequests.add(request)
        return queuedPreviews.removeFirstOrNull() ?: requireNotNull(previewResult)
    }

    override suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ): RemoteCanonicalItem {
        replacementId = id
        replacementIdempotencyKey = idempotencyKey
        replacementIdempotencyKeys += idempotencyKey
        replacementRequest = request
        replacementError?.let { throw it }
        return requireNotNull(replacementResult)
    }
}
