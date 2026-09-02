package com.greengolddog.dayweave

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PendingSchedulePublication
import com.greengolddog.dayweave.network.SchedulePublishHttpRequest
import com.greengolddog.dayweave.sync.CanonicalRefreshOutcome
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

class CanonicalStateOrchestrationTest {
    @Test
    fun retainedAuthoringConflictDoesNotBlockReadOnlyReplicaRecovery() {
        val conflicted = PendingCanonicalAuthoringMutation(
            id = "11111111-1111-4111-8111-111111111111",
            itemId = "22222222-2222-4222-8222-222222222222",
            operation = CanonicalAuthoringOperation.TRASH,
            expectedRevision = 3,
            createdAt = "2026-08-30T10:00:00Z",
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "Review this remote conflict",
        ).also(PendingCanonicalAuthoringMutation::requireValid)

        val state = DayWeaveUiState(
            pendingCanonicalAuthoringMutations = listOf(conflicted),
        )

        assertEquals(false, state.requiresStartupWriteRecovery())
        assertEquals(
            true,
            state.copy(
                pendingCanonicalAuthoringMutations = listOf(
                    conflicted.copy(
                        disposition = CanonicalAuthoringDisposition.PENDING,
                        diagnostic = null,
                    ),
                ),
            ).requiresStartupWriteRecovery(),
        )
    }

    @Test
    fun restoredPendingPublicationRunsExactWriteRecoveryBeforeReplicaGet() = runBlocking {
        val calls = mutableListOf<String>()
        val restored = DayWeaveUiState(
            pendingSchedulePublication = PendingSchedulePublication(
                schemaVersion = 1,
                idempotencyKey = "33333333-3333-4333-8333-333333333333",
                syncOrigin = "https://api.example.test/",
                configurationId = "connection-1",
                preparedAt = "2026-09-01T07:00:00Z",
                request = SchedulePublishHttpRequest(
                    url = "https://api.example.test/v1/schedule/publish",
                    method = "POST",
                    acceptHeader = "application/json",
                    contentTypeHeader = "application/json; charset=utf-8",
                    cacheControlHeader = "no-store",
                    pragmaHeader = "no-cache",
                    bodyJson = "{}",
                    bodySha256 = "sha256:${"a".repeat(64)}",
                ),
                candidate = CanonicalPlanUpdate(
                    items = emptyList(),
                    schedule = emptyList(),
                    syncOrigin = "https://api.example.test/",
                    configurationId = "connection-1",
                    deltaCursor = "cursor-1",
                    inputDigest = "sha256:${"b".repeat(64)}",
                    generatedAt = "2026-09-01T07:00:00Z",
                    planningZoneId = "UTC",
                    rejectedItemCount = 0,
                    unscheduledItemCount = 0,
                    protectedFreeMinutes = 0,
                    dayScore = 100,
                    violationMessages = emptyList(),
                    violationCount = 0,
                    errorViolationCount = 0,
                    unscheduledWork = emptyList(),
                    occurrenceSeriesItemIds = emptyMap(),
                    message = "Pending publication",
                ),
            ),
        )

        val outcome = recoverCurrentPublishedScheduleSequence(
            requiresWriteRecovery = restored.requiresStartupWriteRecovery(),
            canonicalWriteRecovery = {
                calls += "replay-pending-publication"
                CanonicalRefreshOutcome.SUCCESS
            },
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            replicaRefresh = {
                calls += "current-schedule-get"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(listOf("replay-pending-publication"), calls)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
    }

    @Test
    fun cleanStartupBracketsReadOnlyReplicaInstallWithExecutionTruth() = runBlocking {
        val calls = mutableListOf<String>()

        val outcome = recoverCurrentPublishedScheduleSequence(
            requiresWriteRecovery = false,
            canonicalWriteRecovery = {
                calls += "write-recovery"
                CanonicalRefreshOutcome.SUCCESS
            },
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            replicaRefresh = {
                calls += "current-schedule-get"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(listOf("execution", "current-schedule-get", "execution"), calls)
        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
    }

    @Test
    fun executionTruthBracketsTerminalProjectionAndComposition() = runBlocking {
        val calls = mutableListOf<String>()
        var executionReads = 0

        val outcome = refreshCanonicalStateSequence(
            executionRefresh = {
                executionReads += 1
                calls += "execution-$executionReads"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefresh = {
                calls += "canonical-projection-compose"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(
            listOf("execution-1", "canonical-projection-compose", "execution-2"),
            calls,
        )
        assertEquals(CanonicalRefreshOutcome.SUCCESS, outcome)
    }

    @Test
    fun failedInitialExecutionReadNeverComposesOverUnknownTruth() = runBlocking {
        val calls = mutableListOf<String>()

        val outcome = refreshCanonicalStateSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE
            },
            canonicalRefresh = {
                calls += "canonical"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(listOf("execution"), calls)
        assertEquals(null, outcome)
    }

    @Test
    fun foregroundCrossDeviceCompletionProjectsThenRechecksExecution() = runBlocking {
        val calls = mutableListOf<String>()
        var projectionNeeded = true

        refreshForegroundExecutionSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefreshNeeded = { projectionNeeded },
            canonicalRefresh = {
                calls += "canonical-projection-compose"
                projectionNeeded = false
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(
            listOf("execution", "canonical-projection-compose", "execution"),
            calls,
        )
    }

    @Test
    fun steadyForegroundPollDoesNotBlindlyRecompose() = runBlocking {
        val calls = mutableListOf<String>()

        refreshForegroundExecutionSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefreshNeeded = { false },
            canonicalRefresh = {
                calls += "canonical"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(listOf("execution"), calls)
    }

    @Test
    fun foregroundCrossDeviceDeferRecomposesAndPublishesThenRechecksExecution() = runBlocking {
        val calls = mutableListOf<String>()
        var deferredSourceStillPublished = true

        refreshForegroundExecutionSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefreshNeeded = { deferredSourceStillPublished },
            canonicalRefresh = {
                calls += "canonical-compose-publish"
                deferredSourceStillPublished = false
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(
            listOf("execution", "canonical-compose-publish", "execution"),
            calls,
        )
    }
}
