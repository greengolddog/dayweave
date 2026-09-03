package com.greengolddog.dayweave.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class OnboardingReadinessProofTest {
    @Test
    fun planningDemandIsStrictAndFailsClosed() {
        val task = plannedTask()
        assertTrue(task.createsPlanningDemand(ITEM_ID))
        assertFalse(
            task.copy(placement = CanonicalDraftPlacement.INBOX)
                .createsPlanningDemand(ITEM_ID),
        )
        assertFalse(task.copy(durationSeconds = null).createsPlanningDemand(ITEM_ID))
        assertFalse(task.copy(kind = ItemKind.GOAL).createsPlanningDemand(ITEM_ID))
        assertFalse(task.copy(kind = ItemKind.ROUTINE).createsPlanningDemand(ITEM_ID))
        val goalWithOwnEffort = task.copy(
            kind = ItemKind.GOAL,
            constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
        )
        val routineWithOwnEffort = task.copy(
            kind = ItemKind.ROUTINE,
            constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
        )
        assertTrue(goalWithOwnEffort.createsPlanningDemand(ITEM_ID))
        assertTrue(routineWithOwnEffort.createsPlanningDemand(ITEM_ID))
        assertFalse(
            goalWithOwnEffort.copy(durationSeconds = null).createsPlanningDemand(ITEM_ID),
        )
        assertFalse(task.copy(title = " ").createsPlanningDemand(ITEM_ID))

        val event = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.EVENT,
            title = "First event",
            timezoneName = "UTC",
            durationSeconds = 1_800,
            deadlineAt = "2026-09-03T09:30:00Z",
            earliestStartAt = "2026-09-03T09:00:00Z",
            eventTiming = CanonicalEventTimingDraft(
                startsAt = "2026-09-03T09:00:00Z",
                endsAt = "2026-09-03T09:30:00Z",
            ),
        )
        assertTrue(event.createsPlanningDemand(ITEM_ID))
        assertFalse(
            event.copy(durationSeconds = 1_700).createsPlanningDemand(ITEM_ID),
        )
    }

    @Test
    fun canonicalDemandRequiresExactActiveLeaf() {
        val item = canonicalItem()
        assertTrue(item.createsPlanningDemand(listOf(item)))
        assertTrue(
            item.copy(status = "scheduled").createsPlanningDemand(
                listOf(item.copy(status = "scheduled")),
            ),
        )
        assertFalse(item.copy(isExecutable = false).createsPlanningDemand(listOf(item)))
        assertFalse(item.copy(deletedAt = UPDATED_AT).createsPlanningDemand(listOf(item)))
        val goalWithOwnEffort = item.copy(
            kind = "goal",
            flexibleConstraintsJson = """{"has_own_effort":true}""",
        )
        val routineWithOwnEffort = item.copy(
            kind = "routine",
            flexibleConstraintsJson = """{"has_own_effort":true}""",
        )
        assertTrue(goalWithOwnEffort.createsPlanningDemand(listOf(goalWithOwnEffort)))
        assertTrue(routineWithOwnEffort.createsPlanningDemand(listOf(routineWithOwnEffort)))
        assertFalse(
            goalWithOwnEffort.copy(flexibleConstraintsJson = "{}")
                .createsPlanningDemand(listOf(goalWithOwnEffort.copy(flexibleConstraintsJson = "{}"))),
        )

        val child = canonicalItem(
            id = CHILD_ID,
            revision = 1,
            parentId = ITEM_ID,
        )
        assertFalse(item.createsPlanningDemand(listOf(item, child)))
    }

    @Test
    fun pendingAndCanonicalChecksRequireTheirExactEvidence() {
        val create = pendingCreate()
        val pending = DayWeaveUiState(
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
            pendingCanonicalAuthoringMutations = listOf(create),
        )
        assertTrue(pending.hasValidOnboardingFirstItemAnchorRelationship())
        assertEquals(
            OnboardingFirstItemCheck.PENDING_CREATE,
            pending.validatedOnboardingFirstItemCheck(),
        )

        val childCreate = pendingCreate(
            itemId = CHILD_ID,
            mutationId = CHILD_MUTATION_ID,
            draft = plannedTask().copy(parentId = ITEM_ID),
        )
        assertNull(
            pending.copy(
                pendingCanonicalAuthoringMutations = listOf(create, childCreate),
            ).validatedOnboardingFirstItemCheck(),
        )
        assertNull(
            pending.copy(
                pendingCanonicalAuthoringMutations = listOf(
                    create.copy(
                        disposition = CanonicalAuthoringDisposition.CONFLICTED,
                        diagnostic = "Review this retained create",
                    ),
                ),
            ).validatedOnboardingFirstItemCheck(),
        )

        val item = canonicalItem()
        val canonical = DayWeaveUiState(
            canonicalItems = listOf(item),
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(
                ITEM_ID,
                item.revision,
            ),
        )
        assertTrue(canonical.hasValidOnboardingFirstItemAnchorRelationship())
        assertEquals(
            OnboardingFirstItemCheck.CANONICAL_ITEM,
            canonical.validatedOnboardingFirstItemCheck(),
        )
        assertNull(
            canonical.copy(
                pendingCanonicalAuthoringMutations = listOf(childCreate),
            ).validatedOnboardingFirstItemCheck(),
        )
        assertTrue(
            canonical.copy(
                pendingCanonicalAuthoringMutations = listOf(childCreate),
            ).hasValidOnboardingFirstItemAnchorRelationship(),
        )
        assertFalse(
            canonical.copy(
                onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(
                    ITEM_ID,
                    item.revision + 1,
                ),
            ).hasValidOnboardingFirstItemAnchorRelationship(),
        )
    }

    @Test
    fun reconciliationPromotesOnlyAnExactReviewedGeneration() {
        val anchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID)
        val create = pendingCreate()
        val matching = canonicalItem()
        assertEquals(
            OnboardingFirstItemAnchorSnapshot(ITEM_ID, matching.revision),
            reconciledOnboardingFirstItemAnchor(
                anchor = anchor,
                canonicalItems = listOf(matching),
                pendingAuthoringMutations = listOf(create),
                recentlyDeleted = emptyList(),
            ),
        )

        val unrelated = matching.copy(title = "Unreviewed same-id content")
        assertEquals(
            anchor,
            reconciledOnboardingFirstItemAnchor(
                anchor = anchor,
                canonicalItems = listOf(unrelated),
                pendingAuthoringMutations = listOf(create),
                recentlyDeleted = emptyList(),
            ),
        )

        assertNull(
            reconciledOnboardingFirstItemAnchor(
                anchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID, 1),
                canonicalItems = listOf(matching.copy(revision = 2)),
                pendingAuthoringMutations = emptyList(),
                recentlyDeleted = emptyList(),
            ),
        )
        assertNull(
            reconciledOnboardingFirstItemAnchor(
                anchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID, 1),
                canonicalItems = emptyList(),
                pendingAuthoringMutations = emptyList(),
                recentlyDeleted = listOf(
                    CanonicalRecentlyDeletedRecord(
                        id = ITEM_ID,
                        revision = 2,
                        deletedAt = UPDATED_AT,
                        retentionAnchorAt = UPDATED_AT,
                    ),
                ),
                authoritativeMissing = true,
            ),
        )
    }

    @Test
    fun accountResetPreservesOnlyAnExactUnboundCreateAnchor() {
        val create = pendingCreate()
        assertEquals(
            OnboardingFirstItemAnchorSnapshot(ITEM_ID),
            reconciledOnboardingFirstItemAnchor(
                anchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
                canonicalItems = emptyList(),
                pendingAuthoringMutations = listOf(create),
                recentlyDeleted = emptyList(),
                authoritativeMissing = true,
            ),
        )
        assertNull(
            reconciledOnboardingFirstItemAnchor(
                anchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID, 7),
                canonicalItems = emptyList(),
                pendingAuthoringMutations = emptyList(),
                recentlyDeleted = emptyList(),
                authoritativeMissing = true,
            ),
        )
    }

    @Test
    fun firstPlanRequiresTheWholeCurrentPlanAndExactAnchoredRevision() {
        val state = publishedState()
        assertTrue(state.hasExactOnboardingFirstPlanProof())

        assertFalse(
            state.copy(
                onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID, 2),
            ).hasExactOnboardingFirstPlanProof(),
        )
        assertFalse(
            state.copy(
                publishedScheduleProof = requireNotNull(state.publishedScheduleProof).copy(
                    blocks = emptyList(),
                ),
            ).hasExactOnboardingFirstPlanProof(),
        )
        assertFalse(
            state.copy(scheduleInputDigest = "sha256:${"b".repeat(64)}")
                .hasExactOnboardingFirstPlanProof(),
        )
    }

    private fun publishedState(): DayWeaveUiState {
        val item = canonicalItem()
        val block = ScheduleItem(
            id = BLOCK_ID,
            title = item.title,
            kind = ItemKind.TASK,
            startMinute = 9 * 60,
            durationMinutes = 30,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = item.id,
            canonicalRevision = item.revision,
            sessionIndex = 0,
            absoluteStartAt = "2026-09-03T09:00:00Z",
            absoluteEndAt = "2026-09-03T09:30:00Z",
            planningZoneId = "UTC",
            canonicalBlockKind = "planned",
        )
        val revision = PublishedScheduleRevisionSnapshot(
            id = REVISION_ID,
            revision = "1:$REVISION_ID",
            revisionNumber = 1uL,
            inputDigest = DIGEST,
            horizonStart = "2026-09-03T00:00:00Z",
            horizonEnd = "2026-09-10T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = "2026-09-03T08:00:00Z",
        )
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            revision = revision,
            asOf = "2026-09-03T08:00:00Z",
            blocks = listOf(PublishedScheduleBlockProofSnapshot.from(block)),
        )
        return DayWeaveUiState(
            schedule = listOf(block),
            canonicalItems = listOf(item),
            canonicalSyncOrigin = ORIGIN,
            canonicalConfigurationId = CONFIGURATION_ID,
            publishedScheduleRevision = revision,
            publishedScheduleProof = proof,
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(
                ITEM_ID,
                item.revision,
            ),
            scheduleInputDigest = DIGEST,
            scheduleGeneratedAt = proof.asOf,
            schedulePlanningZoneId = "UTC",
        )
    }

    private fun plannedTask(): CanonicalItemDraft = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.TASK,
        title = "First planned task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
    )

    private fun pendingCreate(
        itemId: String = ITEM_ID,
        mutationId: String = MUTATION_ID,
        draft: CanonicalItemDraft = plannedTask(),
    ): PendingCanonicalAuthoringMutation = PendingCanonicalAuthoringMutation(
        id = mutationId,
        itemId = itemId,
        operation = CanonicalAuthoringOperation.CREATE,
        draft = draft,
        createdAt = CREATED_AT,
    )

    private fun canonicalItem(
        id: String = ITEM_ID,
        revision: Long = 1,
        parentId: String? = null,
    ): CanonicalItemSnapshot = CanonicalItemSnapshot(
        id = id,
        kind = "task",
        status = "planned",
        title = "First planned task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = true,
        revision = revision,
        createdAt = CREATED_AT,
        updatedAt = UPDATED_AT,
    )

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val CHILD_ID = "22222222-2222-4222-8222-222222222222"
        const val MUTATION_ID = "33333333-3333-4333-8333-333333333333"
        const val CHILD_MUTATION_ID = "44444444-4444-4444-8444-444444444444"
        const val BLOCK_ID = "55555555-5555-4555-8555-555555555555"
        const val REVISION_ID = "66666666-6666-4666-8666-666666666666"
        const val CREATED_AT = "2026-09-03T07:00:00Z"
        const val UPDATED_AT = "2026-09-03T07:30:00Z"
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "configuration-1"
        const val DIGEST =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
}
