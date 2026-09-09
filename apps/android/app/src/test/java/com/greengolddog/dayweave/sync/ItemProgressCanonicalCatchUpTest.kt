package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.state.PlannerStore
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Test

class ItemProgressCanonicalCatchUpTest {
    @Test fun deltaOnlyCatchUpNeedsNoPublishedScheduleAndRetainsOutboxExecutionAndHighWater() = runBlocking {
        val pending = progressTestMutation(submitted = true)
        val original = initial().copy(
            itemProgressLedger = progressTestLedger().copy(pending = listOf(pending)),
            activeSession = ActiveSession(itemId = "synthetic-local-timer", elapsedMinutes = 3, isPaused = false),
            publishedScheduleRevisionHint = PublishedScheduleRevisionHintSnapshot(PROGRESS_ORIGIN, PROGRESS_CONFIGURATION, 12uL),
            scheduleInputDigest = "sha256:" + "a".repeat(64),
        )
        val store = PlannerStore(original)
        val before = store.state.value
        val future = remote().copy(id = PROGRESS_COMPONENT, kind = "future_read_only", status = "future_status")
        val transport = DeltaOnlyTransport { page(remote(), future) }
        val manager = CanonicalSyncManager(store, GenerationBoundCredentialStore(), transport)
        assertTrue(manager.refreshItemProgressCanonicalEvidence { true })
        assertEquals(1, transport.deltaCalls)
        assertEquals(8L, store.state.value.progressItem(PROGRESS_ITEM)?.revision)
        assertEquals("future_read_only", store.state.value.canonicalItems.single { it.id == PROGRESS_COMPONENT }.kind)
        assertEquals(before.itemProgressLedger, store.state.value.itemProgressLedger)
        assertEquals(before.activeSession, store.state.value.activeSession)
        assertEquals(before.schedule, store.state.value.schedule)
        assertEquals(before.publishedScheduleRevisionHint, store.state.value.publishedScheduleRevisionHint)
        assertNull(store.state.value.scheduleInputDigest)
        assertNull(store.state.value.publishedScheduleProof)
        assertEquals("synthetic-caught-up", store.state.value.canonicalDeltaCursor)
    }

    @Test fun pendingCanonicalAuthoringBlocksReadOnlyCatchUpWithoutTouchingExactIntent() = runBlocking {
        val original = initial().copy(pendingCanonicalAuthoringMutations = listOf(PendingCanonicalAuthoringMutation(
            id = PROGRESS_OPERATION, itemId = PROGRESS_COMPONENT, operation = CanonicalAuthoringOperation.CREATE,
            draft = CanonicalItemDraft(title = "Synthetic draft", timezoneName = "UTC"), createdAt = PROGRESS_NOW)))
        val store = PlannerStore(original)
        val transport = DeltaOnlyTransport { error("A pending canonical mutation must block the read") }
        assertFalse(CanonicalSyncManager(store, GenerationBoundCredentialStore(), transport)
            .refreshItemProgressCanonicalEvidence { true })
        assertEquals(0, transport.deltaCalls)
        assertEquals(original, store.state.value)
    }

    @Test fun stoppedLifetimeOrEqualRevisionForkNeverInstallsReceivedCanonicalEvidence() = runBlocking {
        for (fork in listOf(false, true)) {
            val store = PlannerStore(initial())
            val before = store.state.value
            var visible = true
            val transport = DeltaOnlyTransport {
                if (!fork) visible = false
                page(remote().copy(revision = if (fork) 7 else 8, title = "Synthetic changed title"))
            }
            assertFalse(CanonicalSyncManager(store, GenerationBoundCredentialStore(), transport)
                .refreshItemProgressCanonicalEvidence { visible })
            assertEquals(before, store.state.value)
        }
    }

    private fun initial() = progressTestState().copy(canonicalItems = listOf(progressTestItem().copy(
        kind = "goal", status = "planned", hasOwnEffort = false, splitPolicyJson = "{\"type\":\"indivisible\"}")))

    private fun remote() = RemoteCanonicalItem(id = PROGRESS_ITEM, isSensitive = false, kind = "goal", status = "planned",
        title = "Synthetic progress item", timezoneName = "UTC", durationKind = CanonicalDurationKind.UNKNOWN,
        deadlineKind = CanonicalDeadlineKind.NONE, hasOwnEffort = false,
        flexibleConstraints = buildJsonObject {}, splitPolicy = buildJsonObject { put("type", "indivisible") },
        importance = 1, urgency = 1, siblingOrder = 0, isExecutable = false, revision = 8,
        createdAt = PROGRESS_NOW, updatedAt = PROGRESS_NOW)

    private fun page(vararg items: RemoteCanonicalItem) = RemoteItemDeltaPage(
        items.map { RemoteItemDeltaChange("upsert", item = it) }, "synthetic-caught-up", false)

    /** Any schedule/provider/authoring request is a test failure, never an actual network call. */
    private class DeltaOnlyTransport(val delta: suspend () -> RemoteItemDeltaPage) : CanonicalPlannerTransport {
        var deltaCalls = 0
        override suspend fun itemDelta(configuration: AuthenticatedApiConfiguration, cursor: String?): RemoteItemDeltaPage {
            deltaCalls++
            return delta()
        }
        override suspend fun currentSchedule(configuration: AuthenticatedApiConfiguration): RemoteCurrentPublishedSchedule? = error("No schedule read")
        override suspend fun preview(configuration: AuthenticatedApiConfiguration, request: SchedulePreviewRequest): RemoteSchedulePreview = error("No compose")
        override suspend fun publish(configuration: AuthenticatedApiConfiguration, request: SchedulePublishHttpRequest): RemoteSchedulePublishResponse = error("No publish")
        override suspend fun createItem(configuration: AuthenticatedApiConfiguration, idempotencyKey: String, request: CreateCanonicalItemRequest): RemoteCanonicalItem = error("No authoring")
        override suspend fun replaceItem(configuration: AuthenticatedApiConfiguration, id: String, idempotencyKey: String, request: ReplaceCanonicalItemRequest): RemoteCanonicalItem = error("No authoring")
        override suspend fun trashItem(configuration: AuthenticatedApiConfiguration, id: String, idempotencyKey: String, expectedRevision: Long): RemoteCanonicalItem = error("No authoring")
        override suspend fun restoreItem(configuration: AuthenticatedApiConfiguration, id: String, idempotencyKey: String, request: CanonicalItemRevisionRequest): RemoteCanonicalItem = error("No authoring")
    }
}
