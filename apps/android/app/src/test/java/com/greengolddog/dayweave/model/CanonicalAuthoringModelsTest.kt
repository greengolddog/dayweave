package com.greengolddog.dayweave.model

import kotlinx.serialization.json.long
import kotlinx.serialization.json.jsonPrimitive
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringModelsTest {
    @Test
    fun allSixKindsHaveAValidTypedInboxOrPlannedDraft() {
        val drafts = listOf(
            taskDraft(),
            taskDraft().copy(
                kind = ItemKind.HABIT,
                recurrence = CanonicalRecurrenceDraft(
                    kind = CanonicalRecurrenceKind.DAILY,
                    occurrencesPerPeriod = 2,
                ),
            ),
            taskDraft().copy(
                kind = ItemKind.ROUTINE,
                recurrence = CanonicalRecurrenceDraft(
                    kind = CanonicalRecurrenceKind.WEEKLY,
                    occurrencesPerPeriod = 3,
                    weekdays = listOf(
                        CanonicalWeekday.MONDAY,
                        CanonicalWeekday.WEDNESDAY,
                        CanonicalWeekday.FRIDAY,
                    ),
                ),
            ),
            taskDraft().copy(
                kind = ItemKind.GOAL,
                durationSeconds = null,
                split = CanonicalSplitDraft(),
            ),
            eventDraft(),
            taskDraft().copy(kind = ItemKind.BREAK, split = CanonicalSplitDraft()),
        )

        drafts.forEach { it.requireValid(ITEM_ID) }
        assertEquals(
            setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE, ItemKind.GOAL,
                ItemKind.EVENT, ItemKind.BREAK),
            drafts.map { it.kind }.toSet(),
        )
        assertEquals(
            setOf(CanonicalDraftPlacement.INBOX, CanonicalDraftPlacement.PLANNED),
            drafts.map { it.placement }.toSet(),
        )
    }

    @Test
    fun recurrenceSplitHierarchyAndEventBoundsFailClosed() {
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(kind = ItemKind.HABIT, recurrence = null).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(
                durationSeconds = 1_800,
                split = CanonicalSplitDraft(
                    kind = CanonicalSplitKind.SPLITTABLE,
                    minimumChunkSeconds = 900,
                    maximumChunkSeconds = 3_600,
                ),
            ).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(parentId = ITEM_ID).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            eventDraft().copy(deadlineAt = "2026-08-30T11:30:00Z").requireValid(ITEM_ID)
        }
    }

    @Test
    fun intervalRecurrenceUsesWholeMinuteWireUnits() {
        val recurrence = CanonicalRecurrenceDraft(
            kind = CanonicalRecurrenceKind.EVERY_INTERVAL,
            intervalSeconds = 2 * 60 * 60,
        )
        assertEquals(120L, recurrence.toCanonicalJson().getValue("interval").jsonPrimitive.long)
        assertThrows(IllegalArgumentException::class.java) {
            recurrence.copy(intervalSeconds = 90).requireValid()
        }

        val draft = taskDraft().copy(recurrence = recurrence)
        assertEquals(recurrence, canonicalItem(draft).toCanonicalDraft().recurrence)
    }

    @Test
    fun preferredStartRequiresDurationAndMustFinishWithinTheDay() {
        val preferred = CanonicalFlexibleConstraintsDraft(preferredStartMinute = 23 * 60)
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(durationSeconds = null, constraints = preferred)
                .requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(durationSeconds = 3_601, constraints = preferred)
                .requireValid(ITEM_ID)
        }
        taskDraft().copy(
            durationSeconds = 3_601,
            constraints = preferred.copy(preferredStartMinute = 22 * 60 + 59),
            split = CanonicalSplitDraft(),
        ).requireValid(ITEM_ID)
    }

    @Test
    fun allDayFirmBlockUsesExclusiveLocalMidnightsAndSoleMetadata() {
        val timing = CanonicalEventTimingDraft(
            startsAt = "2026-08-29T22:00:00Z",
            endsAt = "2026-08-30T22:00:00Z",
            allDay = true,
        )
        val draft = eventDraft().copy(
            durationSeconds = 24 * 60 * 60,
            earliestStartAt = timing.startsAt,
            deadlineAt = timing.endsAt,
            eventTiming = timing,
        )
        draft.requireValid(ITEM_ID)
        assertEquals(
            setOf("dayweave_firm_block"),
            draft.constraints.toCanonicalJson(
                timing,
                draft.durationSeconds,
                draft.timezoneName,
            ).keys,
        )
        assertThrows(IllegalArgumentException::class.java) {
            draft.copy(
                eventTiming = timing.copy(startsAt = "2026-08-29T23:00:00Z"),
                earliestStartAt = "2026-08-29T23:00:00Z",
                durationSeconds = 23 * 60 * 60,
            ).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            draft.copy(
                constraints = CanonicalFlexibleConstraintsDraft(energy = EnergyLevel.DEEP),
            ).requireValid(ITEM_ID)
        }
    }

    @Test
    fun authoringRejectsJavaOnlyZonesAndNanosecondTimestamps() {
        listOf("+02:00", "GMT+02:00", "SystemV/EST5EDT").forEach { timezone ->
            assertThrows(IllegalArgumentException::class.java) {
                taskDraft().copy(timezoneName = timezone).requireValid(ITEM_ID)
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(earliestStartAt = "2026-08-30T09:00:00.000000001Z")
                .requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = ITEM_ID,
                operation = CanonicalAuthoringOperation.CREATE,
                draft = taskDraft(),
                createdAt = "2026-08-30T10:00:00.000000001Z",
            ).requireValid()
        }
        taskDraft().copy(timezoneName = "GMT").requireValid(ITEM_ID)
        taskDraft().copy(earliestStartAt = "2026-08-30T09:00:00.000001Z")
            .requireValid(ITEM_ID)
    }

    @Test
    fun authoringJournalHasPerMutationAndAggregateEncodedBudgets() {
        val oversizedBase = canonicalItem(taskDraft()).copy(
            flexibleConstraintsJson = "x".repeat(
                CanonicalAuthoringJournalPolicy.MAX_MUTATION_BYTES + 1,
            ),
        )
        assertThrows(IllegalArgumentException::class.java) {
            PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = ITEM_ID,
                operation = CanonicalAuthoringOperation.TRASH,
                expectedRevision = oversizedBase.revision,
                baseItem = oversizedBase,
                createdAt = "2026-08-30T10:00:00Z",
            ).requireValid()
        }

        val largeDraft = taskDraft().copy(notes = "😀".repeat(100_000))
        val mutations = (0 until 12).map { index ->
            PendingCanonicalAuthoringMutation(
                id = stableUuid("large-mutation-$index"),
                itemId = stableUuid("large-item-$index"),
                operation = CanonicalAuthoringOperation.CREATE,
                draft = largeDraft,
                createdAt = "2026-08-30T10:00:00Z",
            )
        }
        mutations.forEach(PendingCanonicalAuthoringMutation::requireValid)
        assertThrows(IllegalArgumentException::class.java) {
            requireCanonicalAuthoringJournalBudget(mutations)
        }
    }

    @Test
    fun draftReconstructsAndMatchesTheExactCanonicalAuthoringSubset() {
        val draft = taskDraft().copy(
            isSensitive = true,
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.DEEP,
                tags = listOf("work", "focus"),
                preferredStartMinute = 540,
                minimumGapMinutes = 30,
                maximumSessions = 3,
            ).normalized(),
        )
        val item = canonicalItem(draft)

        assertTrue(draft.matches(item))
        assertEquals(draft, item.toCanonicalDraft())
        assertFalse(draft.matches(item.copy(urgency = draft.urgency + 1)))
    }

    @Test
    fun submittedJournalRequiresExactIdentityBindingAndImmutableShape() {
        val draft = taskDraft()
        val mutation = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft,
            createdAt = "2026-08-30T10:00:00Z",
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            submittedAt = "2026-08-30T10:01:00Z",
        )

        mutation.requireValid()
        assertTrue(mutation.isSubmitted)
        assertThrows(IllegalArgumentException::class.java) {
            mutation.copy(idempotencyKey = "different").requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            mutation.copy(syncOrigin = null, submittedAt = null).requireValid()
        }
        mutation.copy(
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "revision changed",
        ).requireValid()
    }

    private fun taskDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.INBOX,
        kind = ItemKind.TASK,
        title = "Write Android persistence tests",
        notes = "Keep the exact draft encrypted",
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        earliestStartAt = "2026-08-30T09:00:00Z",
        deadlineAt = "2026-08-31T09:00:00Z",
        split = CanonicalSplitDraft(
            kind = CanonicalSplitKind.SPLITTABLE,
            minimumChunkSeconds = 900,
            maximumChunkSeconds = 2_700,
        ),
        importance = 80,
        urgency = 60,
        siblingOrder = 2,
    )

    private fun eventDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.EVENT,
        title = "Planning call",
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        earliestStartAt = "2026-08-30T10:00:00Z",
        deadlineAt = "2026-08-30T11:00:00Z",
        eventTiming = CanonicalEventTimingDraft(
            startsAt = "2026-08-30T10:00:00Z",
            endsAt = "2026-08-30T11:00:00Z",
        ),
    )

    private fun canonicalItem(draft: CanonicalItemDraft) = CanonicalItemSnapshot(
        id = ITEM_ID,
        isSensitive = draft.isSensitive,
        kind = draft.kind.name.lowercase(),
        status = draft.placement.wireValue,
        title = draft.title,
        notes = draft.notes,
        timezoneName = draft.timezoneName,
        durationSeconds = draft.durationSeconds,
        deadlineAt = draft.deadlineAt,
        earliestStartAt = draft.earliestStartAt,
        recurrenceJson = draft.recurrence?.toCanonicalJson()?.toString(),
        flexibleConstraintsJson = draft.constraints.toCanonicalJson(
            draft.eventTiming,
            draft.durationSeconds,
            draft.timezoneName,
        ).toString(),
        splitPolicyJson = draft.split.toCanonicalJson(draft.durationSeconds).toString(),
        importance = draft.importance,
        urgency = draft.urgency,
        parentId = draft.parentId,
        siblingOrder = draft.siblingOrder,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-30T10:00:00Z",
        updatedAt = "2026-08-30T10:00:00Z",
    )

    private fun stableUuid(seed: String): String =
        UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val MUTATION_ID = "22222222-2222-4222-8222-222222222222"
    }
}
