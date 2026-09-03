package com.greengolddog.dayweave.ui

import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.OnboardingFirstItemAnchorSnapshot
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorMode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class OnboardingFirstItemEditorRouteTest {
    @Test
    fun pendingCreateReopensItsExactDraftAndCanonicalAnchorReopensReplacement() {
        val draft = plannedDraft()
        val pending = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft,
            createdAt = CREATED_AT,
        )
        val pendingRoute = requireNotNull(
            onboardingFirstItemEditorRoute(
                DayWeaveUiState(
                    onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
                    pendingCanonicalAuthoringMutations = listOf(pending),
                ),
            ),
        )
        assertEquals(CanonicalItemEditorMode.UPDATE_PENDING, pendingRoute.mode)
        assertEquals(MUTATION_ID, pendingRoute.mutationId)
        assertEquals(draft, pendingRoute.initialDraft)

        val item = canonicalItem()
        val canonicalRoute = requireNotNull(
            onboardingFirstItemEditorRoute(
                DayWeaveUiState(
                    onboardingFirstItemAnchor =
                        OnboardingFirstItemAnchorSnapshot(ITEM_ID, item.revision),
                    canonicalItems = listOf(item),
                ),
            ),
        )
        assertEquals(CanonicalItemEditorMode.REPLACE, canonicalRoute.mode)
        assertEquals(item.title, canonicalRoute.initialDraft.title)
    }

    @Test
    fun submittedCreateWaitsForReconciliationAndStaleEvidenceStartsFreshRecovery() {
        val submitted = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = plannedDraft(),
            createdAt = CREATED_AT,
            syncOrigin = "https://example.test/",
            configurationId = CONFIGURATION_ID,
            submittedAt = "2026-09-03T07:01:00Z",
        )
        assertNull(
            onboardingFirstItemEditorRoute(
                DayWeaveUiState(
                    onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
                    pendingCanonicalAuthoringMutations = listOf(submitted),
                ),
            ),
        )

        val item = canonicalItem()
        val child = canonicalItem(id = CHILD_ID, parentId = ITEM_ID)
        val recovery = requireNotNull(
            onboardingFirstItemEditorRoute(
                DayWeaveUiState(
                    onboardingFirstItemAnchor =
                        OnboardingFirstItemAnchorSnapshot(ITEM_ID, item.revision),
                    canonicalItems = listOf(item, child),
                ),
            ),
        )
        assertEquals(CanonicalItemEditorMode.CREATE, recovery.mode)
    }

    private fun plannedDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.TASK,
        title = "First task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
    )

    private fun canonicalItem(
        id: String = ITEM_ID,
        parentId: String? = null,
    ) = CanonicalItemSnapshot(
        id = id,
        kind = "task",
        status = "planned",
        title = "First task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = CREATED_AT,
        updatedAt = "2026-09-03T07:30:00Z",
    )

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val CHILD_ID = "22222222-2222-4222-8222-222222222222"
        const val MUTATION_ID = "33333333-3333-4333-8333-333333333333"
        const val CONFIGURATION_ID = "44444444-4444-4444-8444-444444444444"
        const val CREATED_AT = "2026-09-03T07:00:00Z"
    }
}
