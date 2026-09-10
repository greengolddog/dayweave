package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.*
import org.junit.Assert.*
import org.junit.Test

class RoutineOccurrencePresentationTest {
    @Test fun completeFlatTreeIncludesUnscheduledOptionalMembersAndExactReopening() {
        val rows = routineTestSnapshot(true).reviewRows()
        assertEquals(listOf(ROUTINE_ROOT, ROUTINE_CHILD, ROUTINE_OPTIONAL), rows.map { it.definition.itemId })
        assertEquals(listOf(0, 1, 1), rows.map { it.depth })
        assertTrue(rows.first().isParent)
        assertEquals("completed", rows.first().state.status)
        assertEquals("blocked", rows.first().state.open.status)
        assertFalse(rows.last().state.requiredForParent)
        assertEquals("planned", rows.last().state.status)
        assertEquals(1L, rows.first().evaluation.counts.completed)
        assertTrue(rows.last().canChange(true, true, false))
        assertFalse(rows.last().canChange(false, true, false))
        assertFalse(rows.last().canChange(true, false, false))
        assertFalse(rows.last().canChange(true, true, true))
    }

    @Test fun nestedIndependentRecurrenceNeverInheritsControls() {
        val row = routineTestSnapshot().reviewRows().last()
        assertFalse(row.copy(evaluation = row.evaluation.copy(occurrenceEvidenceRequired = true)).canChange(true, true, false))
    }

    @Test fun selectedIdentityUsesOnlyExactPublishedMappingEvenWhenCanonicalRootIsMissing() {
        val block = ScheduleItem(id = "synthetic", title = "Synthetic", kind = ItemKind.TASK, startMinute = 10,
            durationMinutes = 20, status = ItemStatus.SCHEDULED, canonicalItemId = ROUTINE_CHILD, occurrenceId = ROUTINE_PLANNER_ID)
        val state = routineStateTestUi().copy(occurrenceSeriesItemIds = mapOf(ROUTINE_PLANNER_ID to ROUTINE_ROOT))
        assertEquals(ROUTINE_ROOT, state.routineOccurrenceSelection(block)?.seriesItemId)
        assertEquals(ROUTINE_PLANNER_ID, state.routineOccurrenceSelection(block)?.occurrenceId)
        assertNull(state.copy(occurrenceSeriesItemIds = emptyMap()).routineOccurrenceSelection(block))
    }

    @Test fun planningFingerprintCannotReuseMemoAcrossPrivateAuthorityOrReadGeneration() {
        val before = routineStateTestUi()
        val fingerprint = before.localScheduleCompositionStateFingerprint()
        val after = before.copy(routineOccurrenceAuthorityGeneration = 1)
        after.inheritLocalScheduleCompositionMemo(before)
        assertNotEquals(fingerprint, after.localScheduleCompositionStateFingerprint())
        assertTrue(after.requiresRemoteRoutineOccurrenceComposition())
        val policy = routineTestSnapshot().copy(evidenceHash = "sha256:" + "b".repeat(64), freshEditEligible = false)
        assertNotEquals(fingerprint, before.copy(routineOccurrenceLedger = routineStateTestLedger(policy)).localScheduleCompositionStateFingerprint())
    }

    @Test fun emptyNeverManagedTerminalPreservesLocalV1ButCursorAndLatchRemainFenced() {
        val empty = routineStateTestUi(RoutineOccurrenceLedger(syncOrigin = PROGRESS_ORIGIN, configurationId = PROGRESS_CONFIGURATION,
            deltaCursor = "DWR1.empty"))
        assertFalse(empty.requiresRemoteRoutineOccurrenceComposition())
        assertTrue(empty.copy(routineOccurrenceLedger = empty.routineOccurrenceLedger.copy(needsRemoteScheduleCatchUp = true))
            .requiresRemoteRoutineOccurrenceComposition())
        assertNotEquals(empty.localScheduleCompositionStateFingerprint(), empty.copy(routineOccurrenceLedger =
            empty.routineOccurrenceLedger.copy(deltaCursor = "DWR1.another-empty")).localScheduleCompositionStateFingerprint())
    }
}
