package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.OnboardingFirstItemAnchorSnapshot
import com.greengolddog.dayweave.model.OnboardingFirstItemCheck
import com.greengolddog.dayweave.model.validatedOnboardingFirstItemCheck
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class OnboardingFirstItemStoreTest {
    @Test
    fun reviewedCreateAndAnchorCommitAtomicallyAndSurviveRestart() = runBlocking {
        val repository = MemoryRepository(boundState())
        var scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        var store = restoredStore(repository, scope)

        val transition = requireNotNull(
            store.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
        )
        assertTrue(withTimeout(3_000) { transition.persistence.awaitDurable() })
        assertEquals(
            OnboardingFirstItemCheck.PENDING_CREATE,
            repository.snapshot().validatedOnboardingFirstItemCheck(),
        )
        scope.cancel()

        scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            store = restoredStore(repository, scope)
            assertEquals(
                OnboardingFirstItemAnchorSnapshot(ITEM_ID),
                store.durableState.value?.onboardingFirstItemAnchor,
            )
            assertEquals(MUTATION_ID, store.canonicalAuthoringMutation(MUTATION_ID)?.id)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun failedSaveNeverCreatesDurableOnboardingEvidence() = runBlocking {
        val repository = MemoryRepository(boundState())
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = restoredStore(repository, scope)
            repository.failSaves = true

            val transition = requireNotNull(
                store.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
            )
            assertFalse(withTimeout(3_000) { transition.persistence.awaitDurable() })
            assertNull(store.durableState.value?.onboardingFirstItemAnchor)
            assertNull(repository.snapshot().onboardingFirstItemAnchor)
            assertEquals(
                PlannerLoadState.PERSISTENCE_FAILED,
                withTimeout(3_000) {
                    store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED }
                },
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun discardClearsPendingAnchorAndExactResponsePromotesItToServerRevision() {
        val discardStore = PlannerStore(boundState())
        requireNotNull(
            discardStore.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
        )
        assertTrue(discardStore.discardCanonicalAuthoringMutation(MUTATION_ID) != null)
        assertNull(discardStore.state.value.onboardingFirstItemAnchor)

        val store = PlannerStore(boundState())
        val queued = requireNotNull(
            store.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
        ).mutation
        requireNotNull(store.bindCanonicalAuthoringMutation(queued.id, ORIGIN, CONFIGURATION_ID))
        val submitted = requireNotNull(store.markCanonicalAuthoringSubmitted(queued.id)).mutation
        val response = canonicalItem()
        requireNotNull(store.applyCanonicalAuthoringResponse(submitted, response))

        assertEquals(
            OnboardingFirstItemAnchorSnapshot(ITEM_ID, response.revision),
            store.state.value.onboardingFirstItemAnchor,
        )
        assertEquals(
            OnboardingFirstItemCheck.CANONICAL_ITEM,
            store.state.value.validatedOnboardingFirstItemCheck(),
        )

        val deletedAt = "2026-09-03T08:00:00Z"
        requireNotNull(
            store.recordCanonicalRecentlyDeleted(
                CanonicalRecentlyDeletedRecord(
                    id = ITEM_ID,
                    revision = 2,
                    deletedAt = deletedAt,
                    lastKnownItem = response.copy(
                        revision = 2,
                        isExecutable = false,
                        updatedAt = deletedAt,
                        deletedAt = deletedAt,
                    ),
                    retentionAnchorAt = deletedAt,
                ),
            ),
        )
        assertNull(store.state.value.onboardingFirstItemAnchor)
    }

    @Test
    fun accountAbandonmentPreservesOnlyAQualifyingUnboundCreateAnchor() {
        val pendingStore = PlannerStore(boundState())
        requireNotNull(
            pendingStore.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
        )
        requireNotNull(pendingStore.abandonCanonicalConnection())
        assertEquals(
            OnboardingFirstItemAnchorSnapshot(ITEM_ID),
            pendingStore.state.value.onboardingFirstItemAnchor,
        )

        val item = canonicalItem()
        val canonicalStore = PlannerStore(
            boundState().copy(
                canonicalItems = listOf(item),
                onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(
                    ITEM_ID,
                    item.revision,
                ),
            ),
        )
        requireNotNull(canonicalStore.abandonCanonicalConnection())
        assertNull(canonicalStore.state.value.onboardingFirstItemAnchor)
    }

    @Test
    fun authoritativeRefreshPromotesExactLocalCreateButClearsUnreviewedRevision() {
        val pendingStore = PlannerStore(boundState())
        requireNotNull(
            pendingStore.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
        )
        requireNotNull(
            pendingStore.replaceCanonicalPlan(canonicalUpdate(listOf(canonicalItem()))),
        )
        assertEquals(
            OnboardingFirstItemAnchorSnapshot(ITEM_ID, 1),
            pendingStore.state.value.onboardingFirstItemAnchor,
        )

        val original = canonicalItem()
        val revisionStore = PlannerStore(
            boundState().copy(
                canonicalItems = listOf(original),
                onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID, 1),
            ),
        )
        requireNotNull(
            revisionStore.replaceCanonicalPlan(
                canonicalUpdate(
                    listOf(
                        original.copy(
                            revision = 2,
                            updatedAt = "2026-09-03T08:00:00Z",
                        ),
                    ),
                ),
            ),
        )
        assertNull(revisionStore.state.value.onboardingFirstItemAnchor)
    }

    @Test
    fun reviewedTaskWithQueuedChildMustRetainExplicitIndependentEffort() {
        val store = PlannerStore(boundState())
        requireNotNull(
            store.enqueueOnboardingFirstItemCreate(plannedDraft(), ITEM_ID, MUTATION_ID),
        )
        requireNotNull(
            store.enqueueCanonicalCreate(
                plannedDraft().copy(parentId = ITEM_ID),
                CHILD_ID,
                CHILD_MUTATION_ID,
            ),
        )
        assertNull(store.state.value.validatedOnboardingFirstItemCheck())

        assertThrows(IllegalArgumentException::class.java) {
            store.updateCanonicalAuthoringDraft(
                MUTATION_ID,
                plannedDraft().copy(title = "Still only rolled-up effort"),
            )
        }
        assertEquals(
            "First task",
            store.canonicalAuthoringMutation(MUTATION_ID)?.draft?.title,
        )

        requireNotNull(
            store.updateCanonicalAuthoringDraft(
                MUTATION_ID,
                plannedDraft().copy(
                    title = "Independent parent work",
                    constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
                ),
            ),
        )
        assertEquals(
            OnboardingFirstItemCheck.PENDING_CREATE,
            store.state.value.validatedOnboardingFirstItemCheck(),
        )
    }

    private suspend fun restoredStore(
        repository: PlannerStateRepository,
        scope: CoroutineScope,
    ): PlannerStore = PlannerStore(
        initialState = DayWeaveUiState(),
        repository = repository,
        scope = scope,
        nowEpochMillis = { NOW_MILLIS },
    ).also { store ->
        withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
    }

    private fun plannedDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.TASK,
        title = "First task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
    )

    private fun canonicalItem() = CanonicalItemSnapshot(
        id = ITEM_ID,
        kind = "task",
        status = "planned",
        title = "First task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = CREATED_AT,
        updatedAt = "2026-09-03T07:30:00Z",
    )

    private fun boundState() = DayWeaveUiState(
        canonicalSyncOrigin = ORIGIN,
        canonicalConfigurationId = CONFIGURATION_ID,
        canonicalDeltaCursor = "cursor-1",
    )

    private fun canonicalUpdate(items: List<CanonicalItemSnapshot>) = CanonicalPlanUpdate(
        items = items,
        schedule = emptyList(),
        syncOrigin = ORIGIN,
        configurationId = CONFIGURATION_ID,
        deltaCursor = "cursor-2",
        inputDigest = PLAN_DIGEST,
        generatedAt = "2026-09-03T08:00:00Z",
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

    private class MemoryRepository(initial: DayWeaveUiState) : PlannerStateRepository {
        @Volatile
        private var persisted = initial

        @Volatile
        var failSaves = false

        override suspend fun load(): DayWeaveUiState = persisted

        override suspend fun save(state: DayWeaveUiState) {
            if (failSaves) throw IllegalStateException("synthetic save failure")
            persisted = state
        }

        fun snapshot(): DayWeaveUiState = persisted
    }

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val MUTATION_ID = "22222222-2222-4222-8222-222222222222"
        const val CHILD_ID = "44444444-4444-4444-8444-444444444444"
        const val CHILD_MUTATION_ID = "55555555-5555-4555-8555-555555555555"
        const val ORIGIN = "https://example.test/"
        const val CONFIGURATION_ID = "33333333-3333-4333-8333-333333333333"
        const val CREATED_AT = "2026-09-03T07:00:00Z"
        const val NOW_MILLIS = 1_778_000_000_000L
        const val PLAN_DIGEST =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
}
