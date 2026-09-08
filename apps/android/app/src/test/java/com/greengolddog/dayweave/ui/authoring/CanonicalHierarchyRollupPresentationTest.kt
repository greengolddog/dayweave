package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.TerminalExecutionOutcomeSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HierarchyRollupTotals
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.PendingProposalApplicationMutation
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.network.ProposalApplicationHttpRequest
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.ui.screens.hierarchySourceKey
import com.greengolddog.dayweave.ui.screens.hierarchyStoredDurationLabel
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalHierarchyRollupPresentationTest {
    @Test
    fun completeAdmittedCacheIsRequiredButConnectivityIsNot() {
        val state = state(item(1))
        assertTrue(CanonicalHierarchyRollupPresentation.build(state)[id(1)] is HierarchyRollupDisplay.Available)
        listOf(state.copy(canonicalConfigurationId = null), state.copy(canonicalSyncOrigin = null),
            state.copy(canonicalDeltaCursor = null), state.copy(canonicalDeltaCursor = ""),
        ).forEach { assertEquals(HierarchyRollupDisplay.Incomplete, CanonicalHierarchyRollupPresentation.build(it)[id(1)]) }
    }

    @Test
    fun sensitiveDescendantConcealsAncestorsWithoutConcealingUnrelatedPublicBranch() {
        val state = state(item(1, kind = "goal"), item(2, 1).copy(isSensitive = true), item(3, 1), item(4))
        val result = CanonicalHierarchyRollupPresentation.build(state)
        assertEquals(HierarchyRollupDisplay.Sensitive, result[id(1)])
        assertEquals(HierarchyRollupDisplay.Sensitive, result[id(2)])
        assertTrue(result[id(3)] is HierarchyRollupDisplay.Available)
        assertTrue(result[id(4)] is HierarchyRollupDisplay.Available)
        assertFalse(result[id(1)].summaryLabel().any(Char::isDigit))
    }

    @Test
    fun everyAuthoringJournalIncludingPrivacyDowngradeMoveAndTrashWithholdsOldAndNewAncestors() {
        val sensitive = item(2, 1).copy(isSensitive = true)
        val state = state(item(1, kind = "goal"), sensitive, item(3, kind = "project"))
        for (operation in CanonicalAuthoringOperation.entries) {
            val mutation = PendingCanonicalAuthoringMutation(
                id = id(10), itemId = sensitive.id, operation = operation,
                draft = if (operation in setOf(CanonicalAuthoringOperation.CREATE, CanonicalAuthoringOperation.REPLACE)) {
                    sensitive.toCanonicalDraft().copy(parentId = id(3), isSensitive = false)
                } else null,
                expectedRevision = if (operation == CanonicalAuthoringOperation.CREATE) null else 1,
                baseItem = if (operation == CanonicalAuthoringOperation.CREATE) null else sensitive,
                createdAt = NOW,
            )
            val result = CanonicalHierarchyRollupPresentation.build(state.copy(pendingCanonicalAuthoringMutations = listOf(mutation)))
            for (id in listOf(id(1), id(2), id(3))) assertEquals(HierarchyRollupDisplay.Pending, result[id])
        }
    }

    @Test
    fun queuedStatusIsNotClaimedAsRecordedCompletion() {
        val state = state(item(1)).copy(pendingCanonicalMutation = PendingCanonicalMutation(
            idempotencyKey = "synthetic-status", syncOrigin = ORIGIN, configurationId = "binding",
            itemId = id(1), expectedRevision = 1, targetStatus = "completed", startedAt = NOW,
            replacementRequestJson = "{}", focusedBlockId = id(4), displayStatus = ItemStatus.COMPLETED,
        ))
        assertEquals(HierarchyRollupDisplay.Pending, CanonicalHierarchyRollupPresentation.build(state)[id(1)])
    }

    @Test
    fun ambiguousProposalAndExecutionCommandsInvalidateBothAdmissionAndCacheKey() {
        val original = state(item(1))
        val proposal = PendingProposalApplicationMutation(schemaVersion = 1, kind = ProposalApplicationMutationKind.APPLY,
            idempotencyKey = "synthetic-complete-proposal", syncOrigin = ORIGIN, configurationId = "binding",
            proposalId = id(20), expectedProposalRevision = 1, expectedCommandIds = listOf(id(21)), preparedAt = NOW,
            request = ProposalApplicationHttpRequest("${ORIGIN}v1/proposals/${id(20)}/apply", "POST", "application/json",
                "application/json", "no-store", "no-cache", "{\"type\":\"complete_item\"}", "synthetic-hash"))
        val command = PendingExecutionCommand(idempotencyKey = "synthetic-complete-execution", syncOrigin = ORIGIN,
            configurationId = "binding", expectedRevision = 1, sessionId = id(30), itemId = id(1), itemRevision = 1,
            sessionIndex = 0, commandType = "complete", requestJson = "{\"type\":\"complete\"}", focusedBlockId = id(31), startedAt = NOW)
        for (changed in listOf(original.copy(pendingProposalApplicationMutation = proposal),
            original.copy(pendingExecutionCommand = command),
        )) {
            assertEquals(HierarchyRollupDisplay.Pending, CanonicalHierarchyRollupPresentation.build(changed)[id(1)])
            assertNotEquals(hierarchySourceKey(original), hierarchySourceKey(changed))
        }
    }

    @Test
    fun terminalProjectionWaitsForItsExactRevisionInCanonicalItemOrRetainedTombstone() {
        val original = state(item(1, kind = "goal"), item(2, 1))
        val receipt = terminalReceipt().copy(canonicalProjectionRevision = 2)
        val lagging = original.copy(terminalExecutionOutcomes = mapOf(receipt.session.id to receipt))
        assertEquals(HierarchyRollupDisplay.Pending, CanonicalHierarchyRollupPresentation.build(lagging)[id(1)])
        assertNotEquals(hierarchySourceKey(original), hierarchySourceKey(lagging))
        val caughtUp = lagging.copy(canonicalItems = listOf(item(1, kind = "goal"), item(2, 1).copy(status = "completed", revision = 2)))
        assertEquals(1L, available(caughtUp, 1).completedLeafItems)
        assertNotEquals(hierarchySourceKey(lagging), hierarchySourceKey(caughtUp))
        val absent = lagging.copy(canonicalItems = listOf(item(1, kind = "goal")))
        assertEquals(HierarchyRollupTotals(openLeafItems = 1), available(absent, 1))
        val tombstone = absent.copy(canonicalRecentlyDeleted = listOf(CanonicalRecentlyDeletedRecord(
            id = id(2), revision = 2, deletedAt = NOW, parentId = id(1),
        )))
        assertEquals(HierarchyRollupTotals(openLeafItems = 1), available(tombstone, 1))
        assertNotEquals(hierarchySourceKey(absent), hierarchySourceKey(tombstone))
        val staleTombstone = tombstone.copy(canonicalRecentlyDeleted = tombstone.canonicalRecentlyDeleted.map { it.copy(revision = 1) })
        assertEquals(HierarchyRollupDisplay.Pending, CanonicalHierarchyRollupPresentation.build(staleTombstone)[id(1)])
    }

    @Test
    fun unresolvedProjectionWithholdsButExplicitKeepLatestAndOccurrenceOnlyDoNotInventCompletion() {
        val original = state(item(1, kind = "goal"), item(2, 1))
        val receipt = terminalReceipt()
        val pending = original.copy(terminalExecutionOutcomes = mapOf(receipt.session.id to receipt))
        assertEquals(HierarchyRollupDisplay.Pending, CanonicalHierarchyRollupPresentation.build(pending)[id(1)])
        for (resolved in listOf(receipt.copy(canonicalProjectionResolution = "user_kept_latest_item"),
            receipt.copy(requiresCanonicalItemProjection = false),
        )) {
            val state = original.copy(terminalExecutionOutcomes = mapOf(resolved.session.id to resolved))
            assertEquals(0L, available(state, 1).completedLeafItems)
            assertEquals(1L, available(state, 1).openLeafItems)
        }
    }

    @Test
    fun trashIsExcludedButItsSurvivingChildMakesTopologyUnavailable() {
        val deleted = item(2, 1).copy(deletedAt = NOW, isSensitive = true)
        val clean = state(item(1, kind = "goal"), deleted)
        assertEquals(HierarchyRollupTotals(openLeafItems = 1), available(clean, 1))
        val broken = clean.copy(canonicalItems = clean.canonicalItems + item(3, 2))
        assertEquals(HierarchyRollupDisplay.Invalid, CanonicalHierarchyRollupPresentation.build(broken)[id(1)])
    }

    @Test
    fun leafPolicyIgnoresStaleExecutableFlagsAndNeverDuplicatesParentEffort() {
        val result = state(item(1, kind = "goal").copy(hasOwnEffort = true),
            item(2, 1).copy(isExecutable = false), item(3, kind = "project").copy(hasOwnEffort = true),
            item(4, kind = "routine"))
        assertEquals(30L, available(result, 1).expectedEstimateSeconds)
        assertEquals(30L, available(result, 3).expectedEstimateSeconds)
        assertEquals(0L, available(result, 4).expectedEstimateSeconds)
    }

    @Test
    fun subminuteRangeStaysExactAndUnknownIsNotZeroWork() {
        val range = item(2, 1).copy(durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 1, durationSeconds = 30, durationMaxSeconds = 59)
        val unknown = item(3, 1).copy(durationKind = CanonicalDurationKind.UNKNOWN,
            durationMinSeconds = null, durationSeconds = null, durationMaxSeconds = null, durationSource = null)
        val result = available(state(item(1, kind = "goal"), range, unknown), 1)
        assertEquals(HierarchyRollupTotals(openLeafItems = 2, unknownEstimates = 1,
            minimumEstimateSeconds = 1, expectedEstimateSeconds = 30, maximumEstimateSeconds = 59), result)
        assertTrue(result.detailLabels().any { it.contains("30s expected · 1s minimum · 59s maximum") })
        assertTrue(result.detailLabels().any { it.contains("Unknown leaf effort estimates · 1") })
    }

    @Test
    fun unknownAndMalformedCanonicalFieldsNeverProduceTrustedZeros() {
        val item = item(1)
        listOf(item.copy(id = "not-an-id"), item.copy(id = "ABCDEF00-0000-4000-8000-000000000001"),
            item.copy(status = "future"), item.copy(kind = "future"),
            item.copy(durationKind = CanonicalDurationKind.UNKNOWN), item.copy(durationMaxSeconds = 31),
            item.copy(parentId = item.id),
        ).forEach { malformed ->
            assertEquals(HierarchyRollupDisplay.Invalid, CanonicalHierarchyRollupPresentation.build(state(malformed))[malformed.id])
        }
        assertEquals(HierarchyRollupDisplay.Invalid, CanonicalHierarchyRollupPresentation.build(state(item, item))[item.id])
    }

    @Test
    fun browserSearchAndCollapseDoNotChangeCompleteForestTotalsOrRecordedParentStatus() {
        val state = state(item(1, kind = "goal"), item(2, 1).copy(status = "completed"), item(3, 1))
        val rows = CanonicalAuthoringPresentation.build(state).hierarchyRows
        val rollups = CanonicalHierarchyRollupPresentation.build(state)
        for (query in listOf("", "Item 2")) {
            val visible = CanonicalHierarchyBrowserPresentation.build(rows, ItemKind.GOAL, query, setOf(id(1)))
            assertEquals("inbox", visible.rows.first().item.status)
            assertTrue(hierarchyStoredDurationLabel(visible.rows.first().item, true).contains("excluded from leaf totals"))
            assertEquals(HierarchyRollupTotals(completedLeafItems = 1, openLeafItems = 1,
                minimumEstimateSeconds = 60, expectedEstimateSeconds = 60, maximumEstimateSeconds = 60),
                (rollups[id(1)] as HierarchyRollupDisplay.Available).totals)
        }
    }

    @Test
    fun cacheKeyExcludesClockPresentationButIncludesAuthorityGraphAndPrivacy() {
        val state = state(item(1))
        val key = hierarchySourceKey(state)
        assertEquals(key, hierarchySourceKey(state.copy(scheduleMessage = "tick", showCompleted = !state.showCompleted)))
        for (changed in listOf(state.copy(canonicalDeltaCursor = null), state.copy(canonicalSyncOrigin = null),
            state.copy(canonicalConfigurationId = "other"),
            state.copy(canonicalItems = listOf(item(1).copy(isSensitive = true))),
            state.copy(canonicalItems = listOf(item(1).copy(status = "completed"))),
        )) assertNotEquals(key, hierarchySourceKey(changed))
    }

    @Test
    fun fullFiveThousandNodePrivacyAndSummaryProjectionIsIterative() {
        val nodes = (1..5_000).map { item(it, if (it == 1) null else it - 1, "goal") }
        val result = CanonicalHierarchyRollupPresentation.build(state(*nodes.toTypedArray()))
        assertEquals(HierarchyRollupTotals(openLeafItems = 1), (result[id(1)] as HierarchyRollupDisplay.Available).totals)
        val privateNodes = nodes.dropLast(1) + nodes.last().copy(isSensitive = true)
        val concealed = CanonicalHierarchyRollupPresentation.build(state(*privateNodes.toTypedArray()))
        assertEquals(HierarchyRollupDisplay.Sensitive, concealed[id(1)])
        assertEquals(HierarchyRollupDisplay.Sensitive, concealed[id(5_000)])
    }

    private fun available(state: DayWeaveUiState, value: Int) =
        (CanonicalHierarchyRollupPresentation.build(state)[id(value)] as HierarchyRollupDisplay.Available).totals

    private fun state(vararg items: CanonicalItemSnapshot) = DayWeaveUiState(canonicalItems = items.toList(),
        canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = "binding", canonicalDeltaCursor = "cursor")

    private fun item(value: Int, parent: Int? = null, kind: String = "task") = CanonicalItemSnapshot(
        id = id(value), parentId = parent?.let(::id), kind = kind, status = "inbox", title = "Item $value",
        timezoneName = "UTC", durationSeconds = 30, flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50, urgency = 50, siblingOrder = 0, isExecutable = true, revision = 1, createdAt = NOW, updatedAt = NOW,
    )

    private fun id(value: Int) = UUID(0, value.toLong()).toString()

    private fun terminalReceipt() = TerminalExecutionOutcomeSnapshot(syncOrigin = ORIGIN,
        session = CanonicalExecutionSessionSnapshot(id = id(50), itemId = id(2), itemRevision = 1,
            sessionIndex = 0, sourceDeviceId = id(51), status = "completed", revision = 2,
            accumulatedSeconds = 30, actualSeconds = 30, startedAt = NOW, endedAt = NOW, createdAt = NOW, updatedAt = NOW),
        requiresCanonicalItemProjection = true, recordedAt = NOW)

    private companion object { const val NOW = "2026-09-09T10:00:00Z"; const val ORIGIN = "https://example.test/" }
}
