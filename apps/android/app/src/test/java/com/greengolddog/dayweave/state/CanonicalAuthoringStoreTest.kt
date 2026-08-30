package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.CanonicalSplitDraft
import com.greengolddog.dayweave.model.CanonicalSplitKind
import com.greengolddog.dayweave.model.CanonicalTrashRetentionPolicy
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.canonicalTrashItemBytes
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.model.toCanonicalDraft
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringStoreTest {
    @Test
    fun offlineCreateIsDurableAcrossRestartWithoutInventingScheduleState() = runBlocking {
        val repository = MemoryRepository(DayWeaveUiState())
        var scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        var store = PlannerStore(
            initialState = DayWeaveUiState(),
            repository = repository,
            scope = scope,
            nowEpochMillis = { NOW_MILLIS },
        )
        withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }

        val transition = requireNotNull(
            store.enqueueCanonicalCreate(taskDraft(), ITEM_ID, MUTATION_ID),
        )
        assertTrue(withTimeout(3_000) { transition.persistence.awaitDurable() })
        assertEquals(MUTATION_ID, transition.mutation.id)
        assertFalse(transition.mutation.isSubmitted)
        assertNull(transition.mutation.syncOrigin)
        assertTrue(store.state.value.schedule.isEmpty())
        assertTrue(store.state.value.canonicalItems.isEmpty())
        scope.cancel()

        scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
            )
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertEquals(
                transition.mutation,
                store.state.value.pendingCanonicalAuthoringMutations.single(),
            )
            assertTrue(store.state.value.schedule.isEmpty())
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun unsentDraftCanBeEditedWithoutChangingItsRequestIdentity() {
        val store = PlannerStore(boundState(), nowEpochMillis = { NOW_MILLIS })
        val queued = requireNotNull(
            store.enqueueCanonicalCreate(taskDraft(), ITEM_ID, MUTATION_ID),
        ).mutation
        val bound = requireNotNull(
            store.bindCanonicalAuthoringMutation(MUTATION_ID, ORIGIN, CONFIGURATION_ID),
        ).mutation
        assertEquals(queued.idempotencyKey, bound.idempotencyKey)

        val updated = requireNotNull(
            store.updateCanonicalAuthoringDraft(
                MUTATION_ID,
                taskDraft().copy(title = "Edited before the first send"),
            ),
        ).mutation

        assertEquals(queued.id, updated.id)
        assertEquals(queued.itemId, updated.itemId)
        assertEquals(queued.idempotencyKey, updated.idempotencyKey)
        assertEquals(queued.createdAt, updated.createdAt)
        assertEquals("Edited before the first send", updated.draft?.title)
        assertNull(updated.syncOrigin)
        assertNull(updated.configurationId)
        assertFalse(updated.isSubmitted)
        assertEquals(updated, store.state.value.pendingCanonicalAuthoringMutations.single())
    }

    @Test
    fun conflictedDraftCopiesToDetachedSensitiveInboxWithoutDestroyingOriginal() {
        val parent = canonicalItem(PARENT_ID).copy(isSensitive = true)
        val store = PlannerStore(boundState(parent), nowEpochMillis = { NOW_MILLIS })
        val queued = requireNotNull(
            store.enqueueCanonicalCreate(
                taskDraft().copy(isSensitive = false, parentId = PARENT_ID),
                ITEM_ID,
                MUTATION_ID,
            ),
        ).mutation
        val submitted = submit(store, queued)
        val conflicted = requireNotNull(
            store.markCanonicalAuthoringConflict(
                submitted.id,
                "Server rejected the retained contract",
            ),
        ).mutation
        val copyItemId = "12121212-1212-4212-8212-121212121212"
        val copyMutationId = "34343434-3434-4434-8434-343434343434"

        val copy = requireNotNull(
            store.duplicateConflictedCanonicalDraft(
                conflicted.id,
                copyItemId,
                copyMutationId,
            ),
        ).mutation

        assertEquals(
            conflicted,
            store.state.value.pendingCanonicalAuthoringMutations.first { it.id == conflicted.id },
        )
        assertTrue(conflicted.isSubmitted)
        assertEquals(CanonicalAuthoringDisposition.CONFLICTED, conflicted.disposition)
        assertEquals(copyItemId, copy.itemId)
        assertEquals(copyMutationId, copy.id)
        assertFalse(copy.isSubmitted)
        assertNull(copy.syncOrigin)
        assertEquals(CanonicalDraftPlacement.INBOX, copy.draft?.placement)
        assertTrue(copy.draft?.isSensitive == true)
        assertNull(copy.draft?.parentId)
        assertEquals(0L, copy.draft?.siblingOrder)
        assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
    }

    @Test
    fun detachedInboxCaptureDoesNotBlockPausingAnActiveExecutionLease() {
        val active = canonicalItem(ITEM_ID)
        val blockId = "56565656-5656-4656-8656-565656565656"
        val executionId = "78787878-7878-4878-8878-787878787878"
        val deviceId = "90909090-9090-4090-8090-909090909090"
        val running = CanonicalExecutionSessionSnapshot(
            id = executionId,
            itemId = active.id,
            itemRevision = active.revision,
            sessionIndex = 0,
            plannedBlockId = blockId,
            sourceDeviceId = deviceId,
            status = "active",
            revision = 1,
            accumulatedSeconds = 0,
            startedAt = NOW,
            runningSince = NOW,
            createdAt = NOW,
            updatedAt = NOW,
        )
        val block = ScheduleItem(
            id = blockId,
            title = active.title,
            kind = ItemKind.TASK,
            startMinute = 600,
            durationMinutes = 60,
            status = ItemStatus.ACTIVE,
            canonicalItemId = active.id,
            canonicalRevision = active.revision,
            sessionIndex = 0,
        )
        val store = PlannerStore(
            boundState(active).copy(
                schedule = listOf(block),
                canonicalExecutionSyncOrigin = ORIGIN,
                canonicalExecutionConfigurationId = CONFIGURATION_ID,
                canonicalExecutionRevision = 1,
                canonicalExecutionSession = running,
                executionDeviceId = deviceId,
            ),
            nowEpochMillis = { NOW_MILLIS },
        )
        val captureItemId = "abababab-abab-4bab-8bab-abababababab"
        val captureMutationId = "cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd"

        val capture = requireNotNull(
            store.enqueueCanonicalCreate(
                taskDraft().copy(
                    placement = CanonicalDraftPlacement.INBOX,
                    parentId = null,
                ),
                captureItemId,
                captureMutationId,
            ),
        ).mutation
        assertFalse(capture.isSubmitted)
        assertThrows(IllegalArgumentException::class.java) {
            store.enqueueCanonicalCreate(
                taskDraft(),
                "dededede-dede-4ede-8ede-dededededede",
                "efefefef-efef-4fef-8fef-efefefefefef",
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.enqueueCanonicalReplace(active.id, active.toCanonicalDraft())
        }

        val staged = store.stageExecutionCommand(
            PendingExecutionCommand(
                idempotencyKey = "10101010-1010-4010-8010-101010101010",
                syncOrigin = ORIGIN,
                configurationId = CONFIGURATION_ID,
                expectedRevision = 1,
                sessionId = executionId,
                itemId = active.id,
                itemRevision = active.revision,
                sessionIndex = 0,
                plannedBlockId = blockId,
                sourceDeviceId = deviceId,
                commandType = "pause",
                requestJson = "{}",
                focusedBlockId = blockId,
                startedAt = NOW,
            ),
        )

        assertNotNull(staged)
        assertEquals("pause", store.state.value.pendingExecutionCommand?.commandType)
        assertEquals(capture, store.state.value.pendingCanonicalAuthoringMutations.single())
    }

    @Test
    fun hierarchySupportsQueuedParentsButRejectsDuplicatesAndUnlimitedCycles() {
        val parent = canonicalItem(PARENT_ID, parentId = null)
        val child = canonicalItem(CHILD_ID, parentId = PARENT_ID)
        val store = PlannerStore(boundState(parent, child))

        assertThrows(IllegalArgumentException::class.java) {
            store.enqueueCanonicalReplace(
                PARENT_ID,
                parent.toCanonicalDraft().copy(parentId = CHILD_ID),
                MUTATION_ID,
            )
        }

        val queuedParentId = "44444444-4444-4444-8444-444444444444"
        val queuedChildId = "55555555-5555-4555-8555-555555555555"
        assertNotNull(
            store.enqueueCanonicalCreate(
                taskDraft().copy(title = "Queued parent"),
                queuedParentId,
                "66666666-6666-4666-8666-666666666666",
            ),
        )
        assertNotNull(
            store.enqueueCanonicalCreate(
                taskDraft().copy(title = "Queued child", parentId = queuedParentId),
                queuedChildId,
                "77777777-7777-4777-8777-777777777777",
            ),
        )
        assertThrows(IllegalArgumentException::class.java) {
            store.enqueueCanonicalCreate(
                taskDraft().copy(title = "Duplicate child"),
                queuedChildId,
                "88888888-8888-4888-8888-888888888888",
            )
        }
    }

    @Test
    fun submittedMutationIsImmutableFencedAndConflictBecomesDiscardable() {
        val item = canonicalItem(ITEM_ID)
        val store = PlannerStore(boundState(item), nowEpochMillis = { NOW_MILLIS })
        val queued = requireNotNull(
            store.enqueueCanonicalReplace(
                ITEM_ID,
                item.toCanonicalDraft().copy(title = "Edited title"),
                MUTATION_ID,
            ),
        ).mutation
        val bound = requireNotNull(
            store.bindCanonicalAuthoringMutation(MUTATION_ID, ORIGIN, CONFIGURATION_ID),
        ).mutation
        val submitted = requireNotNull(
            store.markCanonicalAuthoringSubmitted(MUTATION_ID),
        ).mutation

        assertEquals(queued.copy(syncOrigin = ORIGIN, configurationId = CONFIGURATION_ID), bound)
        assertTrue(submitted.isSubmitted)
        assertTrue(store.hasCredentialReplacementBlocker())
        assertThrows(IllegalArgumentException::class.java) {
            store.bindCanonicalAuthoringMutation(MUTATION_ID, ORIGIN, CONFIGURATION_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.discardCanonicalAuthoringMutation(MUTATION_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.enqueueCanonicalCreate(
                taskDraft(),
                "99999999-9999-4999-8999-999999999999",
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            )
        }

        val conflicted = requireNotNull(
            store.markCanonicalAuthoringConflict(MUTATION_ID, " revision changed "),
        ).mutation
        assertEquals(CanonicalAuthoringDisposition.CONFLICTED, conflicted.disposition)
        assertEquals("revision changed", conflicted.diagnostic)
        assertTrue(store.hasCredentialReplacementBlocker())
        assertThrows(IllegalArgumentException::class.java) {
            store.abandonCanonicalConnection()
        }
        assertNotNull(store.discardCanonicalAuthoringMutation(MUTATION_ID))
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertFalse(store.hasCredentialReplacementBlocker())
    }

    @Test
    fun credentialReplacementBlocksEveryAuthoringJournalThatCannotSurviveAbandonment() {
        val localParentId = stableUuid("replacement-safe-local-parent")
        val localChildId = stableUuid("replacement-safe-local-child")
        val safe = PlannerStore(boundState(), nowEpochMillis = { NOW_MILLIS })
        requireNotNull(
            safe.enqueueCanonicalCreate(
                taskDraft().copy(title = "Local parent"),
                localParentId,
                stableUuid("replacement-safe-parent-mutation"),
            ),
        )
        requireNotNull(
            safe.enqueueCanonicalCreate(
                taskDraft().copy(title = "Local child", parentId = localParentId),
                localChildId,
                stableUuid("replacement-safe-child-mutation"),
            ),
        )
        assertFalse(safe.hasCredentialReplacementBlocker())

        val remoteParent = canonicalItem(PARENT_ID)
        val remoteDependent = PlannerStore(
            boundState(remoteParent),
            nowEpochMillis = { NOW_MILLIS },
        )
        requireNotNull(
            remoteDependent.enqueueCanonicalCreate(
                taskDraft().copy(title = "Remote-dependent child", parentId = PARENT_ID),
                localChildId,
                stableUuid("replacement-remote-dependent-mutation"),
            ),
        )
        assertTrue(remoteDependent.hasCredentialReplacementBlocker())

        val boundBeforeSubmit = PlannerStore(boundState(), nowEpochMillis = { NOW_MILLIS })
        val boundMutationId = stableUuid("replacement-bound-before-submit")
        requireNotNull(
            boundBeforeSubmit.enqueueCanonicalCreate(
                taskDraft(),
                ITEM_ID,
                boundMutationId,
            ),
        )
        requireNotNull(
            boundBeforeSubmit.bindCanonicalAuthoringMutation(
                boundMutationId,
                ORIGIN,
                CONFIGURATION_ID,
            ),
        )
        assertTrue(boundBeforeSubmit.hasCredentialReplacementBlocker())

        val replacement = PlannerStore(
            boundState(canonicalItem(ITEM_ID)),
            nowEpochMillis = { NOW_MILLIS },
        )
        requireNotNull(
            replacement.enqueueCanonicalReplace(
                ITEM_ID,
                taskDraft().copy(title = "Unsent replacement"),
                MUTATION_ID,
            ),
        )
        assertTrue(replacement.hasCredentialReplacementBlocker())
    }

    @Test
    fun rejectedParentCreateDurablyConflictsQueuedDescendants() = runBlocking {
        val repository = MemoryRepository(boundState())
        var scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        var store = PlannerStore(
            initialState = DayWeaveUiState(),
            repository = repository,
            scope = scope,
            nowEpochMillis = { NOW_MILLIS },
        )
        withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
        val parentItemId = stableUuid("rejected-parent-create-item")
        val childItemId = stableUuid("rejected-child-create-item")
        val parentMutationId = stableUuid("rejected-parent-create-mutation")
        val childMutationId = stableUuid("rejected-child-create-mutation")
        val parent = requireNotNull(
            store.enqueueCanonicalCreate(taskDraft(), parentItemId, parentMutationId),
        ).mutation
        assertNotNull(
            store.enqueueCanonicalCreate(
                taskDraft().copy(parentId = parentItemId),
                childItemId,
                childMutationId,
            ),
        )
        val submitted = submit(store, parent)

        val conflict = requireNotNull(
            store.markCanonicalAuthoringConflict(submitted.id, "server rejected parent"),
        )
        assertTrue(withTimeout(3_000) { conflict.persistence.awaitDurable() })
        assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
            it.disposition == CanonicalAuthoringDisposition.CONFLICTED
        })
        scope.cancel()

        scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { NOW_MILLIS },
            )
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
            assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
                it.disposition == CanonicalAuthoringDisposition.CONFLICTED
            })
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun rejectedParentRestoreConflictsAQueuedChildRestore() {
        val deletedParent = deletedRecord(PARENT_ID, NOW)
        val deletedChildItem = requireNotNull(
            deletedRecord(CHILD_ID, NOW).lastKnownItem,
        ).copy(parentId = PARENT_ID)
        val deletedChild = deletedRecord(CHILD_ID, NOW).copy(
            parentId = PARENT_ID,
            lastKnownItem = deletedChildItem,
        )
        val store = PlannerStore(
            boundState().copy(
                canonicalRecentlyDeleted = listOf(deletedParent, deletedChild),
            ),
            nowEpochMillis = { NOW_MILLIS },
        )
        val parent = requireNotNull(
            store.enqueueCanonicalRestore(PARENT_ID, stableUuid("parent-restore")),
        ).mutation
        assertNotNull(
            store.enqueueCanonicalRestore(CHILD_ID, stableUuid("child-restore")),
        )
        val submitted = submit(store, parent)

        assertNotNull(
            store.markCanonicalAuthoringConflict(submitted.id, "server rejected restore"),
        )
        assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
            it.disposition == CanonicalAuthoringDisposition.CONFLICTED
        })
    }

    @Test
    fun replayedParentTombstoneKeepsAQueuedRestoreTreePending() {
        val deletedParent = deletedRecord(PARENT_ID, NOW)
        val deletedChildItem = requireNotNull(
            deletedRecord(CHILD_ID, NOW).lastKnownItem,
        ).copy(parentId = PARENT_ID)
        val deletedChild = deletedRecord(CHILD_ID, NOW).copy(
            parentId = PARENT_ID,
            lastKnownItem = deletedChildItem,
        )
        val store = PlannerStore(
            boundState().copy(
                canonicalRecentlyDeleted = listOf(deletedParent, deletedChild),
            ),
            nowEpochMillis = { NOW_MILLIS },
        )
        assertNotNull(
            store.enqueueCanonicalRestore(PARENT_ID, stableUuid("replayed-parent-restore")),
        )
        assertNotNull(
            store.enqueueCanonicalRestore(CHILD_ID, stableUuid("replayed-child-restore")),
        )

        assertNotNull(store.recordCanonicalRecentlyDeleted(deletedParent))

        assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        })
        assertEquals(
            listOf(PARENT_ID, CHILD_ID),
            store.sortedCanonicalAuthoringMutations().map { it.itemId },
        )
    }

    @Test
    fun bodylessRestoreTreeKeepsParentFirstSubmissionEligibility() {
        val deletedParent = deletedRecord(PARENT_ID, NOW)
        val deletedChildItem = requireNotNull(
            deletedRecord(CHILD_ID, NOW).lastKnownItem,
        ).copy(parentId = PARENT_ID)
        val deletedChild = deletedRecord(CHILD_ID, NOW).copy(
            parentId = PARENT_ID,
            lastKnownItem = deletedChildItem,
        )
        var clock = NOW_MILLIS
        val store = PlannerStore(
            boundState().copy(
                canonicalRecentlyDeleted = listOf(deletedParent, deletedChild),
            ),
            nowEpochMillis = { clock },
        )
        val parentMutationId = stableUuid("bodyless-parent-restore")
        val childMutationId = stableUuid("bodyless-child-restore")
        assertNotNull(store.enqueueCanonicalRestore(PARENT_ID, parentMutationId))
        assertNotNull(store.enqueueCanonicalRestore(CHILD_ID, childMutationId))

        clock += CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
        store.navigate(AppDestination.INBOX)

        assertTrue(store.state.value.canonicalRecentlyDeleted.all { it.lastKnownItem == null })
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
            it.baseItem == null
        })
        assertEquals(
            listOf(parentMutationId, childMutationId),
            store.sortedCanonicalAuthoringMutations().map { it.id },
        )
        assertNotNull(
            store.bindCanonicalAuthoringMutation(parentMutationId, ORIGIN, CONFIGURATION_ID),
        )
        assertNotNull(store.markCanonicalAuthoringSubmitted(parentMutationId))
    }

    @Test
    fun trashAndRestoreAtomicallyMoveFullItemsThroughRecentlyDeletedState() {
        val item = canonicalItem(ITEM_ID)
        val store = PlannerStore(boundState(item), nowEpochMillis = { NOW_MILLIS })
        val trash = submit(
            store,
            requireNotNull(store.enqueueCanonicalTrash(ITEM_ID, MUTATION_ID)).mutation,
        )
        val deleted = item.copy(
            revision = item.revision + 1,
            isExecutable = false,
            updatedAt = "2026-08-30T10:01:00Z",
            deletedAt = "2026-08-30T10:01:00Z",
        )

        assertNotNull(store.applyCanonicalAuthoringResponse(trash, deleted))
        assertTrue(store.state.value.canonicalItems.isEmpty())
        val record = store.state.value.canonicalRecentlyDeleted.single()
        assertEquals(deleted, record.lastKnownItem)
        assertTrue(record.isSensitive)

        val restoreQueued = requireNotNull(
            store.enqueueCanonicalRestore(
                ITEM_ID,
                "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            ),
        ).mutation
        val restore = submit(store, restoreQueued)
        val restored = deleted.copy(
            revision = deleted.revision + 1,
            updatedAt = "2026-08-30T10:02:00Z",
            deletedAt = null,
        )
        assertNotNull(store.applyCanonicalAuthoringResponse(restore, restored))
        assertEquals(restored, store.state.value.canonicalItems.single())
        assertTrue(store.state.value.canonicalRecentlyDeleted.isEmpty())
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.isEmpty())
    }

    @Test
    fun mismatchedResponseCannotConsumeSubmittedJournal() {
        val store = PlannerStore(boundState(), nowEpochMillis = { NOW_MILLIS })
        val queued = requireNotNull(
            store.enqueueCanonicalCreate(taskDraft(), ITEM_ID, MUTATION_ID),
        ).mutation
        val submitted = submit(store, queued)
        val wrong = canonicalItem(ITEM_ID, revision = 1).copy(title = "Different title")

        assertThrows(IllegalArgumentException::class.java) {
            store.applyCanonicalAuthoringResponse(submitted, wrong)
        }
        assertEquals(submitted, store.state.value.pendingCanonicalAuthoringMutations.single())
        assertTrue(store.state.value.canonicalItems.isEmpty())
    }

    @Test
    fun pendingAuthoringRaisesEffectiveSensitivityAndHardensDescendantSchedule() {
        val parent = canonicalItem(PARENT_ID).copy(isSensitive = false)
        val child = canonicalItem(CHILD_ID, parentId = PARENT_ID).copy(isSensitive = false)
        val mutation = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = PARENT_ID,
            operation = com.greengolddog.dayweave.model.CanonicalAuthoringOperation.REPLACE,
            draft = parent.toCanonicalDraft().copy(isSensitive = true),
            expectedRevision = parent.revision,
            baseItem = parent,
            createdAt = NOW,
        )
        val block = ScheduleItem(
            id = "scheduled-child",
            isSensitive = false,
            title = child.title,
            kind = ItemKind.TASK,
            startMinute = 600,
            durationMinutes = 60,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = child.id,
            canonicalRevision = child.revision,
        )
        val store = PlannerStore(
            boundState(parent, child).copy(
                pendingCanonicalAuthoringMutations = listOf(mutation),
                schedule = listOf(block),
            ),
            nowEpochMillis = { NOW_MILLIS },
        )

        assertTrue(
            effectiveCanonicalSensitivity(
                listOf(parent, child),
                child.id,
                pendingAuthoringMutations = listOf(mutation),
            ),
        )
        assertTrue(store.state.value.schedule.single().isSensitive)
    }

    @Test
    fun trashRetentionIsRestoreAwareAndBoundsAgeCountAndPerBodyBytes() {
        val recent = (0..CanonicalTrashRetentionPolicy.MAX_ENTRIES).map { offset ->
            deletedRecord(
                id = stableUuid("recent-$offset"),
                deletedAt = Instant.ofEpochMilli(NOW_MILLIS - offset * 1_000L).toString(),
            )
        }
        val expired = deletedRecord(
            id = stableUuid("expired"),
            deletedAt = Instant.ofEpochMilli(
                NOW_MILLIS -
                    (CanonicalTrashRetentionPolicy.RETENTION_SECONDS + 1L) * 1_000L,
            ).toString(),
        )
        val pinned = deletedRecord(
            id = stableUuid("pinned"),
            deletedAt = Instant.ofEpochMilli(
                NOW_MILLIS -
                    (CanonicalTrashRetentionPolicy.RETENTION_SECONDS + 2L) * 1_000L,
            ).toString(),
        ).let { record ->
            record.copy(
                lastKnownItem = requireNotNull(record.lastKnownItem).copy(isSensitive = false),
                effectiveIsSensitive = false,
            )
        }
        val oversized = deletedRecord(
            id = stableUuid("oversized"),
            deletedAt = Instant.ofEpochMilli(NOW_MILLIS + 1_000L).toString(),
            notes = "x".repeat(CanonicalTrashRetentionPolicy.MAX_ITEM_BYTES + 1),
        ).let { record ->
            record.copy(
                lastKnownItem = requireNotNull(record.lastKnownItem).copy(isSensitive = false),
                effectiveIsSensitive = false,
            )
        }
        val restore = PendingCanonicalAuthoringMutation(
            id = stableUuid("restore-mutation"),
            itemId = pinned.id,
            operation = com.greengolddog.dayweave.model.CanonicalAuthoringOperation.RESTORE,
            expectedRevision = pinned.revision,
            createdAt = NOW,
        )

        val store = PlannerStore(
            DayWeaveUiState(
                pendingCanonicalAuthoringMutations = listOf(restore),
                canonicalRecentlyDeleted = recent + expired + pinned + oversized,
            ),
            nowEpochMillis = { NOW_MILLIS },
        )
        val retained = store.state.value.canonicalRecentlyDeleted

        assertEquals(CanonicalTrashRetentionPolicy.MAX_ENTRIES, retained.size)
        assertFalse(retained.any { it.id == expired.id })
        assertTrue(retained.any { it.id == pinned.id })
        assertNull(retained.single { it.id == pinned.id }.lastKnownItem)
        assertTrue(retained.single { it.id == pinned.id }.isSensitive)
        assertNull(retained.single { it.id == oversized.id }.lastKnownItem)
        assertTrue(retained.single { it.id == oversized.id }.isSensitive)
    }

    @Test
    fun trashRetentionBoundsAggregateBodiesAndPreservesEqualRevisionReplayBody() {
        val bodyRecords = (0 until 50).map { offset ->
            deletedRecord(
                id = stableUuid("body-$offset"),
                deletedAt = Instant.ofEpochMilli(NOW_MILLIS - offset * 1_000L).toString(),
                notes = "y".repeat(100_000),
            )
        }
        val bounded = PlannerStore(
            DayWeaveUiState(canonicalRecentlyDeleted = bodyRecords),
            nowEpochMillis = { NOW_MILLIS },
        ).state.value.canonicalRecentlyDeleted
        val retainedBytes = bounded.sumOf {
            (it.lastKnownItem?.let(::canonicalTrashItemBytes) ?: 0).toLong()
        }
        assertTrue(retainedBytes <= CanonicalTrashRetentionPolicy.MAX_RETAINED_ITEM_BYTES)
        assertTrue(bounded.any { it.lastKnownItem == null })

        val full = deletedRecord(
            id = ITEM_ID,
            deletedAt = NOW,
            notes = "full recovery body",
        )
        val replayStore = PlannerStore(
            DayWeaveUiState(canonicalRecentlyDeleted = listOf(full)),
            nowEpochMillis = { NOW_MILLIS },
        )
        assertNotNull(replayStore.recordCanonicalRecentlyDeleted(full.copy(lastKnownItem = null)))
        assertEquals(
            full.lastKnownItem,
            replayStore.state.value.canonicalRecentlyDeleted.single().lastKnownItem,
        )
    }

    @Test
    fun planRefreshPreservesHierarchyOverlayAndAtomicallyReconcilesRestore() {
        val parent = canonicalItem(PARENT_ID)
        val overlayStore = PlannerStore(
            boundState(parent),
            nowEpochMillis = { NOW_MILLIS },
        )
        val childId = stableUuid("refresh-child")
        val childMutationId = stableUuid("refresh-child-mutation")
        assertNotNull(
            overlayStore.enqueueCanonicalCreate(
                taskDraft().copy(parentId = parent.id),
                childId,
                childMutationId,
            ),
        )

        assertNotNull(overlayStore.replaceCanonicalPlan(canonicalUpdate(emptyList(), "cursor-2")))
        assertEquals(parent, overlayStore.state.value.canonicalItems.single())
        assertEquals(
            childMutationId,
            overlayStore.state.value.pendingCanonicalAuthoringMutations.single().id,
        )

        val deleted = deletedRecord(ITEM_ID, NOW)
        val restoreStore = PlannerStore(
            boundState().copy(canonicalRecentlyDeleted = listOf(deleted)),
            nowEpochMillis = { NOW_MILLIS },
        )
        val restore = requireNotNull(
            restoreStore.enqueueCanonicalRestore(ITEM_ID, MUTATION_ID),
        ).mutation
        assertNotNull(restoreStore.replaceCanonicalPlan(canonicalUpdate(emptyList(), "cursor-2")))
        assertEquals(restore, restoreStore.state.value.pendingCanonicalAuthoringMutations.single())
        assertEquals(deleted, restoreStore.state.value.canonicalRecentlyDeleted.single())

        val restored = requireNotNull(deleted.lastKnownItem).copy(
            revision = deleted.revision + 1,
            isExecutable = true,
            updatedAt = "2026-08-30T10:01:00Z",
            deletedAt = null,
        )
        assertNotNull(
            restoreStore.replaceCanonicalPlan(canonicalUpdate(listOf(restored), "cursor-3")),
        )
        assertEquals(restored, restoreStore.state.value.canonicalItems.single())
        assertTrue(restoreStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertTrue(restoreStore.state.value.canonicalRecentlyDeleted.isEmpty())
    }

    @Test
    fun parentTombstoneDurablyConflictsDependentCreateAndReplaceWithoutMaskingDeletion() =
        runBlocking {
            val parent = canonicalItem(PARENT_ID)
            val mover = canonicalItem(MOVER_ID)
            val repository = MemoryRepository(boundState(parent, mover))
            var scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            var store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { NOW_MILLIS },
            )
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertNotNull(
                store.enqueueCanonicalCreate(
                    taskDraft().copy(parentId = parent.id),
                    CHILD_ID,
                    CHILD_CREATE_MUTATION_ID,
                ),
            )
            assertNotNull(
                store.enqueueCanonicalReplace(
                    mover.id,
                    mover.toCanonicalDraft().copy(parentId = parent.id),
                    MOVER_REPLACE_MUTATION_ID,
                ),
            )
            val tombstone = deletedRecord(
                parent.id,
                Instant.ofEpochMilli(NOW_MILLIS + 60_000L).toString(),
            )

            val receipt = requireNotNull(store.recordCanonicalRecentlyDeleted(tombstone))
            assertTrue(withTimeout(3_000) { receipt.awaitDurable() })
            assertEquals(listOf(mover.id), store.state.value.canonicalItems.map { it.id })
            assertEquals(parent.id, store.state.value.canonicalRecentlyDeleted.single().id)
            val conflicted = store.state.value.pendingCanonicalAuthoringMutations
            assertEquals(2, conflicted.size)
            assertTrue(conflicted.all {
                it.disposition == CanonicalAuthoringDisposition.CONFLICTED &&
                    it.diagnostic?.contains("parent was deleted remotely") == true
            })
            assertEquals(
                setOf(parent.id),
                conflicted.mapNotNull { it.draft?.parentId }.toSet(),
            )
            scope.cancel()

            scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                store = PlannerStore(
                    initialState = DayWeaveUiState(),
                    repository = repository,
                    scope = scope,
                    nowEpochMillis = { NOW_MILLIS },
                )
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
                assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
                assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
                    it.disposition == CanonicalAuthoringDisposition.CONFLICTED
                })

                assertNotNull(
                    store.replaceCanonicalPlan(
                        canonicalUpdate(listOf(mover), "cursor-after-parent-delete"),
                    ),
                )
                assertEquals(listOf(mover.id), store.state.value.canonicalItems.map { it.id })
                assertFalse(store.state.value.canonicalItems.any { it.id == parent.id })
                assertEquals(parent.id, store.state.value.canonicalRecentlyDeleted.single().id)
            } finally {
                scope.cancel()
            }
        }

    @Test
    fun refreshFiltersNewerScheduleRevisionWhenPendingOverlayRetainsOldItem() {
        val old = canonicalItem(ITEM_ID, revision = 7)
        val publicationId = stableUuid("published-revision")
        val published = PublishedScheduleRevisionSnapshot(
            id = publicationId,
            revision = "1:$publicationId",
            revisionNumber = 1uL,
            inputDigest = "sha256:${"b".repeat(64)}",
            horizonStart = "2026-08-30T00:00:00Z",
            horizonEnd = "2026-08-31T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = NOW,
        )
        val overlaidStore = PlannerStore(
            boundState(old).copy(
                publishedScheduleRevision = published,
                scheduleInputDigest = published.inputDigest,
            ),
            nowEpochMillis = { NOW_MILLIS },
        )
        assertNotNull(
            overlaidStore.enqueueCanonicalReplace(
                old.id,
                old.toCanonicalDraft().copy(title = "Local draft"),
                MUTATION_ID,
            ),
        )
        val fresh = old.copy(
            revision = 8,
            title = "Remote revision",
            updatedAt = "2026-08-30T10:01:00Z",
        )
        val freshBlock = ScheduleItem(
            id = "fresh-revision-block",
            isSensitive = fresh.isSensitive,
            title = fresh.title,
            kind = ItemKind.TASK,
            startMinute = 600,
            durationMinutes = 60,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = fresh.id,
            canonicalRevision = fresh.revision,
        )
        val update = canonicalUpdate(listOf(fresh), "cursor-revision-8").copy(
            schedule = listOf(freshBlock),
        )

        assertNotNull(overlaidStore.replaceCanonicalPlan(update))

        assertEquals(old, overlaidStore.state.value.canonicalItems.single())
        assertTrue(overlaidStore.state.value.schedule.isEmpty())
        assertNull(overlaidStore.state.value.scheduleInputDigest)
        assertNull(overlaidStore.state.value.publishedScheduleRevision)

        val exactStore = PlannerStore(boundState(old), nowEpochMillis = { NOW_MILLIS })
        assertNotNull(exactStore.replaceCanonicalPlan(update))
        assertEquals(fresh, exactStore.state.value.canonicalItems.single())
        assertEquals(freshBlock, exactStore.state.value.schedule.single())
        assertEquals(update.inputDigest, exactStore.state.value.scheduleInputDigest)
    }

    @Test
    fun newerDeletionConflictsSubmittedRestoreWithoutDroppingItsJournal() {
        val deleted = deletedRecord(ITEM_ID, NOW)
        val store = PlannerStore(
            boundState().copy(canonicalRecentlyDeleted = listOf(deleted)),
            nowEpochMillis = { NOW_MILLIS },
        )
        val submitted = submit(
            store,
            requireNotNull(store.enqueueCanonicalRestore(ITEM_ID, MUTATION_ID)).mutation,
        )
        val newerItem = requireNotNull(deleted.lastKnownItem).copy(
            revision = deleted.revision + 1,
            updatedAt = "2026-08-30T10:02:00Z",
            deletedAt = "2026-08-30T10:02:00Z",
        )
        val newer = deleted.copy(
            revision = newerItem.revision,
            deletedAt = requireNotNull(newerItem.deletedAt),
            lastKnownItem = newerItem,
        )

        assertNotNull(store.recordCanonicalRecentlyDeleted(newer))

        val retained = store.state.value.pendingCanonicalAuthoringMutations.single()
        assertEquals(submitted.id, retained.id)
        assertEquals(CanonicalAuthoringDisposition.CONFLICTED, retained.disposition)
        assertEquals(newer.revision, store.state.value.canonicalRecentlyDeleted.single().revision)
        assertTrue(store.hasCredentialReplacementBlocker())
    }

    @Test
    fun deletionStillRejectsAnUnresolvedActiveChildWithoutAnOverlay() {
        val parent = canonicalItem(PARENT_ID)
        val child = canonicalItem(CHILD_ID, parentId = PARENT_ID)
        val store = PlannerStore(
            boundState(parent, child),
            nowEpochMillis = { NOW_MILLIS },
        )
        val deletedParent = parent.copy(
            revision = parent.revision + 1,
            isExecutable = false,
            updatedAt = "2026-08-30T10:01:00Z",
            deletedAt = "2026-08-30T10:01:00Z",
        )
        val record = CanonicalRecentlyDeletedRecord(
            id = parent.id,
            revision = deletedParent.revision,
            deletedAt = requireNotNull(deletedParent.deletedAt),
            lastKnownItem = deletedParent,
            effectiveIsSensitive = true,
            retentionAnchorAt = requireNotNull(deletedParent.deletedAt),
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.recordCanonicalRecentlyDeleted(record)
        }
        assertEquals(setOf(parent.id, child.id), store.state.value.canonicalItems.map { it.id }.toSet())
    }

    @Test
    fun discardRejectsAParentWhoseRemainingDraftDependsOnIt() {
        val store = PlannerStore(boundState(), nowEpochMillis = { NOW_MILLIS })
        val parentItemId = stableUuid("discard-parent-item")
        val childItemId = stableUuid("discard-child-item")
        val parentMutationId = stableUuid("discard-parent-mutation")
        val childMutationId = stableUuid("discard-child-mutation")
        assertNotNull(
            store.enqueueCanonicalCreate(taskDraft(), parentItemId, parentMutationId),
        )
        assertNotNull(
            store.enqueueCanonicalCreate(
                taskDraft().copy(parentId = parentItemId),
                childItemId,
                childMutationId,
            ),
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.discardCanonicalAuthoringMutation(parentMutationId)
        }
        assertEquals(2, store.state.value.pendingCanonicalAuthoringMutations.size)
        assertNotNull(store.discardCanonicalAuthoringMutation(childMutationId))
        assertNotNull(store.discardCanonicalAuthoringMutation(parentMutationId))
    }

    @Test
    fun submissionOrderIsTopologicalForCreatesAndTrashRegardlessOfUuidOrder() {
        val parentItemId = stableUuid("ordered-parent-item")
        val childItemId = stableUuid("ordered-child-item")
        val parentMutationId = "ffffffff-ffff-4fff-8fff-ffffffffffff"
        val childMutationId = "00000000-0000-4000-8000-000000000001"
        val createStore = PlannerStore(boundState(), nowEpochMillis = { NOW_MILLIS })
        assertNotNull(
            createStore.enqueueCanonicalCreate(taskDraft(), parentItemId, parentMutationId),
        )
        assertNotNull(
            createStore.enqueueCanonicalCreate(
                taskDraft().copy(parentId = parentItemId),
                childItemId,
                childMutationId,
            ),
        )
        assertEquals(
            listOf(parentMutationId, childMutationId),
            createStore.sortedCanonicalAuthoringMutations().map { it.id },
        )
        assertNotNull(
            createStore.bindCanonicalAuthoringMutation(childMutationId, ORIGIN, CONFIGURATION_ID),
        )
        assertThrows(IllegalArgumentException::class.java) {
            createStore.markCanonicalAuthoringSubmitted(childMutationId)
        }

        val parent = canonicalItem(parentItemId)
        val child = canonicalItem(childItemId, parentId = parentItemId)
        val trashStore = PlannerStore(
            boundState(parent, child),
            nowEpochMillis = { NOW_MILLIS },
        )
        assertNotNull(trashStore.enqueueCanonicalTrash(child.id, childMutationId))
        assertNotNull(trashStore.enqueueCanonicalTrash(parent.id, parentMutationId))
        assertEquals(
            listOf(childMutationId, parentMutationId),
            trashStore.sortedCanonicalAuthoringMutations().map { it.id },
        )
        assertNotNull(
            trashStore.bindCanonicalAuthoringMutation(parentMutationId, ORIGIN, CONFIGURATION_ID),
        )
        assertThrows(IllegalArgumentException::class.java) {
            trashStore.markCanonicalAuthoringSubmitted(parentMutationId)
        }
    }

    @Test
    fun expiredTrashBodiesKeepChildBeforeParentSubmissionOrder() {
        val parentItemId = stableUuid("expired-ordered-parent-item")
        val childItemId = stableUuid("expired-ordered-child-item")
        val parentMutationId = "00000000-0000-4000-8000-000000000001"
        val childMutationId = "ffffffff-ffff-4fff-8fff-ffffffffffff"
        val parent = canonicalItem(parentItemId)
        val child = canonicalItem(childItemId, parentId = parentItemId)
        var clock = NOW_MILLIS
        val store = PlannerStore(
            boundState(parent, child),
            nowEpochMillis = { clock },
        )
        assertNotNull(store.enqueueCanonicalTrash(child.id, childMutationId))
        assertNotNull(store.enqueueCanonicalTrash(parent.id, parentMutationId))

        clock += CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
        store.navigate(AppDestination.INBOX)

        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.all {
            it.baseItem == null
        })
        assertEquals(
            listOf(childMutationId, parentMutationId),
            store.sortedCanonicalAuthoringMutations().map { it.id },
        )
        assertNotNull(
            store.bindCanonicalAuthoringMutation(parentMutationId, ORIGIN, CONFIGURATION_ID),
        )
        assertThrows(IllegalArgumentException::class.java) {
            store.markCanonicalAuthoringSubmitted(parentMutationId)
        }
    }

    @Test
    fun restoreJournalAndTrashBodiesExpireTogetherAfterSevenDays() {
        val deleted = deletedRecord(ITEM_ID, NOW)
        var clock = Instant.parse(NOW).toEpochMilli() + 1_000L
        val store = PlannerStore(
            boundState().copy(canonicalRecentlyDeleted = listOf(deleted)),
            nowEpochMillis = { clock },
        )
        assertNotNull(store.enqueueCanonicalRestore(ITEM_ID, MUTATION_ID))
        assertNotNull(store.state.value.pendingCanonicalAuthoringMutations.single().baseItem)

        clock = Instant.parse(NOW).toEpochMilli() +
            CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L
        store.navigate(AppDestination.INBOX)
        assertNotNull(store.state.value.pendingCanonicalAuthoringMutations.single().baseItem)

        clock += 1L
        store.navigate(AppDestination.TODAY)
        assertNull(store.state.value.pendingCanonicalAuthoringMutations.single().baseItem)
        assertNull(store.state.value.canonicalRecentlyDeleted.single().lastKnownItem)
    }

    @Test
    fun expiredTrashBodyPreservesReplayIdentityAcrossSubmittedConflictStates() {
        val item = canonicalItem(ITEM_ID)
        var clock = NOW_MILLIS
        val store = PlannerStore(boundState(item), nowEpochMillis = { clock })
        val submitted = submit(
            store,
            requireNotNull(store.enqueueCanonicalTrash(ITEM_ID, MUTATION_ID)).mutation,
        )
        val idempotencyKey = submitted.idempotencyKey

        clock += CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
        store.navigate(AppDestination.INBOX)

        val bodyless = store.state.value.pendingCanonicalAuthoringMutations.single()
        assertNull(bodyless.baseItem)
        assertEquals(submitted.id, bodyless.id)
        assertEquals(submitted.itemId, bodyless.itemId)
        assertEquals(submitted.expectedRevision, bodyless.expectedRevision)
        assertEquals(idempotencyKey, bodyless.idempotencyKey)
        assertEquals(submitted.submittedAt, bodyless.submittedAt)

        val conflicted = requireNotNull(
            store.markCanonicalAuthoringConflict(bodyless.id, "server revision changed"),
        ).mutation
        assertNull(conflicted.baseItem)
        assertEquals(idempotencyKey, conflicted.idempotencyKey)
        assertEquals(CanonicalAuthoringDisposition.CONFLICTED, conflicted.disposition)
    }

    @Test
    fun submissionTransitionUsesTheExactBodylessGenerationProducedAtExpiry() {
        val item = canonicalItem(ITEM_ID)
        var clock = NOW_MILLIS
        val store = PlannerStore(boundState(item), nowEpochMillis = { clock })
        val queued = requireNotNull(store.enqueueCanonicalTrash(ITEM_ID, MUTATION_ID)).mutation
        assertNotNull(store.bindCanonicalAuthoringMutation(queued.id, ORIGIN, CONFIGURATION_ID))

        clock += CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
        val submitted = requireNotNull(store.markCanonicalAuthoringSubmitted(queued.id)).mutation

        assertNull(submitted.baseItem)
        assertEquals(submitted, store.canonicalAuthoringMutation(queued.id))
        val deletedAt = Instant.ofEpochMilli(clock).toString()
        val response = item.copy(
            revision = item.revision + 1,
            isExecutable = false,
            updatedAt = deletedAt,
            deletedAt = deletedAt,
        )
        assertNotNull(store.applyCanonicalAuthoringResponse(submitted, response))
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.isEmpty())
    }

    @Test
    fun responseFenceAcceptsOnlyTheBodylessProjectionOfAnInflightTrashRequest() {
        val item = canonicalItem(ITEM_ID)
        var clock = NOW_MILLIS
        val store = PlannerStore(boundState(item), nowEpochMillis = { clock })
        val submitted = submit(
            store,
            requireNotNull(store.enqueueCanonicalTrash(ITEM_ID, MUTATION_ID)).mutation,
        )
        assertNotNull(submitted.baseItem)

        clock += CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
        store.navigate(AppDestination.INBOX)
        assertNull(store.canonicalAuthoringMutation(submitted.id)?.baseItem)
        val deletedAt = Instant.ofEpochMilli(clock).toString()
        val response = item.copy(
            revision = item.revision + 1,
            isExecutable = false,
            updatedAt = deletedAt,
            deletedAt = deletedAt,
        )

        assertNotNull(store.applyCanonicalAuthoringResponse(submitted, response))
        assertTrue(store.state.value.pendingCanonicalAuthoringMutations.isEmpty())
    }

    @Test
    fun delayedTrashResponseReconcilesAfterEveryRecoveryBodyExpires() {
        val item = canonicalItem(ITEM_ID)
        val stagingStore = PlannerStore(boundState(item), nowEpochMillis = { NOW_MILLIS })
        val submitted = submit(
            stagingStore,
            requireNotNull(stagingStore.enqueueCanonicalTrash(ITEM_ID, MUTATION_ID)).mutation,
        )
        val deleted = item.copy(
            revision = item.revision + 1,
            isExecutable = false,
            updatedAt = "2026-08-30T10:01:00Z",
            deletedAt = "2026-08-30T10:01:00Z",
        )
        val bodylessRecord = CanonicalRecentlyDeletedRecord(
            id = deleted.id,
            revision = deleted.revision,
            deletedAt = requireNotNull(deleted.deletedAt),
            parentId = deleted.parentId,
            lastKnownItem = null,
            effectiveIsSensitive = true,
            retentionAnchorAt = NOW,
        )
        val replayStore = PlannerStore(
            boundState().copy(
                pendingCanonicalAuthoringMutations = listOf(submitted.copy(baseItem = null)),
                canonicalRecentlyDeleted = listOf(bodylessRecord),
            ),
            nowEpochMillis = {
                NOW_MILLIS + CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
            },
        )

        assertNotNull(
            replayStore.applyCanonicalAuthoringResponse(
                requireNotNull(replayStore.canonicalAuthoringMutation(MUTATION_ID)),
                deleted,
            ),
        )
        assertTrue(replayStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertTrue(replayStore.state.value.canonicalItems.isEmpty())
        assertTrue(replayStore.state.value.canonicalRecentlyDeleted.isEmpty())
    }

    @Test
    fun futureTombstoneAnchorSurvivesRestartAndQuietTimerDurablyStripsBodies() = runBlocking {
        val futureDeletedAt = Instant.ofEpochMilli(NOW_MILLIS)
            .plusSeconds(365L * 24L * 60L * 60L)
            .toString()
        val futureDeleted = deletedRecord(DELETED_ITEM_ID, futureDeletedAt).let { record ->
            record.copy(
                lastKnownItem = requireNotNull(record.lastKnownItem).copy(isSensitive = false),
                effectiveIsSensitive = false,
                retentionAnchorAt = null,
            )
        }
        val restore = PendingCanonicalAuthoringMutation(
            id = RESTORE_MUTATION_ID,
            itemId = futureDeleted.id,
            operation = com.greengolddog.dayweave.model.CanonicalAuthoringOperation.RESTORE,
            expectedRevision = futureDeleted.revision,
            baseItem = futureDeleted.lastKnownItem,
            createdAt = NOW,
        )
        val repository = MemoryRepository(
            boundState(canonicalItem(ITEM_ID)).copy(
                pendingCanonicalAuthoringMutations = listOf(restore),
                canonicalRecentlyDeleted = listOf(futureDeleted),
            ),
        )
        var clock = NOW_MILLIS
        var scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val firstScheduler = ManualCanonicalTrashCleanupScheduler()
        var store = PlannerStore(
            initialState = DayWeaveUiState(),
            repository = repository,
            scope = scope,
            nowEpochMillis = { clock },
            cleanupScheduler = firstScheduler,
        )
        withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
        val trash = requireNotNull(
            store.enqueueCanonicalTrash(ITEM_ID, MUTATION_ID),
        )
        assertTrue(withTimeout(3_000) { trash.persistence.awaitDurable() })
        val observedAnchor = Instant.ofEpochMilli(NOW_MILLIS).toString()
        assertEquals(
            observedAnchor,
            repository.snapshot()?.canonicalRecentlyDeleted?.single()?.retentionAnchorAt,
        )
        scope.cancel()

        clock += 24L * 60L * 60L * 1_000L
        scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val restartScheduler = ManualCanonicalTrashCleanupScheduler()
        try {
            store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { clock },
                cleanupScheduler = restartScheduler,
            )
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertEquals(
                observedAnchor,
                store.state.value.canonicalRecentlyDeleted.single().retentionAnchorAt,
            )
            assertNotNull(
                store.state.value.pendingCanonicalAuthoringMutations
                    .first { it.operation ==
                        com.greengolddog.dayweave.model.CanonicalAuthoringOperation.TRASH }
                    .baseItem,
            )

            clock = NOW_MILLIS +
                CanonicalTrashRetentionPolicy.RETENTION_SECONDS * 1_000L + 1L
            restartScheduler.runNext()

            assertTrue(
                store.state.value.pendingCanonicalAuthoringMutations.all { it.baseItem == null },
            )
            val retained = store.state.value.canonicalRecentlyDeleted.single()
            assertNull(retained.lastKnownItem)
            assertTrue(retained.isSensitive)
            withTimeout(3_000) {
                while (
                    repository.snapshot()?.pendingCanonicalAuthoringMutations
                        ?.any { it.baseItem != null } != false
                ) {
                    delay(10)
                }
            }
            assertNull(repository.snapshot()?.canonicalRecentlyDeleted?.single()?.lastKnownItem)
            assertEquals(
                observedAnchor,
                repository.snapshot()?.canonicalRecentlyDeleted?.single()?.retentionAnchorAt,
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun trashRetainsInheritedSensitivityWithoutChangingRestorableOwnMark() {
        val parent = canonicalItem(PARENT_ID).copy(isSensitive = true)
        val child = canonicalItem(CHILD_ID, parentId = PARENT_ID).copy(isSensitive = false)
        val store = PlannerStore(
            boundState(parent, child),
            nowEpochMillis = { NOW_MILLIS },
        )
        val submitted = submit(
            store,
            requireNotNull(store.enqueueCanonicalTrash(CHILD_ID, MUTATION_ID)).mutation,
        )
        val deletedChild = child.copy(
            revision = child.revision + 1,
            isExecutable = false,
            updatedAt = "2026-08-30T10:01:00Z",
            deletedAt = "2026-08-30T10:01:00Z",
        )

        assertNotNull(store.applyCanonicalAuthoringResponse(submitted, deletedChild))

        val retained = store.state.value.canonicalRecentlyDeleted.single()
        assertTrue(retained.isSensitive)
        assertFalse(requireNotNull(retained.lastKnownItem).isSensitive)
    }

    @Test
    fun authoringGenerationCannotBeUsedWhenItsEncryptedSaveFails() = runBlocking {
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = DayWeaveUiState()

            override suspend fun save(state: DayWeaveUiState) {
                throw IllegalStateException("synthetic encrypted save failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(DayWeaveUiState(), repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val transition = requireNotNull(
                store.enqueueCanonicalCreate(taskDraft(), ITEM_ID, MUTATION_ID),
            )

            assertFalse(withTimeout(3_000) { transition.persistence.awaitDurable() })
            assertEquals(
                PlannerLoadState.PERSISTENCE_FAILED,
                withTimeout(3_000) {
                    store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED }
                },
            )
            assertNull(store.bindCanonicalAuthoringMutation(MUTATION_ID, ORIGIN, CONFIGURATION_ID))
        } finally {
            scope.cancel()
        }
    }

    private fun submit(
        store: PlannerStore,
        queued: com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation,
    ): com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation {
        requireNotNull(
            store.bindCanonicalAuthoringMutation(queued.id, ORIGIN, CONFIGURATION_ID),
        )
        return requireNotNull(store.markCanonicalAuthoringSubmitted(queued.id)).mutation
    }

    private fun taskDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.TASK,
        isSensitive = true,
        title = "Canonical Android draft",
        timezoneName = "UTC",
        durationSeconds = 3_600,
        split = CanonicalSplitDraft(
            kind = CanonicalSplitKind.SPLITTABLE,
            minimumChunkSeconds = 900,
            maximumChunkSeconds = 1_800,
        ),
        importance = 75,
        urgency = 65,
    )

    private fun canonicalItem(
        id: String,
        parentId: String? = null,
        revision: Long = 7,
    ): CanonicalItemSnapshot = CanonicalItemSnapshot(
        id = id,
        isSensitive = true,
        kind = "task",
        status = "planned",
        title = "Canonical Android draft",
        timezoneName = "UTC",
        durationSeconds = 3_600,
        flexibleConstraintsJson = "{}",
        splitPolicyJson =
            "{\"type\":\"splittable\",\"minimum_chunk_seconds\":900," +
                "\"maximum_chunk_seconds\":1800}",
        importance = 75,
        urgency = 65,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = true,
        revision = revision,
        createdAt = "2026-08-30T09:00:00Z",
        updatedAt = "2026-08-30T09:00:00Z",
    )

    private fun deletedRecord(
        id: String,
        deletedAt: String,
        notes: String? = null,
    ): CanonicalRecentlyDeletedRecord {
        val createdAt = Instant.parse(deletedAt).minusSeconds(60).toString()
        val item = canonicalItem(id, revision = 8).copy(
            notes = notes,
            isExecutable = false,
            createdAt = createdAt,
            updatedAt = deletedAt,
            deletedAt = deletedAt,
        )
        return CanonicalRecentlyDeletedRecord(
            id = id,
            revision = item.revision,
            deletedAt = deletedAt,
            parentId = item.parentId,
            lastKnownItem = item,
            retentionAnchorAt = deletedAt,
        )
    }

    private fun canonicalUpdate(
        items: List<CanonicalItemSnapshot>,
        cursor: String,
    ) = CanonicalPlanUpdate(
        items = items,
        schedule = emptyList(),
        syncOrigin = ORIGIN,
        configurationId = CONFIGURATION_ID,
        deltaCursor = cursor,
        inputDigest = "sha256:${"a".repeat(64)}",
        generatedAt = NOW,
        planningZoneId = "UTC",
        rejectedItemCount = 0,
        unscheduledItemCount = items.size,
        protectedFreeMinutes = 0,
        dayScore = 100,
        violationMessages = emptyList(),
        violationCount = 0,
        errorViolationCount = 0,
        unscheduledWork = emptyList(),
        occurrenceSeriesItemIds = emptyMap(),
        message = "Refreshed",
    )

    private fun stableUuid(seed: String): String =
        UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private fun boundState(vararg items: CanonicalItemSnapshot) = DayWeaveUiState(
        canonicalItems = items.toList(),
        canonicalSyncOrigin = ORIGIN,
        canonicalConfigurationId = CONFIGURATION_ID,
        canonicalDeltaCursor = "cursor-1",
    )

    private class MemoryRepository(initial: DayWeaveUiState?) : PlannerStateRepository {
        @Volatile
        private var persisted = initial

        override suspend fun load(): DayWeaveUiState? = persisted

        override suspend fun save(state: DayWeaveUiState) {
            persisted = state
        }

        fun snapshot(): DayWeaveUiState? = persisted
    }

    private class ManualCanonicalTrashCleanupScheduler : CanonicalTrashCleanupScheduler {
        private data class ScheduledCleanup(
            val delayMillis: Long,
            val action: () -> Unit,
            var isCancelled: Boolean = false,
        )

        private val cleanups = mutableListOf<ScheduledCleanup>()

        override fun schedule(
            delayMillis: Long,
            action: () -> Unit,
        ): CanonicalTrashCleanupCancellation {
            val cleanup = ScheduledCleanup(delayMillis, action)
            cleanups += cleanup
            return CanonicalTrashCleanupCancellation { cleanup.isCancelled = true }
        }

        fun runNext() {
            val cleanup = cleanups.firstOrNull { !it.isCancelled }
                ?: throw AssertionError("No canonical trash cleanup is scheduled")
            cleanup.isCancelled = true
            cleanup.action()
        }
    }

    private companion object {
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "connection-1"
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val PARENT_ID = "22222222-2222-4222-8222-222222222222"
        const val CHILD_ID = "33333333-3333-4333-8333-333333333333"
        const val DELETED_ITEM_ID = "44444444-4444-4444-8444-444444444444"
        const val MOVER_ID = "55555555-5555-4555-8555-555555555555"
        const val MUTATION_ID = "99999999-9999-4999-8999-999999999990"
        const val RESTORE_MUTATION_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val CHILD_CREATE_MUTATION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val MOVER_REPLACE_MUTATION_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val NOW_MILLIS = 1_788_086_400_000L
        const val NOW = "2026-08-30T10:00:00Z"
    }
}
