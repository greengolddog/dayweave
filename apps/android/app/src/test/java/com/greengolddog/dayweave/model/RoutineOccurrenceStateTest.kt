package com.greengolddog.dayweave.model

import org.junit.Assert.*
import org.junit.Test

class RoutineOccurrenceStateTest {
    @Test fun strictBindingExactTypedRequestAndOneIntentPerInstance() {
        val intent = routineStateTestIntent()
        val staged = routineStateTestLedger().enqueueRoutineOccurrence(intent)
        assertEquals(intent.requestJson, staged.pending.single().requestJson)
        val otherRequest = intent.request.copy(operationId = routineStateTestId(201))
        val otherMember = intent.copy(operationId = otherRequest.operationId, memberId = ROUTINE_OPTIONAL,
            request = otherRequest, requestJson = ITEM_PROGRESS_JSON.encodeToString(otherRequest))
        otherMember.requireValid()
        assertThrows(IllegalArgumentException::class.java) { staged.enqueueRoutineOccurrence(otherMember) }
        assertThrows(IllegalArgumentException::class.java) { staged.copy(pending = listOf(intent, intent)).requireValid() }
        assertThrows(IllegalArgumentException::class.java) { intent.copy(request = intent.request.copy(expectedInstanceRevision = 2)).requireValid() }
        assertThrows(IllegalArgumentException::class.java) { staged.copy(configurationId = "other-owner").requireValid() }
        assertThrows(IllegalArgumentException::class.java) { routineStateTestLedger().copy(observations = mapOf(
            ROUTINE_PLANNER_ID to routineStateTestLedger().observations.getValue(ROUTINE_INSTANCE))).requireValid() }
        assertThrows(IllegalArgumentException::class.java) { staged.bindRoutineOccurrences(PROGRESS_ORIGIN, "other-owner") }
    }

    @Test fun equalRevisionRequiresExactAggregateButAcceptsFreshEphemeralEvidence() {
        val ledger = routineStateTestLedger()
        val updated = routineTestSnapshot().copy(evidenceHash = "sha256:" + "b".repeat(64), freshEditEligible = false,
            members = routineTestSnapshot().members.map { it.copy(reason = RoutineOccurrenceReason.POLICY_REVIEWED) })
        assertEquals(updated, ledger.observeRoutineOccurrence(RoutineOccurrenceObservation(updated, ROUTINE_NOW)).observations[ROUTINE_INSTANCE]?.snapshot)
        val manifestChanged = updated.copy(aggregate = updated.aggregate.copy(manifest = updated.aggregate.manifest.copy(
            members = updated.aggregate.manifest.members.map { it.copy(title = "Altered immutable title") })))
        assertThrows(IllegalArgumentException::class.java) { ledger.observeRoutineOccurrence(RoutineOccurrenceObservation(manifestChanged, ROUTINE_NOW)) }
        val aggregateChanged = updated.copy(aggregate = updated.aggregate.copy(members = updated.aggregate.members.map {
            if (it.itemId == ROUTINE_OPTIONAL) it.copy(status = "skipped") else it }))
        aggregateChanged.requireValid()
        assertThrows(IllegalArgumentException::class.java) { ledger.observeRoutineOccurrence(RoutineOccurrenceObservation(aggregateChanged, ROUTINE_NOW)) }
    }

    @Test fun historicalReceiptSettlesExactJournalAndPinsMinimumWithoutOverwritingNewerObservation() {
        val intent = routineStateTestIntent(true)
        val newer = routineTestSnapshot(true).let { it.copy(aggregate = it.aggregate.copy(revision = 3), freshEditEligible = false) }
        val ledger = routineStateTestLedger(newer).copy(pending = listOf(intent))
        val settled = ledger.settleRoutineOccurrence(intent, routineTestMutation(true), ROUTINE_NOW)
        assertEquals(newer, settled.observations[ROUTINE_INSTANCE]?.snapshot)
        assertTrue(settled.pending.isEmpty())
        assertEquals(mapOf(ROUTINE_INSTANCE to 2L), settled.minimumCatchUpRevisions)
        assertTrue(settled.needsRemoteScheduleCatchUp)
        assertThrows(IllegalArgumentException::class.java) { ledger.settleRoutineOccurrence(intent.copy(requestJson = intent.requestJson + " "), routineTestMutation(true), ROUTINE_NOW) }
        assertThrows(IllegalArgumentException::class.java) { settled.settleRoutineOccurrence(intent, routineTestMutation(true), ROUTINE_NOW) }
    }

    @Test fun equalRevisionHistoricalReceiptPreservesEveryLaterGetObservationFieldAfterSourceDrift() {
        val intent = routineStateTestIntent(true)
        val receipt = routineTestMutation(true)
        val laterGet = RoutineOccurrenceObservation(receipt.occurrence.copy(
            evidenceHash = "sha256:" + "b".repeat(64), freshEditEligible = false,
            members = receipt.occurrence.members.map { it.copy(reason = RoutineOccurrenceReason.POLICY_REVIEWED) }),
            "2026-09-10T12:00:00Z")
        val ledger = routineStateTestLedger().copy(observations = mapOf(ROUTINE_INSTANCE to laterGet), pending = listOf(intent))
        val settled = ledger.settleRoutineOccurrence(intent, receipt, "2026-09-10T13:00:00Z")
        assertEquals(ledger.observations, settled.observations)
        assertEquals(laterGet, settled.observations[ROUTINE_INSTANCE])
        assertTrue(settled.pending.isEmpty())
        assertEquals(mapOf(ROUTINE_INSTANCE to 2L), settled.minimumCatchUpRevisions)
        assertTrue(settled.needsRemoteScheduleCatchUp)

        val changedAggregate = laterGet.snapshot.aggregate.copy(members = laterGet.snapshot.aggregate.members.map {
            if (it.itemId == ROUTINE_OPTIONAL) it.copy(status = "skipped") else it })
        val changedManifest = laterGet.snapshot.aggregate.copy(manifest = laterGet.snapshot.aggregate.manifest.copy(
            members = laterGet.snapshot.aggregate.manifest.members.map { it.copy(title = "Different immutable title") }))
        for (aggregate in listOf(changedAggregate, changedManifest)) {
            val changedObservation = laterGet.copy(snapshot = laterGet.snapshot.copy(aggregate = aggregate))
            changedObservation.snapshot.requireValid()
            val conflicting = ledger.copy(observations = mapOf(ROUTINE_INSTANCE to changedObservation))
            assertThrows(IllegalArgumentException::class.java) {
                conflicting.settleRoutineOccurrence(intent, receipt, "2026-09-10T13:00:00Z")
            }
            assertEquals(listOf(intent), conflicting.pending)
        }
    }

    @Test fun terminalReadMustSeeReceiptTargetAndExactCapturedJournalBeforeScheduleAcknowledgment() {
        val intent = routineStateTestIntent(true)
        val beforeReceipt = routineStateTestLedger().copy(pending = listOf(intent))
        val settled = beforeReceipt.settleRoutineOccurrence(intent, routineTestMutation(), ROUTINE_NOW)
        val empty = RoutineOccurrencePage(1, emptyList(), "DWR1.synthetic-empty", false)
        assertThrows(IllegalArgumentException::class.java) { settled.installRoutineOccurrenceTerminal(settled, listOf(empty), ROUTINE_NOW) }
        assertThrows(IllegalArgumentException::class.java) { settled.installRoutineOccurrenceTerminal(settled, listOf(routineTestPage()), ROUTINE_NOW) }
        val terminal = routineTestPage().copy(changes = listOf(RoutineOccurrenceChange(2, routineTestMutation().occurrence)))
        assertThrows(IllegalArgumentException::class.java) { settled.installRoutineOccurrenceTerminal(beforeReceipt, listOf(terminal), ROUTINE_NOW) }
        assertThrows(IllegalArgumentException::class.java) { settled.acknowledgeRoutineOccurrenceRemoteScheduleCatchUp(settled) }
        val caughtUp = settled.installRoutineOccurrenceTerminal(settled, listOf(terminal), ROUTINE_NOW)
        assertTrue(caughtUp.minimumCatchUpRevisions.isEmpty())
        assertTrue(caughtUp.needsRemoteScheduleCatchUp)
        assertEquals(terminal.cursor, caughtUp.deltaCursor)
        assertThrows(IllegalArgumentException::class.java) { caughtUp.acknowledgeRoutineOccurrenceRemoteScheduleCatchUp(settled) }
        assertFalse(caughtUp.acknowledgeRoutineOccurrenceRemoteScheduleCatchUp(caughtUp).hasRecoveryCustody)
    }

    @Test fun intermediatePagesAndCrossPageRegressionCannotMintTerminalCursor() {
        val ledger = routineStateTestLedger()
        val first = routineTestPage().copy(hasMore = true)
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(first), ROUTINE_NOW) }
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(first, routineTestPage()), ROUTINE_NOW) }
        val delta = ledger.copy(deltaCursor = "DWR1.synthetic-previous")
        val repeat = routineTestPage().copy(changes = listOf(RoutineOccurrenceChange(2, routineTestSnapshot())))
        assertThrows(IllegalArgumentException::class.java) { delta.installRoutineOccurrenceTerminal(delta, listOf(first, repeat), ROUTINE_NOW) }
        assertEquals(delta.pending, delta.resetRoutineOccurrenceDeltaCursor(delta).pending)
    }

    @Test fun cacheCountEvictsOnlyUnpinnedObservations() {
        val snapshots = (100..355).map { routineStateTestInstance(it) }
        val ledger = routineStateTestLedger().copy(observations = snapshots.associate {
            it.aggregate.manifest.id to RoutineOccurrenceObservation(it, ROUTINE_NOW) }, pending = listOf(routineStateTestIntent()))
        ledger.requireValid()
        val incoming = routineStateTestInstance(356)
        val bounded = ledger.observeRoutineOccurrence(RoutineOccurrenceObservation(incoming, "2026-09-10T11:00:00Z"))
        assertEquals(256, bounded.observations.size)
        assertTrue(ROUTINE_INSTANCE in bounded.observations)
        assertFalse(routineStateTestId(101) in bounded.observations)
        assertTrue(incoming.aggregate.manifest.id in bounded.observations)
        assertEquals(ledger.pending, bounded.pending)
    }

    @Test fun aggregateMemberBudgetProtectsPendingAndReceiptPinnedObservations() {
        val first = routineStateTestInstance(100, 7_001)
        val second = routineStateTestInstance(101, 7_001)
        val original = routineStateTestLedger(first)
        val bounded = original.observeRoutineOccurrence(RoutineOccurrenceObservation(second, "2026-09-10T11:00:00Z"))
        assertEquals(setOf(second.aggregate.manifest.id), bounded.observations.keys)
        val pinned = original.copy(pending = listOf(routineStateTestIntent(snapshot = first)))
        val retained = pinned.observeRoutineOccurrence(RoutineOccurrenceObservation(second, "2026-09-10T11:00:00Z"))
        assertEquals(pinned, retained)
        val third = routineStateTestInstance(102, 7_001)
        val invalid = pinned.copy(observations = pinned.observations +
            (second.aggregate.manifest.id to RoutineOccurrenceObservation(second, ROUTINE_NOW)) +
            (third.aggregate.manifest.id to RoutineOccurrenceObservation(third, ROUTINE_NOW)),
            pending = pinned.pending + routineStateTestIntent(snapshot = second, operation = routineStateTestId(201)))
        assertThrows(IllegalArgumentException::class.java) { invalid.requireValid() }
        val receipt = original.copy(observations = mapOf(first.aggregate.manifest.id to RoutineOccurrenceObservation(
            first.copy(aggregate = first.aggregate.copy(revision = 2)), ROUTINE_NOW)), minimumCatchUpRevisions = mapOf(first.aggregate.manifest.id to 2L), needsRemoteScheduleCatchUp = true)
        assertEquals(receipt, receipt.observeRoutineOccurrence(RoutineOccurrenceObservation(second, "2026-09-10T11:00:00Z")))
    }

    @Test fun aggregateSerializedByteBudgetEvictsCacheButNeverIntent() {
        val first = routineStateTestInstance(100, 1_800, "😀".repeat(500))
        val second = routineStateTestInstance(101, 1_800, "😀".repeat(500))
        first.requireValid(); second.requireValid()
        val pinned = routineStateTestLedger(first).copy(pending = listOf(routineStateTestIntent(snapshot = first)))
        val bounded = pinned.observeRoutineOccurrence(RoutineOccurrenceObservation(second, "2026-09-10T11:00:00Z"))
        assertEquals(pinned, bounded)
        assertEquals(pinned.pending.single().requestJson, bounded.pending.single().requestJson)
        assertThrows(IllegalArgumentException::class.java) { pinned.copy(observations = pinned.observations +
            (second.aggregate.manifest.id to RoutineOccurrenceObservation(second, ROUTINE_NOW))).requireValid() }
    }

    @Test fun sixtyFourFrozenRequestsFitExactOneMiBBudgetAndSixtyFifthIsRejected() {
        val snapshots = (100..163).map { routineStateTestInstance(it) }
        val intents = snapshots.mapIndexed { index, snapshot -> routineStateTestIntent(snapshot = snapshot,
            operation = routineStateTestId(1_000 + index)).let { it.copy(requestJson = it.requestJson + " ".repeat(
                MAX_ROUTINE_OCCURRENCE_REQUEST_BYTES - it.requestJson.toByteArray(Charsets.UTF_8).size)) } }
        val ledger = routineStateTestLedger().copy(observations = snapshots.associate {
            it.aggregate.manifest.id to RoutineOccurrenceObservation(it, ROUTINE_NOW) }, pending = intents)
        ledger.requireValid()
        assertEquals(MAX_ROUTINE_OCCURRENCE_PENDING_BYTES, intents.sumOf { it.requestJson.toByteArray(Charsets.UTF_8).size })
        assertThrows(IllegalArgumentException::class.java) { intents.first().copy(requestJson = intents.first().requestJson + " ").requireValid() }
        val extra = routineStateTestInstance(164)
        val observed = ledger.observeRoutineOccurrence(RoutineOccurrenceObservation(extra, ROUTINE_NOW))
        assertThrows(IllegalArgumentException::class.java) { observed.enqueueRoutineOccurrence(routineStateTestIntent(snapshot = extra, operation = routineStateTestId(1_064))) }
        assertEquals(intents, observed.pending)
    }

    @Test fun unresolvedSubmittedWriteBlocksDiscardQuarantineAndRebinding() {
        val intent = routineStateTestIntent(true)
        val ledger = routineStateTestLedger().copy(pending = listOf(intent))
        assertThrows(IllegalArgumentException::class.java) { ledger.discardRoutineOccurrence(intent) }
        assertThrows(IllegalArgumentException::class.java) { ledger.quarantineRoutineOccurrences() }
        val rejected = ledger.resolveRoutineOccurrence(intent, RoutineOccurrenceDisposition.REJECTED)
        assertTrue(rejected.hasRecoveryCustody)
        val empty = rejected.discardRoutineOccurrence(rejected.pending.single()).quarantineRoutineOccurrences()
        assertEquals(RoutineOccurrenceLedger(), empty)
        assertEquals("new-configuration", empty.bindRoutineOccurrences(PROGRESS_ORIGIN, "new-configuration").configurationId)
    }

    @Test fun terminalModeStartingCheckpointAndCursorCyclesAreFenced() {
        val ledger = routineStateTestLedger().copy(deltaCursor = "DWR1.synthetic-start")
        val terminal = routineTestPage().copy(cursor = "DWR1.synthetic-next")
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(terminal), ROUTINE_NOW, isCurrentList = true) }
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(terminal), ROUTINE_NOW, startingCursor = "DWR1.wrong-start") }
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(terminal.copy(cursor = requireNotNull(ledger.deltaCursor))), ROUTINE_NOW) }
        val next = terminal.copy(changes = listOf(RoutineOccurrenceChange(2, routineTestSnapshot(true))))
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(terminal.copy(hasMore = true), next), ROUTINE_NOW) }
        val empty = terminal.copy(changes = emptyList(), cursor = requireNotNull(ledger.deltaCursor))
        assertEquals(ledger, ledger.installRoutineOccurrenceTerminal(ledger, listOf(empty), ROUTINE_NOW))
    }

    @Test fun terminalCursorAdvanceRequiresScheduleRefreshEvenWithoutMemberChanges() {
        val ledger = routineStateTestLedger().copy(deltaCursor = "DWR1.synthetic-start")
        val changedEmpty = RoutineOccurrencePage(1, emptyList(), "DWR1.synthetic-next", false)
        val changed = ledger.installRoutineOccurrenceTerminal(ledger, listOf(changedEmpty), ROUTINE_NOW)
        assertEquals(ledger.observations, changed.observations)
        assertTrue(changed.needsRemoteScheduleCatchUp)
        val unchangedMembers = routineTestPage().copy(cursor = changedEmpty.cursor)
        assertTrue(ledger.installRoutineOccurrenceTerminal(ledger, listOf(unchangedMembers), ROUTINE_NOW).needsRemoteScheduleCatchUp)

        val unchangedEmpty = changedEmpty.copy(cursor = requireNotNull(ledger.deltaCursor))
        assertFalse(ledger.installRoutineOccurrenceTerminal(ledger, listOf(unchangedEmpty), ROUTINE_NOW).needsRemoteScheduleCatchUp)
        val alreadyDirty = ledger.copy(needsRemoteScheduleCatchUp = true)
        assertTrue(alreadyDirty.installRoutineOccurrenceTerminal(alreadyDirty, listOf(unchangedEmpty), ROUTINE_NOW).needsRemoteScheduleCatchUp)
    }

    @Test fun entireTerminalChainIsBoundedBeforeMergingItsObservations() {
        val ledger = routineStateTestLedger()
        val tooManyPages = List(MAX_ROUTINE_OCCURRENCE_TERMINAL_PAGES + 1) { index ->
            routineTestPage().copy(cursor = "DWR1.synthetic-$index", hasMore = index < MAX_ROUTINE_OCCURRENCE_TERMINAL_PAGES)
        }
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, tooManyPages, ROUTINE_NOW) }
        val memberPages = (0..3).map { page -> RoutineOccurrencePage(1, (0..49).map { index ->
            val ordinal = page * 50 + index
            RoutineOccurrenceChange(ordinal + 1L, routineStateTestInstance(100 + ordinal, 201))
        }, "DWR1.members-$page", page < 3) }
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, memberPages, ROUTINE_NOW) }
        val large = routineStateTestInstance(100, 1_800, "😀".repeat(500))
        val bytePages = (0..7).map { page -> RoutineOccurrencePage(1, listOf(RoutineOccurrenceChange(page + 1L,
            large.copy(aggregate = large.aggregate.copy(manifest = large.aggregate.manifest.copy(id = routineStateTestId(100 + page)))))),
            "DWR1.bytes-$page", page < 7) }
        assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, bytePages, ROUTINE_NOW) }
        assertEquals(setOf(ROUTINE_INSTANCE), ledger.observations.keys)
        assertNull(ledger.deltaCursor)
    }

    @Test fun oversizedLazyPageAndMemberListsAreRejectedBeforeAccessOrEncoding() {
        val ledger = routineStateTestLedger()
        val changes = object : AbstractList<RoutineOccurrenceChange>() {
            override val size: Int = Int.MAX_VALUE
            override fun get(index: Int): RoutineOccurrenceChange = throw AssertionError("Oversized page must not be scanned")
        }
        assertThrows(IllegalArgumentException::class.java) {
            ledger.installRoutineOccurrenceTerminal(ledger, listOf(routineTestPage().copy(changes = changes)), ROUTINE_NOW)
        }
        val definitions = object : AbstractList<RoutineOccurrenceMemberDefinition>() {
            override val size: Int = MAX_ROUTINE_OCCURRENCE_MEMBERS + 1
            override fun get(index: Int): RoutineOccurrenceMemberDefinition = throw AssertionError("Oversized members must not be encoded")
        }
        val snapshot = routineTestSnapshot()
        val oversized = snapshot.copy(aggregate = snapshot.aggregate.copy(manifest = snapshot.aggregate.manifest.copy(members = definitions)))
        val missing = snapshot.copy(aggregate = snapshot.aggregate.copy(manifest = snapshot.aggregate.manifest.copy(members = emptyList())))
        val mismatchedStates = snapshot.copy(aggregate = snapshot.aggregate.copy(members = emptyList()))
        val mismatchedEvaluations = snapshot.copy(members = emptyList())
        for (invalid in listOf(oversized, missing, mismatchedStates, mismatchedEvaluations)) {
            val page = routineTestPage().copy(changes = listOf(RoutineOccurrenceChange(1, invalid)))
            assertThrows(IllegalArgumentException::class.java) { ledger.installRoutineOccurrenceTerminal(ledger, listOf(page), ROUTINE_NOW) }
        }
        assertNull(ledger.deltaCursor)
        assertEquals(setOf(ROUTINE_INSTANCE), ledger.observations.keys)
    }
}
