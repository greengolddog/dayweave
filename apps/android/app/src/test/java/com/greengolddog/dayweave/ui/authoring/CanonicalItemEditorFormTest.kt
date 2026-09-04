package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalBreakCategory
import com.greengolddog.dayweave.model.CanonicalBufferPolicyDraft
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalConstraintLevel
import com.greengolddog.dayweave.model.CanonicalConstraintStrengthDraft
import com.greengolddog.dayweave.model.CanonicalDependencyDraft
import com.greengolddog.dayweave.model.CanonicalDependencyRelation
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalEventTimingDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecurrenceDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceKind
import com.greengolddog.dayweave.model.CanonicalRecurrencePeriod
import com.greengolddog.dayweave.model.CanonicalRecurrenceSemantics
import com.greengolddog.dayweave.model.CanonicalSchedulingConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalSplitDraft
import com.greengolddog.dayweave.model.CanonicalSplitKind
import com.greengolddog.dayweave.model.CanonicalWeekday
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalItemEditorFormTest {
    @Test
    fun formRoundTripsTheTypedDraftContractWithoutJsonEditing() {
        val draft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.ROUTINE,
            isSensitive = true,
            title = "Deep work block",
            notes = "Protect the first hour.",
            timezoneName = "UTC",
            durationSeconds = 3_600,
            earliestStartAt = "2026-08-31T08:00:00Z",
            deadlineAt = "2026-08-31T18:00:00Z",
            recurrence = CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.WEEKLY,
                occurrencesPerPeriod = 2,
                weekdays = listOf(CanonicalWeekday.MONDAY, CanonicalWeekday.WEDNESDAY),
            ),
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.DEEP,
                tags = listOf("focus", "work"),
                preferredStartMinute = 9 * 60,
                minimumGapMinutes = 15,
                maximumSessions = 2,
            ),
            split = CanonicalSplitDraft(
                kind = CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = 900,
                maximumChunkSeconds = 1_800,
            ),
            importance = 85,
            urgency = 65,
            parentId = PARENT_ID,
            siblingOrder = 7,
        )
        draft.requireValid(ITEM_ID)

        val rebuilt = CanonicalItemEditorForm.from(draft).draft(ITEM_ID).getOrThrow()

        assertEquals(draft.normalized(), rebuilt)
    }

    @Test
    fun inboxEventMayStayIncompleteButPlannedEventRequiresExactBounds() {
        val initial = newCanonicalDetailedDraft(
            title = "Appointment",
            kind = ItemKind.EVENT,
        )
        val blankForm = CanonicalItemEditorForm.from(initial)

        assertNull(blankForm.validationIssue(ITEM_ID))
        assertNotNull(
            blankForm.copy(placement = CanonicalDraftPlacement.PLANNED)
                .validationIssue(ITEM_ID),
        )
        assertEquals(null, initial.eventTiming)
        assertEquals(null, initial.earliestStartAt)
        assertEquals(null, initial.deadlineAt)

        val result = blankForm.copy(
            eventStart = "2026-08-31T09:00:00Z",
            eventEnd = "2026-08-31T10:30:00Z",
        ).draft(ITEM_ID).getOrThrow()

        assertEquals("2026-08-31T09:00:00Z", result.eventTiming?.startsAt)
        assertEquals("2026-08-31T10:30:00Z", result.eventTiming?.endsAt)
        assertEquals(5_400L, result.durationSeconds)
        assertEquals(result.eventTiming?.startsAt, result.earliestStartAt)
        assertEquals(result.eventTiming?.endsAt, result.deadlineAt)
    }

    @Test
    fun detailedHabitStartsWithReviewableTypedDailyRecurrence() {
        val draft = newCanonicalDetailedDraft("Morning walk", ItemKind.HABIT)

        assertNull(CanonicalItemEditorForm.from(draft).validationIssue(ITEM_ID))
        assertEquals(CanonicalRecurrenceKind.DAILY, draft.recurrence?.kind)
        assertEquals(1, draft.recurrence?.occurrencesPerPeriod)
    }

    @Test
    fun richControlsProduceTypedFrequencyConstraintsAndSplitExtensions() {
        val form = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Write chapter", ItemKind.TASK),
        ).copy(
            placement = CanonicalDraftPlacement.PLANNED,
            hasDuration = true,
            durationSeconds = "7200",
            recurrenceKind = CanonicalRecurrenceKind.FREQUENCY,
            recurrenceCount = "3",
            recurrencePeriod = CanonicalRecurrencePeriod.WEEK,
            recurrenceSemantics = CanonicalRecurrenceSemantics.CALENDAR,
            recurrenceMinimumSpacingMinutes = "1440",
            weekdays = setOf(
                CanonicalWeekday.MONDAY,
                CanonicalWeekday.WEDNESDAY,
                CanonicalWeekday.FRIDAY,
            ),
            energy = EnergyLevel.DEEP,
            energyStrength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
            tags = listOf("focus", "writing"),
            schedulingSpecified = true,
            constraintEarliest = CanonicalInstantConstraintForm(
                value = "2026-09-03T08:00:00+02:00",
                strength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
            ),
            constraintLatest = CanonicalInstantConstraintForm(
                value = "2026-09-30T18:00:00+02:00",
                strength = CanonicalStrengthForm(CanonicalConstraintLevel.SOFT, "250"),
            ),
            allowedWeekdays = CanonicalWeekday.entries.take(5).toSet(),
            allowedWeekdaysStrength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
            preferredDailyWindows = listOf(
                CanonicalDailyWindowForm(
                    weekdays = setOf(CanonicalWeekday.MONDAY),
                    startMinute = "540",
                    endMinute = "720",
                ),
            ),
            requiredContexts = listOf(
                CanonicalStringConstraintForm(
                    value = "computer",
                    strength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
                ),
            ),
            requiredLocation = CanonicalStringConstraintForm(value = "home"),
            maximumDailyWork = CanonicalMinutesConstraintForm(value = "180"),
            bufferBeforeMinutes = "10",
            bufferAfterMinutes = "15",
            bufferSpecified = true,
            bufferStrength = CanonicalStrengthForm(
                CanonicalConstraintLevel.SOFT,
                "90",
            ),
            isSplittable = true,
            minimumChunkSeconds = "1800",
            maximumChunkSeconds = "3600",
            maximumSessions = "3",
            minimumGapMinutes = "30",
            maximumSplitDays = "2",
        )

        val draft = form.draft(ITEM_ID).getOrThrow()

        assertEquals(CanonicalRecurrenceKind.FREQUENCY, draft.recurrence?.kind)
        assertEquals(1_440L, draft.recurrence?.minimumSpacingMinutes)
        assertEquals(
            CanonicalConstraintStrengthDraft.hard(),
            draft.constraints.energyStrength,
        )
        assertEquals("computer", draft.constraints.scheduling?.requiredContexts?.single()?.value)
        assertEquals(15L, draft.constraints.scheduling?.buffers?.afterMinutes)
        assertEquals(3, draft.constraints.maximumSessions)
        assertEquals(2, draft.constraints.maximumSplitDays)
        assertEquals(
            draft,
            CanonicalItemEditorForm.from(draft).draft(ITEM_ID).getOrThrow(),
        )
    }

    @Test
    fun dependencyFormsRoundTripAndSupportTypedEditsAndDeletion() {
        val initialDependencies = listOf(
            CanonicalDependencyDraft(
                itemId = PARENT_ID,
                relation = CanonicalDependencyRelation.FINISH_TO_START,
                minimumLagMinutes = 10,
                strength = CanonicalConstraintStrengthDraft.hard(),
            ),
            CanonicalDependencyDraft(
                itemId = SECOND_PREDECESSOR_ID,
                relation = CanonicalDependencyRelation.START_TO_START,
                strength = CanonicalConstraintStrengthDraft.soft(30),
            ),
        )
        val source = newCanonicalDetailedDraft("Dependent work", ItemKind.TASK).copy(
            constraints = CanonicalFlexibleConstraintsDraft(
                scheduling = CanonicalSchedulingConstraintsDraft(
                    dependencies = initialDependencies,
                ),
            ),
        )
        val form = CanonicalItemEditorForm.from(source)

        assertEquals(2, form.dependencies.size)
        assertEquals(source, form.draft(ITEM_ID).getOrThrow())

        val edited = form.copy(
            dependencies = listOf(
                form.dependencies.first().copy(
                    relation = CanonicalDependencyRelation.START_TO_FINISH,
                    minimumLagMinutes = "45",
                    strength = CanonicalStrengthForm(CanonicalConstraintLevel.SOFT, "275"),
                ),
            ),
        ).draft(ITEM_ID).getOrThrow()
        val edge = requireNotNull(edited.constraints.scheduling).dependencies.single()
        assertEquals(PARENT_ID, edge.itemId)
        assertEquals(CanonicalDependencyRelation.START_TO_FINISH, edge.relation)
        assertEquals(45L, edge.minimumLagMinutes)
        assertEquals(275L, edge.strength.weight)

        val unlinked = form.copy(dependencies = emptyList()).draft(ITEM_ID).getOrThrow()
        assertNull(unlinked.constraints.scheduling)
    }

    @Test
    fun dependencyContextRedactsSensitiveTitlesAndWarnsBeforeAReachableCycle() {
        val current = dependencyItem(ITEM_ID, "Current task")
        val sensitive = dependencyItem(
            SECOND_PREDECESSOR_ID,
            "Private medical detail",
            isSensitive = true,
        )
        val pendingPredecessor = PendingCanonicalAuthoringMutation(
            id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            itemId = PARENT_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = newCanonicalDetailedDraft("Downstream task", ItemKind.TASK).copy(
                constraints = CanonicalFlexibleConstraintsDraft(
                    scheduling = CanonicalSchedulingConstraintsDraft(
                        dependencies = listOf(
                            CanonicalDependencyDraft(
                                itemId = ITEM_ID,
                                relation = CanonicalDependencyRelation.FINISH_TO_FINISH,
                                strength = CanonicalConstraintStrengthDraft.soft(50),
                            ),
                        ),
                    ),
                ),
            ),
            createdAt = "2026-09-01T00:00:00Z",
        )
        val context = canonicalDependencyEditorContext(
            DayWeaveUiState(
                canonicalItems = listOf(current, sensitive),
                pendingCanonicalAuthoringMutations = listOf(pendingPredecessor),
            ),
            ITEM_ID,
        )
        val currentDraft = newCanonicalDetailedDraft("Current task", ItemKind.TASK)

        assertTrue(context.options.none { it.id == ITEM_ID })
        assertTrue(
            context.wouldCreateCycle(
                draft = currentDraft,
                candidateItemId = PARENT_ID,
            ),
        )
        assertNotNull(
            context.cycleWarning(
                currentDraft.copy(
                    constraints = CanonicalFlexibleConstraintsDraft(
                        scheduling = CanonicalSchedulingConstraintsDraft(
                            dependencies = listOf(
                                CanonicalDependencyForm(itemId = PARENT_ID).draft(),
                            ),
                        ),
                    ),
                ),
            ),
        )
        val sensitiveOption = requireNotNull(context.option(SECOND_PREDECESSOR_ID))
        assertTrue(sensitiveOption.isSensitive)
        assertFalse(sensitiveOption.displayTitle.contains("medical", ignoreCase = true))
        assertTrue(sensitiveOption.displayTitle.startsWith("Sensitive item"))
    }

    @Test
    fun dependencyContextIncludesImplicitOrderedRoutineEdgesInCycleChecks() {
        val routineId = "44444444-4444-4444-8444-444444444444"
        val routine = dependencyItem(
            routineId,
            "Morning routine",
            kind = ItemKind.ROUTINE,
            routineOrdered = true,
        )
        val first = dependencyItem(
            ITEM_ID,
            "First step",
            parentId = routineId,
            siblingOrder = 0,
        )
        val second = dependencyItem(
            PARENT_ID,
            "Second step",
            parentId = routineId,
            siblingOrder = 1,
        )

        val context = canonicalDependencyEditorContext(
            DayWeaveUiState(canonicalItems = listOf(routine, first, second)),
            ITEM_ID,
        )
        val firstDraft = newCanonicalDetailedDraft("First step", ItemKind.TASK).copy(
            parentId = routineId,
            siblingOrder = 0,
        )

        assertTrue(context.wouldCreateCycle(firstDraft, PARENT_ID))
    }

    @Test
    fun dependencyContextProjectsNewItemsAndLiveOrderedRoutineFields() {
        val routineId = "44444444-4444-4444-8444-444444444444"
        val laterId = PARENT_ID
        val routine = dependencyItem(
            routineId,
            "Morning routine",
            kind = ItemKind.ROUTINE,
            routineOrdered = true,
        )
        val later = dependencyItem(
            laterId,
            "Later step",
            parentId = routineId,
            siblingOrder = 1,
        )
        val context = canonicalDependencyEditorContext(
            DayWeaveUiState(canonicalItems = listOf(routine, later)),
            ITEM_ID,
        )
        val newEarlierDraft = newCanonicalDetailedDraft("New earlier step", ItemKind.TASK).copy(
            parentId = routineId,
            siblingOrder = 0,
            constraints = CanonicalFlexibleConstraintsDraft(
                scheduling = CanonicalSchedulingConstraintsDraft(
                    dependencies = listOf(
                        CanonicalDependencyDraft(
                            itemId = laterId,
                            relation = CanonicalDependencyRelation.FINISH_TO_START,
                            strength = CanonicalConstraintStrengthDraft.hard(),
                        ),
                    ),
                ),
            ),
        )

        assertNotNull(context.cycleWarning(newEarlierDraft))

        val unorderedRoutine = dependencyItem(
            routineId,
            "Morning routine",
            kind = ItemKind.ROUTINE,
            routineOrdered = false,
        )
        val earlierWithReverseEdge = dependencyItem(
            ITEM_ID,
            "Earlier step",
            parentId = routineId,
            siblingOrder = 0,
            dependencies = listOf(
                CanonicalDependencyDraft(
                    itemId = laterId,
                    relation = CanonicalDependencyRelation.FINISH_TO_START,
                    strength = CanonicalConstraintStrengthDraft.hard(),
                ),
            ),
        )
        val routineContext = canonicalDependencyEditorContext(
            DayWeaveUiState(
                canonicalItems = listOf(unorderedRoutine, earlierWithReverseEdge, later),
            ),
            routineId,
        )
        val orderedRoutineDraft = newCanonicalDetailedDraft(
            "Morning routine",
            ItemKind.ROUTINE,
        ).copy(
            constraints = CanonicalFlexibleConstraintsDraft(routineOrdered = true),
        )

        assertNotNull(routineContext.cycleWarning(orderedRoutineDraft))
    }

    @Test
    fun opaqueDependencyMetadataIsNotSelectableAndCannotBypassCycleSafety() {
        val opaqueId = SECOND_PREDECESSOR_ID
        val current = dependencyItem(ITEM_ID, "Current task")
        val opaque = dependencyItem(opaqueId, "Newer dependency metadata").copy(
            flexibleConstraintsJson =
                """{"constraints":{"dependencies":{"schema_version":2}}}""",
        )
        val bridge = dependencyItem(
            PARENT_ID,
            "Known bridge",
            dependencies = listOf(
                CanonicalDependencyDraft(
                    itemId = opaqueId,
                    relation = CanonicalDependencyRelation.FINISH_TO_START,
                    strength = CanonicalConstraintStrengthDraft.hard(),
                ),
            ),
        )
        val context = canonicalDependencyEditorContext(
            DayWeaveUiState(canonicalItems = listOf(current, opaque, bridge)),
            ITEM_ID,
        )
        val currentDraft = newCanonicalDetailedDraft("Current task", ItemKind.TASK)
        val draftDependingOnBridge = currentDraft.copy(
            constraints = CanonicalFlexibleConstraintsDraft(
                scheduling = CanonicalSchedulingConstraintsDraft(
                    dependencies = listOf(
                        CanonicalDependencyDraft(
                            itemId = PARENT_ID,
                            relation = CanonicalDependencyRelation.FINISH_TO_START,
                            strength = CanonicalConstraintStrengthDraft.hard(),
                        ),
                    ),
                ),
            ),
        )

        assertTrue(requireNotNull(context.option(opaqueId)).hasOpaqueDependencies)
        assertTrue(context.selectableOptions.none { it.id == opaqueId })
        assertEquals(
            "Cannot verify cycle safety",
            context.candidateIssue(currentDraft, PARENT_ID),
        )
        assertTrue(context.wouldCreateCycle(currentDraft, PARENT_ID))
        assertEquals(
            "Dependency safety cannot be verified because a related item uses newer metadata.",
            context.cycleWarning(draftDependingOnBridge),
        )
    }

    @Test
    fun dependencyContextEnforcesRecurringSubtreeOwnershipAcrossLiveAndPendingHierarchy() {
        val recurringRootId = "44444444-4444-4444-8444-444444444444"
        val otherRecurringRootId = "55555555-5555-4555-8555-555555555555"
        val ordinaryId = "66666666-6666-4666-8666-666666666666"
        val missingParentId = "77777777-7777-4777-8777-777777777777"
        val daily = CanonicalRecurrenceDraft(
            kind = CanonicalRecurrenceKind.DAILY,
            occurrencesPerPeriod = 1,
        )
        val recurringRoot = dependencyItem(
            recurringRootId,
            "Recurring routine",
            kind = ItemKind.ROUTINE,
            recurrence = daily,
        )
        val otherRecurringRoot = dependencyItem(
            otherRecurringRootId,
            "Other recurring routine",
            kind = ItemKind.ROUTINE,
            recurrence = daily,
        )
        val recurringPredecessor = dependencyItem(
            PARENT_ID,
            "Recurring step",
            parentId = recurringRootId,
        )
        val ordinaryPredecessor = dependencyItem(ordinaryId, "Ordinary setup")
        val current = dependencyItem(
            ITEM_ID,
            "Current step",
            dependencies = listOf(
                CanonicalDependencyDraft(
                    itemId = PARENT_ID,
                    relation = CanonicalDependencyRelation.FINISH_TO_START,
                    strength = CanonicalConstraintStrengthDraft.hard(),
                ),
            ),
            parentId = recurringRootId,
        )
        val context = canonicalDependencyEditorContext(
            DayWeaveUiState(
                canonicalItems = listOf(
                    recurringRoot,
                    otherRecurringRoot,
                    recurringPredecessor,
                    ordinaryPredecessor,
                    current,
                ),
            ),
            ITEM_ID,
        )
        val sameOwnerDraft = newCanonicalDetailedDraft("Current step", ItemKind.TASK).copy(
            parentId = recurringRootId,
        )

        assertNull(context.candidateIssue(sameOwnerDraft, PARENT_ID))
        assertEquals(
            "Different recurring subtree",
            context.candidateIssue(sameOwnerDraft.copy(parentId = null), PARENT_ID),
        )
        assertEquals(
            "Different recurring subtree",
            context.candidateIssue(
                sameOwnerDraft.copy(parentId = otherRecurringRootId),
                PARENT_ID,
            ),
        )
        assertNull(
            context.candidateIssue(
                sameOwnerDraft.copy(parentId = null, recurrence = daily),
                ordinaryId,
            ),
        )
        assertEquals(
            "A recurring predecessor can only be linked from within the same recurring subtree.",
            context.cycleWarning(
                sameOwnerDraft.copy(
                    parentId = null,
                    constraints = CanonicalFlexibleConstraintsDraft(
                        scheduling = CanonicalSchedulingConstraintsDraft(
                            dependencies = listOf(
                                CanonicalDependencyDraft(
                                    itemId = PARENT_ID,
                                    relation = CanonicalDependencyRelation.FINISH_TO_START,
                                    strength = CanonicalConstraintStrengthDraft.hard(),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        )

        val pendingReparent = PendingCanonicalAuthoringMutation(
            id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaab",
            itemId = PARENT_ID,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = newCanonicalDetailedDraft("Recurring step", ItemKind.TASK).copy(
                parentId = recurringRootId,
                siblingOrder = 9,
            ),
            createdAt = "2026-09-01T00:00:00Z",
        )
        val pendingContext = canonicalDependencyEditorContext(
            DayWeaveUiState(
                canonicalItems = listOf(
                    recurringRoot,
                    dependencyItem(PARENT_ID, "Initially ordinary"),
                    dependencyItem(ITEM_ID, "External successor"),
                ),
                pendingCanonicalAuthoringMutations = listOf(pendingReparent),
            ),
            ITEM_ID,
        )
        assertEquals(
            "Different recurring subtree",
            pendingContext.candidateIssue(
                newCanonicalDetailedDraft("External successor", ItemKind.TASK),
                PARENT_ID,
            ),
        )

        val pendingRootId = "88888888-8888-4888-8888-888888888888"
        val pendingChildId = "99999999-9999-4999-8999-999999999999"
        val pendingRoot = PendingCanonicalAuthoringMutation(
            id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaac",
            itemId = pendingRootId,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = newCanonicalDetailedDraft("Pending recurring routine", ItemKind.ROUTINE).copy(
                recurrence = daily,
            ),
            createdAt = "2026-09-01T00:00:00Z",
        )
        val pendingChild = PendingCanonicalAuthoringMutation(
            id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaad",
            itemId = pendingChildId,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = newCanonicalDetailedDraft("Pending recurring step", ItemKind.TASK).copy(
                parentId = pendingRootId,
                siblingOrder = 4,
            ),
            createdAt = "2026-09-01T00:00:00Z",
        )
        val pendingCreateContext = canonicalDependencyEditorContext(
            DayWeaveUiState(
                pendingCanonicalAuthoringMutations = listOf(pendingRoot, pendingChild),
            ),
            ITEM_ID,
        )
        assertNull(
            pendingCreateContext.candidateIssue(
                newCanonicalDetailedDraft("New matching step", ItemKind.TASK).copy(
                    parentId = pendingRootId,
                ),
                pendingChildId,
            ),
        )
        assertEquals(
            "Different recurring subtree",
            pendingCreateContext.candidateIssue(
                newCanonicalDetailedDraft("New external step", ItemKind.TASK),
                pendingChildId,
            ),
        )

        val unknownOwner = dependencyItem(
            SECOND_PREDECESSOR_ID,
            "Locally incomplete hierarchy",
            parentId = missingParentId,
        )
        val unknownContext = canonicalDependencyEditorContext(
            DayWeaveUiState(canonicalItems = listOf(unknownOwner)),
            ITEM_ID,
        )
        assertEquals(
            "Cannot verify recurring-subtree ownership",
            unknownContext.candidateIssue(
                newCanonicalDetailedDraft("Successor", ItemKind.TASK),
                SECOND_PREDECESSOR_ID,
            ),
        )
    }

    @Test
    fun kindSpecificControlsProduceHabitRoutineGoalAndBreakMetadata() {
        val habit = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Hydrate", ItemKind.HABIT),
        ).copy(
            hasHabitTarget = true,
            habitTargetAmount = "8",
            habitTargetUnit = "glasses",
            preservesStreakWhenPaused = false,
            preservesStreakSpecified = true,
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(8L, habit.constraints.habitTarget?.amount)
        assertEquals(false, habit.constraints.preservesStreakWhenPaused)

        val routine = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Review", ItemKind.ROUTINE),
        ).copy(
            routineOrdered = true,
            routineOrderedSpecified = true,
            hasOwnEffort = true,
            hasOwnEffortSpecified = true,
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(true, routine.constraints.routineOrdered)
        assertEquals(true, routine.constraints.hasOwnEffort)

        val goal = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Book", ItemKind.GOAL),
        ).copy(
            goalMeasures = listOf(CanonicalGoalMeasureForm("chapters", "12", "3", "chapters")),
            goalMeasuresSpecified = true,
            hasGoalWeeklyAllocation = true,
            goalWeeklyMinimumMinutes = "120",
            goalWeeklyMaximumMinutes = "300",
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(12L, goal.constraints.goalMeasures?.single()?.target)
        assertEquals(300L, goal.constraints.goalWeeklyAllocation?.maximumMinutes)

        val breakDraft = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Move", ItemKind.BREAK),
        ).copy(
            breakCategory = CanonicalBreakCategory.MOVEMENT,
            breakCategorySpecified = true,
            breakMandatory = true,
            breakMandatorySpecified = true,
            breakPromptToResume = false,
            breakPromptSpecified = true,
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(CanonicalBreakCategory.MOVEMENT, breakDraft.constraints.breakCategory)
        assertEquals(true, breakDraft.constraints.breakMandatory)
        assertEquals(false, breakDraft.constraints.breakPromptToResume)
    }

    @Test
    fun plannedUnknownDurationAndInboxHabitWithoutRecurrenceStayEditable() {
        val planned = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Estimate later", ItemKind.TASK),
        ).copy(
            placement = CanonicalDraftPlacement.PLANNED,
            hasDuration = false,
        ).draft(ITEM_ID)
        assertTrue(planned.isSuccess)
        assertNull(planned.getOrThrow().durationSeconds)

        val inboxHabit = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Maybe daily", ItemKind.HABIT),
        ).copy(recurrenceKind = null).draft(ITEM_ID)
        assertTrue(inboxHabit.isSuccess)
        assertNull(inboxHabit.getOrThrow().recurrence)
    }

    @Test
    fun tagContainingCommaAndUnqualifiedZeroBufferRoundTripLosslessly() {
        val draft = newCanonicalDetailedDraft("Metadata", ItemKind.TASK).copy(
            constraints = CanonicalFlexibleConstraintsDraft(
                tags = listOf("focus,writing"),
                scheduling = CanonicalSchedulingConstraintsDraft(
                    buffers = CanonicalBufferPolicyDraft(0, 0, null),
                ),
            ),
        )

        val form = CanonicalItemEditorForm.from(draft)
        val rebuilt = form.draft(ITEM_ID).getOrThrow()

        assertEquals(listOf("focus,writing"), form.tags)
        assertEquals(listOf("focus,writing"), rebuilt.constraints.tags)
        assertTrue(form.bufferSpecified)
        assertNull(rebuilt.constraints.scheduling?.buffers?.strength)
        assertEquals(0L, rebuilt.constraints.scheduling?.buffers?.beforeMinutes)
    }

    @Test
    fun finiteCustomRruleCanBeCreatedChangedAndConvertedInTheForm() {
        val custom = newCanonicalDetailedDraft("Custom recurrence", ItemKind.ROUTINE).copy(
            recurrence = CanonicalRecurrenceDraft(
                CanonicalRecurrenceKind.CUSTOM,
                rrule = "FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=-1,1;COUNT=24",
            ),
        )
        val form = CanonicalItemEditorForm.from(custom)

        assertEquals(custom, form.draft(ITEM_ID).getOrThrow())
        assertEquals(
            "FREQ=DAILY;INTERVAL=1;COUNT=10",
            form.copy(recurrenceRrule = "count=10;freq=daily")
                .draft(ITEM_ID).getOrThrow().recurrence?.rrule,
        )
        assertEquals(
            CanonicalRecurrenceKind.DAILY,
            form.copy(recurrenceKind = CanonicalRecurrenceKind.DAILY)
                .draft(ITEM_ID).getOrThrow().recurrence?.kind,
        )
        val fresh = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("New", ItemKind.TASK),
        ).copy(
            recurrenceKind = CanonicalRecurrenceKind.CUSTOM,
            recurrenceRrule = "FREQ=WEEKLY;BYDAY=MO,FR;COUNT=12",
        )
        assertEquals(
            "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,FR;COUNT=12",
            fresh.draft(ITEM_ID).getOrThrow().recurrence?.rrule,
        )
        assertTrue(
            fresh.copy(recurrenceRrule = "FREQ=MONTHLY;BYDAY=1MO;COUNT=2")
                .draft(ITEM_ID).isFailure,
        )
        assertTrue(fresh.copy(recurrenceRrule = "FREQ=DAILY").draft(ITEM_ID).isFailure)
    }

    @Test
    fun fractionalEventBoundsDoNotInventAnIntegralDuration() {
        val result = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Precise event", ItemKind.EVENT),
        ).copy(
            eventStart = "2026-08-31T09:00:00.000001Z",
            eventEnd = "2026-08-31T10:00:00.000002Z",
        ).draft(ITEM_ID).getOrThrow()

        assertNull(result.durationSeconds)
        assertEquals(result.eventTiming?.startsAt, result.earliestStartAt)
        assertEquals(result.eventTiming?.endsAt, result.deadlineAt)

        val unchanged = result.copy(
            earliestStartAt = null,
            deadlineAt = null,
            durationSeconds = null,
            eventTiming = CanonicalEventTimingDraft(
                "2026-08-31T09:00:00.000001Z",
                "2026-08-31T10:00:00.000002Z",
            ),
        )
        val rebuilt = CanonicalItemEditorForm.from(unchanged).draft(ITEM_ID).getOrThrow()
        assertNull(rebuilt.durationSeconds)
        assertNull(rebuilt.earliestStartAt)
        assertNull(rebuilt.deadlineAt)
    }

    @Test
    fun incompleteInboxEventMetadataIsVisiblePreservedAndClearedOnlyExplicitly() {
        val source = newCanonicalDetailedDraft("Candidate meeting", ItemKind.EVENT).copy(
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.LOW,
                tags = listOf("family,calendar"),
                scheduling = CanonicalSchedulingConstraintsDraft(
                    buffers = CanonicalBufferPolicyDraft(5, 0, null),
                    includesNullOccurrenceWindow = true,
                ),
                hasOwnEffort = false,
            ),
        )
        source.requireValid(ITEM_ID)
        val form = CanonicalItemEditorForm.from(source)

        assertEquals(EnergyLevel.LOW, form.energy)
        assertEquals(listOf("family,calendar"), form.tags)
        assertTrue(form.bufferSpecified)
        assertTrue(form.includesNullOccurrenceWindow)
        assertEquals(source, form.draft(ITEM_ID).getOrThrow())

        val timed = form.copy(
            placement = CanonicalDraftPlacement.PLANNED,
            eventStart = "2026-08-31T09:00:00Z",
            eventEnd = "2026-08-31T10:00:00Z",
        )
        assertTrue(timed.draft(ITEM_ID).isFailure)
        val cleared = timed.withoutEventFlexibleMetadata().draft(ITEM_ID).getOrThrow()
        assertEquals(CanonicalFlexibleConstraintsDraft(), cleared.constraints)
        assertEquals(3_600L, cleared.durationSeconds)
        assertEquals("2026-08-31T09:00:00Z", cleared.eventTiming?.startsAt)
    }

    @Test
    fun longEventBoundsRemainAuthorableWithNoCanonicalDuration() {
        val result = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Long hold", ItemKind.EVENT),
        ).copy(
            placement = CanonicalDraftPlacement.PLANNED,
            eventStart = "2026-01-01T00:00:00Z",
            eventEnd = "2028-01-01T00:00:00Z",
        ).draft(ITEM_ID).getOrThrow()

        assertNull(result.durationSeconds)
        assertEquals("2028-01-01T00:00:00Z", result.eventTiming?.endsAt)
    }

    @Test
    fun legacyReviewRouteCarriesProvenanceIntoEditableNotes() {
        val source = InboxItem(
            id = "proposal-synthetic",
            isSensitive = true,
            title = "Suggested next action",
            source = InboxSource.EXTERNAL_PROPOSAL,
            detail = "Review this synthetic context before scheduling.",
        )

        val route = CanonicalItemEditorRoute.fromInbox(source)

        assertEquals(source.id, route.sourceInboxId)
        assertEquals(source.title, route.initialDraft.title)
        assertEquals(source.detail, route.initialDraft.notes)
        assertEquals(true, route.initialDraft.isSensitive)
        assertEquals(CanonicalItemEditorMode.CREATE, route.mode)
    }

    private fun dependencyItem(
        id: String,
        title: String,
        dependencies: List<CanonicalDependencyDraft> = emptyList(),
        isSensitive: Boolean = false,
        kind: ItemKind = ItemKind.TASK,
        recurrence: CanonicalRecurrenceDraft? = null,
        routineOrdered: Boolean? = null,
        parentId: String? = null,
        siblingOrder: Long = 0,
    ) = CanonicalItemSnapshot(
        id = id,
        isSensitive = isSensitive,
        kind = kind.name.lowercase(),
        status = "inbox",
        title = title,
        timezoneName = "UTC",
        recurrenceJson = recurrence?.toCanonicalJson()?.toString(),
        flexibleConstraintsJson = CanonicalFlexibleConstraintsDraft(
            scheduling = dependencies.takeIf { it.isNotEmpty() }?.let {
                CanonicalSchedulingConstraintsDraft(dependencies = it)
            },
            routineOrdered = routineOrdered,
        ).toCanonicalJson(null).toString(),
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = siblingOrder,
        isExecutable = false,
        revision = 1,
        createdAt = "2026-08-30T10:00:00Z",
        updatedAt = "2026-08-30T10:00:00Z",
    )

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val PARENT_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_PREDECESSOR_ID = "33333333-3333-4333-8333-333333333333"
    }
}
