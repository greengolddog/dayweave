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
        assertTrue(
            task.copy(
                constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = false),
            ).createsPlanningDemand(ITEM_ID),
        )
        assertFalse(task.createsPlanningDemand(ITEM_ID, hasChildren = true))
        assertFalse(
            task.copy(
                constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
                hasOwnEffort = true,
            ).createsPlanningDemand(ITEM_ID, hasChildren = true),
        )
        val habit = task.copy(
            kind = ItemKind.HABIT,
            recurrence = CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.DAILY,
                occurrencesPerPeriod = 1,
            ),
        )
        val breakItem = task.copy(kind = ItemKind.BREAK)
        listOf(habit, breakItem).forEach { leaf ->
            assertTrue(leaf.createsPlanningDemand(ITEM_ID))
            assertFalse(leaf.createsPlanningDemand(ITEM_ID, hasChildren = true))
            assertFalse(
                leaf.copy(
                    constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
                    hasOwnEffort = true,
                ).createsPlanningDemand(ITEM_ID, hasChildren = true),
            )
        }
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
            hasOwnEffort = true,
        )
        val routineWithOwnEffort = task.copy(
            kind = ItemKind.ROUTINE,
            constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
            hasOwnEffort = true,
        )
        assertTrue(goalWithOwnEffort.createsPlanningDemand(ITEM_ID))
        assertTrue(routineWithOwnEffort.createsPlanningDemand(ITEM_ID))
        assertFalse(goalWithOwnEffort.createsPlanningDemand(ITEM_ID, hasChildren = true))
        assertFalse(routineWithOwnEffort.createsPlanningDemand(ITEM_ID, hasChildren = true))
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
        assertTrue(event.createsPlanningDemand(ITEM_ID, hasChildren = true))
        assertFalse(
            event.copy(durationSeconds = 1_700).createsPlanningDemand(ITEM_ID),
        )
    }

    @Test
    fun canonicalDemandRequiresFlexibleLeavesButRetainsFixedEventIntervals() {
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
            hasOwnEffort = true,
        )
        val routineWithOwnEffort = item.copy(
            kind = "routine",
            flexibleConstraintsJson = """{"has_own_effort":true}""",
            hasOwnEffort = true,
        )
        assertTrue(goalWithOwnEffort.createsPlanningDemand(listOf(goalWithOwnEffort)))
        assertTrue(routineWithOwnEffort.createsPlanningDemand(listOf(routineWithOwnEffort)))
        assertTrue(
            goalWithOwnEffort.copy(flexibleConstraintsJson = "{}")
                .createsPlanningDemand(listOf(goalWithOwnEffort.copy(flexibleConstraintsJson = "{}"))),
        )

        val child = canonicalItem(
            id = CHILD_ID,
            revision = 1,
            parentId = ITEM_ID,
        )
        val taskParent = item.copy(isExecutable = false)
        assertFalse(taskParent.createsPlanningDemand(listOf(taskParent, child)))
        assertFalse(item.createsPlanningDemand(listOf(item, child)))
        val taskWithOwnEffort = taskParent.copy(
            flexibleConstraintsJson = """{"has_own_effort":true}""",
            hasOwnEffort = true,
        )
        assertFalse(taskWithOwnEffort.createsPlanningDemand(listOf(taskWithOwnEffort, child)))
        val goalParent = goalWithOwnEffort.copy(isExecutable = false)
        val routineParent = routineWithOwnEffort.copy(isExecutable = false)
        assertFalse(goalParent.createsPlanningDemand(listOf(goalParent, child)))
        assertFalse(routineParent.createsPlanningDemand(listOf(routineParent, child)))
        listOf(taskWithOwnEffort, goalParent, routineParent).forEach { parent ->
            val staleExecutableParent = parent.copy(isExecutable = true)
            listOf("planned", "inbox", "blocked", "completed", "cancelled").forEach { childStatus ->
                assertFalse(
                    staleExecutableParent.createsPlanningDemand(
                        listOf(staleExecutableParent, child.copy(status = childStatus)),
                    ),
                )
            }
        }
        val event = canonicalEvent().copy(isExecutable = false)
        assertTrue(event.createsPlanningDemand(listOf(event, child)))
        assertTrue(event.copy(isExecutable = true).createsPlanningDemand(listOf(event, child)))
        assertEquals("2026-09-03T09:00:00Z", event.earliestStartAt)
        assertEquals("2026-09-03T09:30:00Z", event.deadlineAt)
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
                        draft = plannedTask().copy(
                            constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
                            hasOwnEffort = true,
                        ),
                    ),
                    childCreate,
                ),
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
        assertNull(
            canonical.copy(
                canonicalItems = listOf(
                    item.copy(
                        flexibleConstraintsJson = """{"has_own_effort":true}""",
                        hasOwnEffort = true,
                    ),
                ),
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
    fun queuedChildMovesAreAppliedToTheEffectiveHierarchy() {
        val parent = canonicalItem().copy(isExecutable = false)
        val child = canonicalItem(
            id = CHILD_ID,
            parentId = ITEM_ID,
        )
        val anchored = DayWeaveUiState(
            canonicalItems = listOf(parent, child),
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(
                ITEM_ID,
                parent.revision,
            ),
        )
        assertNull(anchored.validatedOnboardingFirstItemCheck())

        val moveAway = pendingReplace(
            item = child,
            draft = child.toCanonicalDraft().copy(parentId = null),
        )
        assertEquals(
            OnboardingFirstItemCheck.CANONICAL_ITEM,
            anchored.copy(
                pendingCanonicalAuthoringMutations = listOf(moveAway),
            ).validatedOnboardingFirstItemCheck(),
        )

        val leafParent = parent.copy(isExecutable = true)
        val rootChild = child.copy(parentId = null)
        val moveIntoParent = pendingReplace(
            item = rootChild,
            draft = rootChild.toCanonicalDraft().copy(parentId = ITEM_ID),
        )
        assertNull(
            anchored.copy(
                canonicalItems = listOf(leafParent, rootChild),
                pendingCanonicalAuthoringMutations = listOf(moveIntoParent),
            ).validatedOnboardingFirstItemCheck(),
        )
        assertNull(
            anchored.copy(
                canonicalItems = listOf(
                    leafParent.copy(
                        flexibleConstraintsJson = """{"has_own_effort":true}""",
                        hasOwnEffort = true,
                    ),
                    rootChild,
                ),
                pendingCanonicalAuthoringMutations = listOf(moveIntoParent),
            ).validatedOnboardingFirstItemCheck(),
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

    @Test
    fun staleExecutableParentCannotUsePublishedOrQueuedHierarchyAsLeafProof() {
        val published = publishedState()
        val parent = published.canonicalItems.single().copy(
            flexibleConstraintsJson = """{"has_own_effort":true}""",
            hasOwnEffort = true,
        )
        val leaf = published.copy(canonicalItems = listOf(parent))
        val block = leaf.schedule.single()
        assertTrue(leaf.hasExactOnboardingFirstPlanProof())
        assertTrue(leaf.hasPublishedExecutionAuthority(block))

        val child = canonicalItem(id = CHILD_ID, parentId = ITEM_ID)
        val staleParent = leaf.copy(canonicalItems = listOf(parent, child))
        assertFalse(staleParent.hasExactOnboardingFirstPlanProof())
        assertFalse(staleParent.hasPublishedExecutionAuthority(block))
        assertTrue(staleParent.hasValidOnboardingFirstItemAnchorRelationship())

        val pendingMoveAway = staleParent.copy(
            canonicalItems = listOf(parent.copy(isExecutable = false), child),
            pendingCanonicalAuthoringMutations = listOf(pendingReplace(
                item = child,
                draft = child.toCanonicalDraft().copy(parentId = null),
            )),
        )
        assertEquals(
            OnboardingFirstItemCheck.CANONICAL_ITEM,
            pendingMoveAway.validatedOnboardingFirstItemCheck(),
        )
        assertFalse(pendingMoveAway.hasExactOnboardingFirstPlanProof())
        assertFalse(pendingMoveAway.hasPublishedExecutionAuthority(block))

        val queuedChild = pendingCreate(
            itemId = CHILD_ID,
            mutationId = CHILD_MUTATION_ID,
            draft = plannedTask().copy(parentId = ITEM_ID),
        )
        val queuedParent = leaf.copy(pendingCanonicalAuthoringMutations = listOf(queuedChild))
        assertFalse(queuedParent.hasExactOnboardingFirstPlanProof())
        assertFalse(queuedParent.hasPublishedExecutionAuthority(block))
        assertTrue(queuedParent.hasValidOnboardingFirstItemAnchorRelationship())
    }

    @Test
    fun localDesignationSurvivesChildAdditionWithoutRemainingLiveDemandProof() {
        val parentCreate = pendingCreate(draft = plannedTask().copy(
            kind = ItemKind.GOAL,
            constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
            hasOwnEffort = true,
        ))
        val childCreate = pendingCreate(
            itemId = CHILD_ID,
            mutationId = CHILD_MUTATION_ID,
            draft = plannedTask().copy(
                parentId = ITEM_ID,
                placement = CanonicalDraftPlacement.INBOX,
            ),
        )
        val parent = DayWeaveUiState(
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
            pendingCanonicalAuthoringMutations = listOf(parentCreate, childCreate),
        )
        assertTrue(parent.hasValidOnboardingFirstItemAnchorRelationship())
        assertNull(parent.validatedOnboardingFirstItemCheck())
        assertEquals(true, parentCreate.draft?.constraints?.hasOwnEffort)
        val event = canonicalEvent().toCanonicalDraft()
        assertEquals(
            OnboardingFirstItemCheck.PENDING_CREATE,
            parent.copy(pendingCanonicalAuthoringMutations = listOf(
                parentCreate.copy(draft = event), childCreate,
            )).validatedOnboardingFirstItemCheck(),
        )
    }

    @Test
    fun fixedEventParentKeepsPublishedIntervalProofWithoutGainingExecutionAuthority() {
        val base = publishedState()
        val event = canonicalEvent()
        val block = base.schedule.single().copy(
            kind = ItemKind.EVENT,
            canonicalBlockKind = "calendar_event",
            isFlexible = false,
            isHardConstraint = true,
        )
        val proof = requireNotNull(base.publishedScheduleProof).copy(
            blocks = listOf(PublishedScheduleBlockProofSnapshot.from(block)),
        )
        val leaf = base.copy(
            canonicalItems = listOf(event),
            schedule = listOf(block),
            publishedScheduleProof = proof,
        )
        assertTrue(leaf.hasExactOnboardingFirstPlanProof())
        assertTrue(leaf.hasPublishedExecutionAuthority(block))

        val child = canonicalItem(id = CHILD_ID, parentId = ITEM_ID)
        val parent = leaf.copy(canonicalItems = listOf(event.copy(isExecutable = false), child))
        assertTrue(parent.hasExactOnboardingFirstPlanProof())
        assertFalse(parent.hasPublishedExecutionAuthority(block))
        assertEquals(block.absoluteStartAt, parent.schedule.single().absoluteStartAt)
        assertEquals(block.absoluteEndAt, parent.schedule.single().absoluteEndAt)
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
            publishedScheduleRevisionHint = PublishedScheduleRevisionHintSnapshot(
                syncOrigin = ORIGIN,
                configurationId = CONFIGURATION_ID,
                revisionNumber = revision.revisionNumber,
            ),
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

    private fun pendingReplace(
        item: CanonicalItemSnapshot,
        draft: CanonicalItemDraft,
    ): PendingCanonicalAuthoringMutation = PendingCanonicalAuthoringMutation(
        id = CHILD_REPLACE_MUTATION_ID,
        itemId = item.id,
        operation = CanonicalAuthoringOperation.REPLACE,
        draft = draft,
        expectedRevision = item.revision,
        baseItem = item,
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

    private fun canonicalEvent(): CanonicalItemSnapshot {
        val timing = CanonicalEventTimingDraft(
            startsAt = "2026-09-03T09:00:00Z",
            endsAt = "2026-09-03T09:30:00Z",
        )
        return canonicalItem().copy(
            kind = "event",
            earliestStartAt = timing.startsAt,
            deadlineAt = timing.endsAt,
            flexibleConstraintsJson =
                """{"dayweave_firm_block":${timing.toCanonicalJson("UTC")}}""",
        )
    }

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val CHILD_ID = "22222222-2222-4222-8222-222222222222"
        const val MUTATION_ID = "33333333-3333-4333-8333-333333333333"
        const val CHILD_MUTATION_ID = "44444444-4444-4444-8444-444444444444"
        const val CHILD_REPLACE_MUTATION_ID = "77777777-7777-4777-8777-777777777777"
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
