package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
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
import com.greengolddog.dayweave.state.PlannerLoadState
import java.time.Instant
import java.time.ZoneId
import java.io.IOException
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
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
        val manager = manager(plannerStore, transport)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.refreshAndCompose())
        assertTrue(plannerStore.state.value.canonicalItems.isNotEmpty())

        var changed = false
        manager.withConfigurationUpdateLock(
            requestedBaseUrl = "https://api.example.test/",
            bearerToken = "replacement-secret",
        ) { changed = true }

        assertTrue(changed)
        assertTrue(plannerStore.state.value.canonicalItems.isEmpty())
        assertTrue(plannerStore.state.value.schedule.isEmpty())
        assertEquals(null, plannerStore.state.value.canonicalDeltaCursor)
        assertEquals(null, plannerStore.state.value.canonicalExecutionSyncOrigin)
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
            horizonEnd = "2026-09-01T22:00:00Z",
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

    private fun terminalPreview(revision: Long) = preview().copy(
        sourceItemRevisions = mapOf(TASK_ID to revision),
        plan = preview().plan.copy(
            blocks = emptyList(),
            score = RemotePlanScore(
                scheduledMinutes = 0,
                unscheduledMinutes = 0,
                softPenalty = 0,
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
        const val EXECUTION_ID = "55555555-5555-4555-8555-555555555555"
        const val DEVICE_ID = "66666666-6666-4666-8666-666666666666"
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
    var deltaError: Throwable? = null
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
        replacementRequests += request
        replacementHandler?.let { return it(id, idempotencyKey, request) }
        replacementError?.let { throw it }
        return requireNotNull(replacementResult)
    }
}
