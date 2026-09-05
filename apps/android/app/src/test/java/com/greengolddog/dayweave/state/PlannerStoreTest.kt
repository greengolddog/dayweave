package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceEvidenceSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeCommandSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.PendingExecutionDeferIntent
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import com.greengolddog.dayweave.model.PendingSchedulePublication
import com.greengolddog.dayweave.model.PendingProposalApplicationMutation
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.model.ProposalApplicationReceiptSnapshot
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionHintSnapshot
import com.greengolddog.dayweave.model.RecurrenceMoveSnapshot
import com.greengolddog.dayweave.model.RecurrenceOccurrenceSourceSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.MAX_SCHEDULE_PUBLISH_BODY_BYTES
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SchedulePublishRequest
import com.greengolddog.dayweave.network.buildSchedulePublishHttpRequest
import com.greengolddog.dayweave.network.plannerSha256
import com.greengolddog.dayweave.network.prepareProposalApplyHttpRequest
import com.greengolddog.dayweave.network.prepareProposalUndoHttpRequest
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.ZoneId
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PlannerStoreTest {
    @Test
    fun assistantTurnsPersistOnlyBoundedRealMessagesAndRequireAUserAnchor() = runBlocking {
        val store = PlannerStore(DayWeaveUiState())
        val userId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val assistantId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"

        assertTrue(
            requireNotNull(store.appendAssistantUserMessageDurably(userId, "  Help plan today  "))
                .awaitDurable(),
        )
        assertEquals(
            ChatMessage(userId, ChatRole.USER, "Help plan today"),
            store.state.value.messages.single(),
        )
        assertTrue(
            requireNotNull(
                store.appendAssistantReplyDurably(userId, assistantId, "  Start with focus.  "),
            ).awaitDurable(),
        )
        assertEquals(
            listOf(ChatRole.USER, ChatRole.ASSISTANT),
            store.state.value.messages.map(ChatMessage::role),
        )
        assertThrows(IllegalArgumentException::class.java) {
            store.appendAssistantReplyDurably(
                "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
                "Orphan reply",
            )
        }
        assertFalse(store.sendAssistantMessage(" "))
        assertFalse(store.sendAssistantMessage("x".repeat(8 * 1024 + 1)))
        assertFalse(store.sendAssistantMessage("spoofed\u202Etext"))
        assertFalse(store.sendAssistantMessage("unpaired \uD800 surrogate"))
        assertTrue(store.sendAssistantMessage("Valid emoji 😀"))
        assertEquals("Valid emoji 😀", store.state.value.messages.last().text)
    }

    @Test
    fun assistantTranscriptRestoreDropsInvalidRowsAndKeepsNewestBudget() {
        val oversized = "x".repeat(32 * 1024 + 1)
        val messages = buildList {
            add(ChatMessage("", ChatRole.USER, "bad id"))
            add(ChatMessage("oversized", ChatRole.ASSISTANT, oversized))
            add(ChatMessage("directional", ChatRole.USER, "spoofed\u202Etext"))
            add(ChatMessage("surrogate", ChatRole.USER, "unpaired \uD800 surrogate"))
            repeat(205) { index ->
                add(ChatMessage("message-$index", ChatRole.USER, "turn $index"))
            }
            add(ChatMessage("message-204", ChatRole.USER, "newest duplicate wins"))
        }

        val store = PlannerStore(DayWeaveUiState(messages = messages))

        assertEquals(200, store.state.value.messages.size)
        assertEquals("message-5", store.state.value.messages.first().id)
        assertEquals("newest duplicate wins", store.state.value.messages.last().text)
        assertTrue(
            store.state.value.messages.none {
                it.id.isBlank() || it.text == oversized ||
                    it.id == "directional" || it.id == "surrogate"
            },
        )
    }

    @Test
    fun proposalApplyAndUndoUseExactAtomicJournalWithoutManufacturingDraft() {
        val proposalId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val commandId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        val applicationId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        val affectedItemId = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        val configuration = AuthenticatedApiConfiguration.createBound(
            CANONICAL_ORIGIN,
            "synthetic-token",
            "connection-1",
        )
        val suggestion = PlanningSuggestion(
            id = proposalId,
            title = "Create focused task",
            summary = "One exact task creation",
            source = "Codex",
            kind = SuggestionKind.NEW_TASK,
            expiresInDays = 30,
            remoteRevision = 1,
            remotePayloadSchema = "dayweave.proposal-change-set/1",
            remoteExpiresAt = "2099-01-01T00:00:00Z",
        )
        val store = PlannerStore(
            publishedCanonicalState().copy(
                suggestions = listOf(suggestion),
                inbox = listOf(
                    InboxItem(
                        id = "proposal-$proposalId",
                        title = "Legacy draft must disappear",
                        source = InboxSource.EXTERNAL_PROPOSAL,
                    ),
                ),
            ),
        )
        val reviewHash = "sha256:${"a".repeat(64)}"
        val apply = PendingProposalApplicationMutation(
            schemaVersion = 1,
            kind = ProposalApplicationMutationKind.APPLY,
            idempotencyKey = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            proposalId = proposalId,
            expectedProposalRevision = 1,
            expectedCommandIds = listOf(commandId),
            previewId = "ffffffff-ffff-4fff-8fff-ffffffffffff",
            expectedReviewHash = reviewHash,
            preparedAt = "2026-08-30T10:00:00Z",
            request = prepareProposalApplyHttpRequest(
                configuration,
                "ffffffff-ffff-4fff-8fff-ffffffffffff",
                reviewHash,
            ),
        )
        val applied = ProposalApplicationReceiptSnapshot(
            schemaVersion = 1,
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            applicationId = applicationId,
            proposalId = proposalId,
            appliedProposalRevision = 2,
            applicationRevision = 1,
            status = ProposalApplicationStatusSnapshot.APPLIED,
            commandIds = listOf(commandId),
            affectedItemIds = listOf(affectedItemId),
            appliedAt = "2026-08-30T10:01:00Z",
            undoExpiresAt = "2099-01-01T00:00:00Z",
        )

        assertNotNull(store.stageProposalApplicationMutation(apply))
        assertTrue(store.hasCredentialReplacementBlocker())
        assertNotNull(store.commitProposalApplicationMutation(apply, applied))
        assertNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(
            SuggestionDisposition.TRANSACTIONALLY_APPLIED,
            store.state.value.suggestions.single().disposition,
        )
        assertTrue(store.state.value.inbox.none { it.id == "proposal-$proposalId" })
        assertPublishedPlanInvalidated(store)

        val undo = PendingProposalApplicationMutation(
            schemaVersion = 1,
            kind = ProposalApplicationMutationKind.UNDO,
            idempotencyKey = "12121212-1212-4212-8212-121212121212",
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            proposalId = proposalId,
            expectedProposalRevision = 2,
            expectedCommandIds = listOf(commandId),
            applicationId = applicationId,
            expectedApplicationRevision = 1,
            preparedAt = "2026-08-30T10:02:00Z",
            request = prepareProposalUndoHttpRequest(configuration, applicationId, 1),
        )
        assertNotNull(store.stageProposalApplicationMutation(undo))
        assertNotNull(
            store.commitProposalApplicationMutation(
                undo,
                applied.copy(
                    status = ProposalApplicationStatusSnapshot.UNDONE,
                    applicationRevision = 2,
                    undoneAt = "2026-08-30T10:03:00Z",
                ),
            ),
        )
        assertEquals(
            ProposalApplicationStatusSnapshot.UNDONE,
            store.state.value.proposalApplications.getValue(proposalId).status,
        )
    }

    @Test
    fun reservedTypedProposalCannotUseLegacyApprovalPath() {
        val proposal = PlanningSuggestion(
            id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            title = "Typed change",
            summary = "Must be reviewed",
            source = "ChatGPT",
            kind = SuggestionKind.SCHEDULE_CHANGE,
            expiresInDays = 1,
            remoteRevision = 1,
            remotePayloadSchema = "dayweave.proposal-change-set/2",
        )
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(proposal)))

        store.approveSuggestion(proposal.id)

        assertEquals(SuggestionDisposition.PENDING, store.state.value.suggestions.single().disposition)
        assertTrue(store.state.value.inbox.isEmpty())
    }

    @Test
    fun overLimitPublicationBodyIsRejectedBeforeStateMutationOrPersistence() = runBlocking {
        val initial = DayWeaveUiState()
        var saveCalls = 0
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveCalls += 1
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val normal = publication(
                canonicalUpdate(
                    item = canonicalItem("planned", 8),
                    block = canonicalBlock(ItemStatus.SCHEDULED, 8),
                    cursor = "cursor-1",
                ),
            )
            val normalBytes = normal.request.bodyJson.toByteArray(StandardCharsets.UTF_8).size
            val overLimitBody = normal.request.bodyJson +
                " ".repeat(MAX_SCHEDULE_PUBLISH_BODY_BYTES + 1 - normalBytes)
            assertEquals(
                MAX_SCHEDULE_PUBLISH_BODY_BYTES + 1,
                overLimitBody.toByteArray(StandardCharsets.UTF_8).size,
            )
            val overLimit = normal.copy(
                request = normal.request.copy(
                    bodyJson = overLimitBody,
                    bodySha256 = plannerSha256(overLimitBody),
                ),
            )

            org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
                store.stageSchedulePublication(overLimit)
            }

            assertNull(store.state.value.pendingSchedulePublication)
            assertEquals(0, saveCalls)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun publicationJournalFencesCurrentPlanAndCommitIsExactAtomicCas() {
        val oldItem = canonicalItem("planned", 7)
        val oldBlock = canonicalBlock(ItemStatus.SCHEDULED, 7)
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(oldItem),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(oldBlock),
                scheduleInputDigest = "sha256:${"0".repeat(64)}",
                scheduleGeneratedAt = "1970-01-01T00:00:00Z",
                schedulePlanningZoneId = "UTC",
            ),
        )
        val candidate = canonicalUpdate(
            item = canonicalItem("planned", 8),
            block = canonicalBlock(ItemStatus.SCHEDULED, 8),
            cursor = "cursor-1",
        )
        val pending = publication(candidate)

        assertNotNull(store.stageSchedulePublication(pending))
        assertEquals("cursor-0", store.state.value.canonicalDeltaCursor)
        assertEquals(7L, store.state.value.canonicalItems.single().revision)
        assertTrue(store.hasCredentialReplacementBlocker())
        assertFalse(store.state.value.isCanonicalPlanCurrent(Instant.EPOCH, java.time.ZoneOffset.UTC))

        val revision = publishedRevision()
        assertNotNull(store.commitSchedulePublication(pending, revision, replayed = false))
        assertEquals(null, store.state.value.pendingSchedulePublication)
        assertEquals("cursor-1", store.state.value.canonicalDeltaCursor)
        assertEquals(8L, store.state.value.canonicalItems.single().revision)
        assertEquals(revision, store.state.value.publishedScheduleRevision)
        assertNotNull(store.state.value.publishedScheduleProof)
        assertTrue(store.state.value.isCanonicalPlanCurrent(Instant.EPOCH, java.time.ZoneId.of("UTC")))

        val stale = runCatching {
            store.commitSchedulePublication(pending, revision, replayed = false)
        }
        assertTrue(stale.isFailure)
        assertEquals("cursor-1", store.state.value.canonicalDeltaCursor)
    }

    @Test
    fun habitDeltaPreservesAmbiguousPublicationJournalButRevokesItsLaterAuthority() {
        val item = canonicalItem("planned", 7)
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7)
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(item),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(block),
                scheduleInputDigest = "sha256:${"0".repeat(64)}",
                scheduleGeneratedAt = "1970-01-01T00:00:00Z",
                schedulePlanningZoneId = "UTC",
            ),
        )
        store.bindHabitLedger(CANONICAL_ORIGIN, "connection-1")
        store.applyHabitDeltaPage(
            CANONICAL_ORIGIN,
            "connection-1",
            listOf(habitOccurrenceForPublication()),
            emptyList(),
            "habit_cursor_0",
        )
        val candidate = canonicalUpdate(item, block, cursor = "cursor-1")
        val pending = publication(candidate)
        assertNotNull(store.stageSchedulePublication(pending))

        store.applyHabitDeltaPage(
            CANONICAL_ORIGIN,
            "connection-1",
            listOf(
                habitOccurrenceForPublication(
                    HabitOutcomeSnapshot(
                        revision = 1,
                        status = HabitOutcomeStatusSnapshot.COMPLETED,
                        progressBasisPoints = 10_000,
                        quantity = null,
                        unit = null,
                        actualSeconds = 1_800,
                        note = null,
                        occurredAt = "1970-01-01T01:30:00Z",
                        updatedAt = "1970-01-01T01:31:00Z",
                    ),
                ),
            ),
            emptyList(),
            "habit_cursor_1",
        )

        assertEquals(pending, store.state.value.pendingSchedulePublication)
        assertTrue(store.state.value.pendingSchedulePublicationInvalidated)
        assertNotNull(store.commitSchedulePublication(pending, publishedRevision(), replayed = false))
        assertNull(store.state.value.pendingSchedulePublication)
        assertFalse(store.state.value.pendingSchedulePublicationInvalidated)
        assertNull(store.state.value.publishedScheduleRevision)
        assertNull(store.state.value.publishedScheduleProof)
        assertNull(store.state.value.scheduleInputDigest)
        assertEquals(
            HabitOutcomeStatusSnapshot.COMPLETED,
            store.state.value.habitLedger.occurrences.values.single().outcome?.status,
        )
    }

    @Test
    fun canonicalHabitToTaskTransitionPurgesOnlyHabitDerivedRecurrenceAuthority() {
        val habit = canonicalItem("planned", 7).copy(
            kind = "habit",
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(habit),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
            ),
        )
        store.bindHabitLedger(CANONICAL_ORIGIN, "connection-1")
        store.applyHabitDeltaPage(
            CANONICAL_ORIGIN,
            "connection-1",
            occurrences = listOf(
                habitOccurrenceForPublication(
                    HabitOutcomeSnapshot(
                        revision = 1,
                        status = HabitOutcomeStatusSnapshot.COMPLETED,
                        progressBasisPoints = 10_000,
                        quantity = null,
                        unit = null,
                        actualSeconds = 1_800,
                        note = null,
                        occurredAt = "1970-01-01T01:30:00Z",
                        updatedAt = "1970-01-01T01:31:00Z",
                    ),
                ),
            ),
            pauses = emptyList(),
            nextCursor = "habit_cursor_1",
        )
        assertTrue(store.state.value.recurrenceOutcomes.isNotEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isNotEmpty())

        val task = canonicalItem("planned", 8)
        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = task,
                block = canonicalBlock(ItemStatus.SCHEDULED, 8),
                cursor = "cursor-1",
            ),
        )

        assertTrue(store.state.value.habitLedger.occurrences.isNotEmpty())
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())
    }

    @Test
    fun deletedCanonicalHabitCannotBeResurrectedByLaterHabitDeltaOrWindowMerge() {
        val habit = canonicalItem("planned", 7).copy(
            kind = "habit",
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(habit),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
            ),
        )
        store.bindHabitLedger(CANONICAL_ORIGIN, "connection-1")
        val completed = habitOccurrenceForPublication(
            HabitOutcomeSnapshot(
                revision = 1,
                status = HabitOutcomeStatusSnapshot.COMPLETED,
                progressBasisPoints = 10_000,
                quantity = null,
                unit = null,
                actualSeconds = 1_800,
                note = null,
                occurredAt = "1970-01-01T01:30:00Z",
                updatedAt = "1970-01-01T01:31:00Z",
            ),
        )
        store.applyHabitDeltaPage(
            CANONICAL_ORIGIN,
            "connection-1",
            listOf(completed),
            emptyList(),
            "habit_cursor_1",
        )
        assertTrue(store.state.value.recurrenceOutcomes.isNotEmpty())

        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = habit.copy(revision = 8),
                block = canonicalBlock(ItemStatus.SCHEDULED, 8),
                cursor = "cursor-1",
            ).copy(items = emptyList(), schedule = emptyList()),
        )
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())

        val skipped = completed.copy(
            outcome = completed.outcome?.copy(
                revision = 2,
                status = HabitOutcomeStatusSnapshot.SKIPPED,
                progressBasisPoints = 0,
                actualSeconds = null,
                updatedAt = "1970-01-01T01:32:00Z",
            ),
        )
        store.applyHabitDeltaPage(
            CANONICAL_ORIGIN,
            "connection-1",
            listOf(skipped),
            emptyList(),
            "habit_cursor_2",
        )
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())

        val completedAgain = skipped.copy(
            outcome = skipped.outcome?.copy(
                revision = 3,
                status = HabitOutcomeStatusSnapshot.COMPLETED,
                progressBasisPoints = 10_000,
                actualSeconds = 1_700,
                updatedAt = "1970-01-01T01:33:00Z",
            ),
        )
        store.mergeHabitOccurrencePage(
            CANONICAL_ORIGIN,
            "connection-1",
            CANONICAL_ITEM_ID,
            listOf(completedAgain),
        )
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())
        assertEquals(3L, store.state.value.habitLedger.occurrences.values.single().outcome?.revision)
    }

    @Test
    fun canonicalHabitAdmissionProjectsLedgerRowsThatArrivedDuringBootstrap() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
            ),
        )
        store.bindHabitLedger(CANONICAL_ORIGIN, "connection-1")
        val completed = habitOccurrenceForPublication(
            HabitOutcomeSnapshot(
                revision = 1,
                status = HabitOutcomeStatusSnapshot.COMPLETED,
                progressBasisPoints = 10_000,
                quantity = null,
                unit = null,
                actualSeconds = 1_800,
                note = null,
                occurredAt = "1970-01-01T01:30:00Z",
                updatedAt = "1970-01-01T01:31:00Z",
            ),
        )
        store.applyHabitDeltaPage(
            CANONICAL_ORIGIN,
            "connection-1",
            listOf(completed),
            emptyList(),
            "habit_cursor_1",
        )
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())

        val habit = canonicalItem("planned", 7).copy(
            kind = "habit",
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = habit,
                block = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(kind = ItemKind.HABIT),
                cursor = "cursor-1",
            ),
        )

        assertEquals(
            ItemStatus.COMPLETED,
            store.state.value.recurrenceOutcomes.values.single().status,
        )
        assertEquals(
            "1970-01-01T01:30:00Z",
            store.state.value.recurrenceCompletionAnchors[CANONICAL_ITEM_ID],
        )
    }

    @Test
    fun currentScheduleReplicaInstallsExactProofThenClearsOnlyFromDurableExpectedState() =
        runBlocking {
            val initial = DayWeaveUiState(
                canonicalItems = listOf(canonicalItem("planned", 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
            )
            val store = PlannerStore(initial)
            val expected = store.state.value
            val update = canonicalUpdate(
                item = canonicalItem("planned", 8),
                block = canonicalBlock(ItemStatus.SCHEDULED, 8),
                cursor = "cursor-1",
            )

            val install = requireNotNull(
                store.installCurrentPublishedSchedule(expected, update, publishedRevision()),
            )
            assertTrue(install.awaitDurable())
            val installed = store.state.value
            assertEquals(8L, installed.canonicalItems.single().revision)
            assertEquals("cursor-1", installed.canonicalDeltaCursor)
            assertEquals(publishedRevision(), installed.publishedScheduleRevision)
            assertTrue(requireNotNull(installed.publishedScheduleProof).matchesPublishedPlan(
                installed.schedule,
            ))

            val clear = requireNotNull(
                store.installNoCurrentPublishedSchedule(
                    expectedState = installed,
                    syncOrigin = CANONICAL_ORIGIN,
                    configurationId = "connection-1",
                    epochResetFromRevision = 1uL,
                ),
            )
            assertTrue(clear.awaitDurable())
            assertNull(store.state.value.publishedScheduleRevision)
            assertNull(store.state.value.publishedScheduleProof)
            assertNull(store.state.value.scheduleInputDigest)
            assertTrue(store.state.value.schedule.isEmpty())
        }

    @Test
    fun durableHeadHintFencesOlderCurrentUnlessExactCursorEpochResetIsStillCurrent() = runBlocking {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem("planned", 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
            ),
        )
        val hintReceipt = requireNotNull(
            store.recordPublishedScheduleRevisionHint(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revisionNumber = 2uL,
            ),
        )
        assertTrue(hintReceipt.awaitDurable())
        assertNull(store.state.value.publishedOccurrenceMembershipProof)
        assertEquals(
            PublishedScheduleRevisionHintSnapshot(
                CANONICAL_ORIGIN,
                "connection-1",
                2uL,
            ),
            store.state.value.publishedScheduleRevisionHint,
        )
        val expected = store.state.value
        val update = canonicalUpdate(
            item = canonicalItem("planned", 8),
            block = canonicalBlock(ItemStatus.SCHEDULED, 8),
            cursor = "cursor-1",
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.installCurrentPublishedSchedule(expected, update, publishedRevision())
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.installCurrentPublishedSchedule(
                expectedState = expected,
                update = update,
                revision = publishedRevision(),
                epochResetFromRevision = 3uL,
            )
        }
        assertNull(store.state.value.publishedOccurrenceMembershipProof)
        assertEquals(2uL, store.state.value.publishedScheduleRevisionHint?.revisionNumber)

        val resetReceipt = requireNotNull(
            store.installCurrentPublishedSchedule(
                expectedState = expected,
                update = update,
                revision = publishedRevision(),
                epochResetFromRevision = 2uL,
            ),
        )
        assertTrue(resetReceipt.awaitDurable())
        assertEquals(1uL, store.state.value.publishedScheduleRevisionHint?.revisionNumber)
        assertEquals(
            1uL,
            store.state.value.publishedOccurrenceMembershipProof?.revision?.revisionNumber,
        )
    }

    @Test
    fun durableHeadHintFencesEmptyCurrentUnlessExactCursorEpochResetIsStillCurrent() = runBlocking {
        val store = PlannerStore(publishedCanonicalState())
        val hintReceipt = requireNotNull(
            store.recordPublishedScheduleRevisionHint(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revisionNumber = 2uL,
            ),
        )
        assertTrue(hintReceipt.awaitDurable())
        val expected = store.state.value

        assertThrows(IllegalArgumentException::class.java) {
            store.installNoCurrentPublishedSchedule(
                expected,
                CANONICAL_ORIGIN,
                "connection-1",
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            store.installNoCurrentPublishedSchedule(
                expected,
                CANONICAL_ORIGIN,
                "connection-1",
                epochResetFromRevision = 3uL,
            )
        }
        assertEquals(2uL, store.state.value.publishedScheduleRevisionHint?.revisionNumber)

        val clearReceipt = requireNotNull(
            store.installNoCurrentPublishedSchedule(
                expected,
                CANONICAL_ORIGIN,
                "connection-1",
                epochResetFromRevision = 2uL,
            ),
        )
        assertTrue(clearReceipt.awaitDurable())
        assertNull(store.state.value.publishedScheduleRevisionHint)
        assertNull(store.state.value.publishedScheduleProof)

        val raced = PlannerStore(publishedCanonicalState())
        val secondHint = requireNotNull(
            raced.recordPublishedScheduleRevisionHint(
                CANONICAL_ORIGIN,
                "connection-1",
                2uL,
            ),
        )
        assertTrue(secondHint.awaitDurable())
        val staleExpected = raced.state.value
        val thirdHint = requireNotNull(
            raced.recordPublishedScheduleRevisionHint(
                CANONICAL_ORIGIN,
                "connection-1",
                3uL,
            ),
        )
        assertTrue(thirdHint.awaitDurable())
        assertThrows(IllegalArgumentException::class.java) {
            raced.installNoCurrentPublishedSchedule(
                staleExpected,
                CANONICAL_ORIGIN,
                "connection-1",
                epochResetFromRevision = 2uL,
            )
        }
        assertEquals(3uL, raced.state.value.publishedScheduleRevisionHint?.revisionNumber)
    }

    @Test
    fun typedMissingCurrentScheduleClearsDisplayOnlyLocalComposition() = runBlocking {
        val local = DayWeaveUiState(
            canonicalItems = listOf(canonicalItem("planned", 7)),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalDeltaCursor = "cursor-0",
            schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, 7)),
            scheduleGeneratedAt = "1970-01-01T00:00:00Z",
            schedulePlanningZoneId = "UTC",
        )
        val store = PlannerStore(local)

        val clear = requireNotNull(
            store.installNoCurrentPublishedSchedule(
                expectedState = store.state.value,
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
            ),
        )

        assertTrue(clear.awaitDurable())
        assertTrue(store.state.value.schedule.isEmpty())
        assertNull(store.state.value.scheduleGeneratedAt)
        assertEquals(
            "No schedule has been published for this workspace yet",
            store.state.value.scheduleMessage,
        )
    }

    @Test
    fun staleEncryptedExpectedStateCannotInstallOrClearCurrentSchedule() = runBlocking {
        val installStore = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem("planned", 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
            ),
        )
        val staleInstallExpected = installStore.state.value
        installStore.toggleCompleted()
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            installStore.installCurrentPublishedSchedule(
                staleInstallExpected,
                canonicalUpdate(
                    item = canonicalItem("planned", 8),
                    block = canonicalBlock(ItemStatus.SCHEDULED, 8),
                    cursor = "cursor-1",
                ),
                publishedRevision(),
            )
        }
        assertNull(installStore.state.value.publishedScheduleProof)
        assertEquals(7L, installStore.state.value.canonicalItems.single().revision)

        val clearStore = PlannerStore(publishedCanonicalState())
        val staleClearExpected = clearStore.state.value
        clearStore.toggleCompleted()
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            clearStore.installNoCurrentPublishedSchedule(
                staleClearExpected,
                CANONICAL_ORIGIN,
                "connection-1",
            )
        }
        assertNotNull(clearStore.state.value.publishedScheduleProof)
        assertEquals(publishedRevision(), clearStore.state.value.publishedScheduleRevision)
    }

    @Test
    fun schedulingProfileChangeRevokesPublicationAuthorityAndRejectsUncertaintyOrLease() {
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7)
        val store = PlannerStore(publishedCanonicalState(block = block))
        val original = store.state.value

        assertTrue(store.updateScheduleCompositionProfile(original.scheduleCompositionProfile))
        assertEquals(original.publishedScheduleProof, store.state.value.publishedScheduleProof)

        assertTrue(
            store.updateScheduleCompositionProfile(
                original.scheduleCompositionProfile.copy(firmHorizonDays = 14),
            ),
        )
        assertEquals(listOf(block), store.state.value.schedule)
        assertNull(store.state.value.publishedScheduleRevision)
        assertNull(store.state.value.publishedScheduleProof)
        assertNull(store.state.value.scheduleInputDigest)
        assertNull(store.state.value.localScheduleCompositionProvenance)
        assertEquals(
            "Scheduling profile changed · recompose to refresh the firm horizon",
            store.state.value.scheduleMessage,
        )
        assertTrue(store.isCanonicalExecutionStartBlocked(block.id))

        val pending = publication(
            canonicalUpdate(
                item = canonicalItem("planned", 8),
                block = canonicalBlock(ItemStatus.SCHEDULED, 8),
                cursor = "cursor-profile-pending",
            ),
        )
        val pendingStore = PlannerStore(DayWeaveUiState())
        assertNotNull(pendingStore.stageSchedulePublication(pending))
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            pendingStore.updateScheduleCompositionProfile(
                ScheduleCompositionProfileSnapshot(dayStartMinute = 8 * 60),
            )
        }
        assertEquals(pending, pendingStore.state.value.pendingSchedulePublication)

        val lease = executionSession("active", 1)
        val leaseStore = PlannerStore(
            publishedCanonicalState(block = block.copy(status = ItemStatus.ACTIVE)).copy(
                canonicalExecutionSyncOrigin = CANONICAL_ORIGIN,
                canonicalExecutionConfigurationId = "connection-1",
                canonicalExecutionRevision = 1,
                canonicalExecutionSession = lease,
            ),
        )
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            leaseStore.updateScheduleCompositionProfile(
                ScheduleCompositionProfileSnapshot(dayStartMinute = 8 * 60),
            )
        }
        assertNotNull(leaseStore.state.value.publishedScheduleProof)
    }

    @Test
    fun replayedPublicationCannotMintProofAtStoreBoundary() {
        val candidate = canonicalUpdate(
            item = canonicalItem("planned", 8),
            block = canonicalBlock(ItemStatus.SCHEDULED, 8),
            cursor = "cursor-replay",
        )
        val pending = publication(candidate)
        val store = PlannerStore(DayWeaveUiState())
        assertNotNull(store.stageSchedulePublication(pending))

        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            store.commitSchedulePublication(
                expected = pending,
                revision = publishedRevision(),
                replayed = true,
            )
        }

        assertEquals(pending, store.state.value.pendingSchedulePublication)
        assertNull(store.state.value.publishedScheduleProof)
        assertTrue(store.state.value.schedule.isEmpty())
    }

    @Test
    fun overlappingPublishedBlockRetainsExactProofAcrossHorizonBoundary() {
        val overlapping = canonicalBlock(ItemStatus.SCHEDULED, 8).copy(
            startMinute = 23 * 60,
            durationMinutes = 120,
            absoluteStartAt = "1969-12-31T23:00:00Z",
            absoluteEndAt = "1970-01-01T01:00:00Z",
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        val candidate = canonicalUpdate(
            item = canonicalItem("planned", 8),
            block = overlapping,
            cursor = "cursor-overlap",
        )
        val pending = publication(candidate)
        val store = PlannerStore(DayWeaveUiState())
        assertNotNull(store.stageSchedulePublication(pending))

        assertNotNull(
            store.commitSchedulePublication(
                expected = pending,
                revision = publishedRevision(),
                replayed = false,
            ),
        )

        val proof = requireNotNull(store.state.value.publishedScheduleProof)
        assertTrue(proof.hasValidShape())
        assertTrue(proof.matches(store.state.value.schedule.single()))
    }

    @Test
    fun canonicalStartRequiresExactBlockProofAndServerSessionIndex() {
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7)
        val actionable = publishedCanonicalState(block = block).copy(
            canonicalExecutionSyncOrigin = CANONICAL_ORIGIN,
            canonicalExecutionConfigurationId = "connection-1",
            canonicalExecutionHistoryVerified = true,
        )
        assertFalse(PlannerStore(actionable).isCanonicalExecutionStartBlocked(block.id))

        val newerHeadObserved = actionable.copy(
            publishedScheduleRevisionHint = requireNotNull(
                actionable.publishedScheduleRevisionHint,
            ).copy(revisionNumber = 2uL),
        )
        assertTrue(PlannerStore(newerHeadObserved).isCanonicalExecutionStartBlocked(block.id))
        assertTrue(
            newerHeadObserved.isPublishedScheduleDisplayCurrent(
                Instant.parse("1970-01-01T01:00:00Z"),
                ZoneId.of("UTC"),
            ),
        )

        val shifted = actionable.copy(
            schedule = listOf(
                block.copy(absoluteStartAt = "1970-01-01T01:05:00Z"),
            ),
        )
        assertTrue(PlannerStore(shifted).isCanonicalExecutionStartBlocked(block.id))

        val missingServerIndex = actionable.copy(
            schedule = listOf(block.copy(sessionIndex = null)),
        )
        assertTrue(
            PlannerStore(missingServerIndex).isCanonicalExecutionStartBlocked(block.id),
        )

        val localHelper = actionable.copy(publishedScheduleProof = null)
        assertEquals(listOf(block), localHelper.schedule)
        assertTrue(PlannerStore(localHelper).isCanonicalExecutionStartBlocked(block.id))

        val extraCanonicalBlock = block.copy(
            id = "55555555-5555-4555-8555-555555555555",
            sessionIndex = 8,
            absoluteStartAt = "1970-01-01T02:00:00Z",
            absoluteEndAt = "1970-01-01T03:00:00Z",
        )
        val planSetMismatch = actionable.copy(
            schedule = listOf(block, extraCanonicalBlock),
        )
        assertTrue(
            PlannerStore(planSetMismatch).isCanonicalExecutionStartBlocked(block.id),
        )

        val operationId = "66666666-6666-4666-8666-666666666666"
        val habitId = "77777777-7777-4777-8777-777777777778"
        val occurrenceId = "88888888-8888-4888-8888-888888888889"
        val outcome = HabitOutcomeCommandSnapshot(
            operationId = operationId,
            expectedRevision = 0,
            outcome = HabitOutcomeInputSnapshot(
                status = HabitOutcomeStatusSnapshot.SKIPPED,
                progressBasisPoints = 0,
                quantity = null,
                unit = null,
                actualSeconds = null,
                note = null,
                occurredAt = "1970-01-01T00:00:00Z",
            ),
        )
        val reviewedMutation = PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.OUTCOME,
            habitId = habitId,
            targetId = occurrenceId,
            expectedRevision = 0,
            idempotencyKey = operationId,
            requestJson = outcome.encoded(),
            createdAt = "1970-01-01T00:00:00Z",
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            disposition = PendingHabitMutationDisposition.CONFLICT,
        )
        val habitBlocked = actionable.copy(
            habitLedger = HabitLedgerSnapshot(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                pendingMutations = listOf(reviewedMutation),
            ),
        )
        val habitBlockedStore = PlannerStore(habitBlocked)
        assertTrue(habitBlockedStore.isCanonicalExecutionStartBlocked(block.id))
        assertThrows(IllegalArgumentException::class.java) {
            habitBlockedStore.stageCanonicalMutation(
                canonicalMutation("in_progress", ItemStatus.ACTIVE),
            )
        }
        assertNull(habitBlockedStore.state.value.pendingCanonicalMutation)
        assertThrows(IllegalArgumentException::class.java) {
            habitBlockedStore.stageExecutionCommand(
                PendingExecutionCommand(
                    idempotencyKey = "99999999-9999-4999-8999-999999999998",
                    syncOrigin = CANONICAL_ORIGIN,
                    configurationId = "connection-1",
                    expectedRevision = 0,
                    sessionId = EXECUTION_ID,
                    itemId = CANONICAL_ITEM_ID,
                    itemRevision = 7,
                    sessionIndex = 0,
                    plannedBlockId = block.id,
                    sourceDeviceId = DEVICE_ID,
                    commandType = "start",
                    requestJson = "{}",
                    focusedBlockId = block.id,
                    startedAt = "1970-01-01T00:00:00Z",
                ),
            )
        }
        assertNull(habitBlockedStore.state.value.pendingExecutionCommand)
    }

    @Test
    fun localShiftAndRecomposeInvalidateProofWithoutHidingSchedule() {
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7)
        val shiftedStore = PlannerStore(
            publishedCanonicalState(block = block).copy(
                activeSession = ActiveSession(
                    itemId = block.id,
                    elapsedMinutes = 0,
                    isPaused = false,
                ),
            ),
        )
        shiftedStore.doActiveLater()
        assertEquals(block.startMinute + 60, shiftedStore.state.value.schedule.single().startMinute)
        assertNull(shiftedStore.state.value.publishedScheduleProof)

        val recomposedStore = PlannerStore(publishedCanonicalState(block = block))
        recomposedStore.recompose()
        assertEquals(listOf(block), recomposedStore.state.value.schedule)
        assertNull(recomposedStore.state.value.publishedScheduleProof)
    }

    @Test
    fun publicationCannotCrossPendingAuthoringOrReinstallAFilteredProof() {
        val oldItem = canonicalItem("planned", 7)
        val store = PlannerStore(publishedCanonicalState(item = oldItem))
        assertNotNull(
            store.enqueueCanonicalReplace(
                oldItem.id,
                oldItem.toCanonicalDraft().copy(title = "Local edit"),
            ),
        )
        assertNull(store.state.value.publishedScheduleRevision)
        assertNull(store.state.value.scheduleInputDigest)
        val candidate = canonicalUpdate(
            item = oldItem.copy(revision = 8, updatedAt = "1970-01-01T00:01:00Z"),
            block = canonicalBlock(ItemStatus.SCHEDULED, 8),
            cursor = "cursor-authoring-race",
        )
        val pending = publication(candidate)

        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            store.stageSchedulePublication(pending)
        }

        // A recovered legacy/racing journal still cannot make a filtered candidate current.
        val recoveryStore = PlannerStore(
            store.state.value.copy(pendingSchedulePublication = pending),
        )
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            recoveryStore.commitSchedulePublication(
                pending,
                publishedRevision(),
                replayed = false,
            )
        }
        assertEquals(pending, recoveryStore.state.value.pendingSchedulePublication)
        assertNull(recoveryStore.state.value.publishedScheduleRevision)
        assertNull(recoveryStore.state.value.scheduleInputDigest)
    }

    @Test
    fun replayAndTypedStaleResolutionClearOnlyExactJournalWithoutInstallingCandidate() {
        listOf(true, false).forEach { replayed ->
            val oldBlock = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(title = "Old plan")
            val store = PlannerStore(
                publishedCanonicalState(block = oldBlock).copy(canonicalDeltaCursor = "cursor-0"),
            )
            val candidate = canonicalUpdate(
                item = canonicalItem("planned", 8),
                block = canonicalBlock(ItemStatus.SCHEDULED, 8).copy(title = "Rejected candidate"),
                cursor = "cursor-1",
            )
            val pending = publication(candidate)
            assertNotNull(store.stageSchedulePublication(pending))

            val receipt = if (replayed) {
                store.resolveReplayedSchedulePublication(pending, publishedRevision())
            } else {
                store.discardStaleSchedulePublication(pending)
            }

            assertNotNull(receipt)
            assertEquals(null, store.state.value.pendingSchedulePublication)
            assertEquals(null, store.state.value.publishedScheduleRevision)
            assertEquals(null, store.state.value.scheduleInputDigest)
            assertEquals("cursor-0", store.state.value.canonicalDeltaCursor)
            assertEquals(listOf(oldBlock), store.state.value.schedule)
            assertFalse(store.state.value.schedule.any { it.title == "Rejected candidate" })

            val stale = runCatching {
                if (replayed) {
                    store.resolveReplayedSchedulePublication(pending, publishedRevision())
                } else {
                    store.discardStaleSchedulePublication(pending)
                }
            }
            assertTrue(stale.isFailure)
        }
    }

    @Test
    fun publicationJournalBlocksOtherServerWritesAndExplicitForgetQuarantinesIt() {
        val candidate = canonicalUpdate(
            item = canonicalItem("planned", 7),
            block = canonicalBlock(ItemStatus.SCHEDULED, 7),
            cursor = "cursor-1",
        )
        val store = PlannerStore(DayWeaveUiState())
        val pending = publication(candidate)
        assertNotNull(store.stageSchedulePublication(pending))

        val mutation = PendingCanonicalMutation(
            idempotencyKey = "99999999-9999-4999-8999-999999999999",
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            itemId = CANONICAL_ITEM_ID,
            expectedRevision = 7,
            targetStatus = "planned",
            targetIsSensitive = false,
            startedAt = "1970-01-01T00:00:00Z",
            replacementRequestJson = "{}",
            focusedBlockId = CANONICAL_BLOCK_ID,
            displayStatus = ItemStatus.SCHEDULED,
        )
        assertTrue(runCatching { store.stageCanonicalMutation(mutation) }.isFailure)

        assertNotNull(store.abandonCanonicalConnection())
        assertEquals(null, store.state.value.pendingSchedulePublication)
        assertEquals(null, store.state.value.publishedScheduleRevision)
        assertFalse(store.hasCredentialReplacementBlocker())
    }

    @Test
    fun acknowledgedCanonicalMutationInvalidatesPublishedReceiptWithItsInputDigest() {
        val store = PlannerStore(publishedCanonicalState())
        val mutation = canonicalMutation(
            targetStatus = "completed",
            displayStatus = ItemStatus.COMPLETED,
        )

        assertNotNull(store.stageCanonicalMutation(mutation))
        assertNotNull(
            store.reconcileCanonicalItem(
                item = canonicalItem("completed", 8),
                focusedBlockId = CANONICAL_BLOCK_ID,
                displayStatus = ItemStatus.COMPLETED,
            ),
        )

        assertPublishedPlanInvalidated(store)
    }

    @Test
    fun acknowledgedSensitivityMutationInvalidatesPublishedReceiptWithItsInputDigest() {
        val store = PlannerStore(publishedCanonicalState())
        val mutation = canonicalMutation(
            targetStatus = "planned",
            displayStatus = ItemStatus.SCHEDULED,
            targetIsSensitive = true,
            focusedBlockId = CANONICAL_ITEM_ID,
        )

        assertNotNull(store.stageCanonicalMutation(mutation))
        assertNotNull(
            store.reconcileCanonicalItemSensitivity(
                canonicalItem("planned", 8).copy(isSensitive = true),
            ),
        )

        assertPublishedPlanInvalidated(store)
    }

    @Test
    fun localRecurrenceResolutionInvalidatesPublishedReceiptWithItsInputDigest() {
        val occurrenceId = "66666666-6666-4666-8666-666666666666"
        val item = canonicalItem("planned", 7).copy(
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(occurrenceId = occurrenceId)
        val store = PlannerStore(
            publishedCanonicalState(item, block).copy(
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
                recurrenceOccurrenceSources = mapOf(occurrenceId to occurrenceSource()),
            ),
            nowEpochMillis = { 60_000L },
        )

        assertNotNull(
            store.reconcileLocalCanonicalSession(
                CANONICAL_BLOCK_ID,
                ItemStatus.COMPLETED,
            ),
        )

        assertPublishedPlanInvalidated(store)
        assertTrue(occurrenceId in store.state.value.recurrenceOutcomes)
    }

    @Test
    fun remoteRecurrenceResolutionInvalidatesPublishedReceiptWithItsInputDigest() {
        val occurrenceId = "66666666-6666-4666-8666-666666666666"
        val item = canonicalItem("planned", 7).copy(
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(occurrenceId = occurrenceId)
        val store = PlannerStore(
            publishedCanonicalState(item, block).copy(
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            ),
        )
        val terminal = executionSession("active", 1).copy(
            occurrenceId = occurrenceId,
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )

        assertNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Recurring session completed",
            ),
        )

        assertPublishedPlanInvalidated(store)
        assertTrue(occurrenceId in store.state.value.recurrenceOutcomes)
    }

    @Test
    fun remoteDeferredSessionRemainsHistoryOnlyAndNeverProjectsAnOutcome() {
        val occurrenceId = "66666666-6666-4666-8666-666666666666"
        val item = canonicalItem("planned", 7).copy(
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(occurrenceId = occurrenceId)
        val store = PlannerStore(
            publishedCanonicalState(item, block).copy(
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            ),
        )
        val running = executionSession("active", 1).copy(occurrenceId = occurrenceId)
        assertNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = running,
                message = "Recurring session active",
            ),
        )
        val deferred = running.copy(
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            moveStart = "1970-01-01T02:00:00Z",
            moveEnd = "1970-01-01T03:00:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )

        assertNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = deferred,
                message = "Recurring session deferred",
            ),
        )
        assertNotNull(
            store.recordCanonicalExecutionHistoryWindow(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                history = listOf(deferred.copy(canonicalProjectionEligibleAtLeaseStart = null)),
                continuityVerified = true,
                message = "Deferred history retained",
            ),
        )

        val state = store.state.value
        assertNull(state.canonicalExecutionSession)
        assertEquals(ItemStatus.SCHEDULED, state.schedule.single().status)
        assertEquals("planned", state.canonicalItems.single().status)
        val deferredOutcome = state.terminalExecutionOutcomes.getValue(deferred.id)
        assertEquals(deferred, deferredOutcome.session)
        assertFalse(deferredOutcome.requiresCanonicalItemProjection)
        assertNull(deferredOutcome.canonicalProjectionRevision)
        assertNull(deferredOutcome.canonicalProjectionResolution)
        assertNull(deferredOutcome.canonicalProjectionConflict)
        assertNull(deferredOutcome.canonicalProjectionRetryAuthorizedAt)
        assertTrue(state.recurrenceOutcomes.isEmpty())
        assertTrue(state.recurrenceCompletionAnchors.isEmpty())
        assertEquals("deferred", state.canonicalExecutionHistoryWindow.single().status)
        assertEquals(deferred.moveStart, state.canonicalExecutionHistoryWindow.single().moveStart)
        assertEquals(deferred.moveEnd, state.canonicalExecutionHistoryWindow.single().moveEnd)
        assertTrue(store.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        assertNull(state.publishedScheduleRevision)
        assertNull(state.publishedScheduleProof)
        assertNull(state.scheduleInputDigest)

        val restarted = PlannerStore(state)
        assertTrue(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        assertEquals(deferredOutcome, restarted.state.value.terminalExecutionOutcomes[deferred.id])

        val recomposedBlockId = "77777777-7777-4777-8777-777777777777"
        val restartedAfterRecompose = PlannerStore(
            state.copy(
                schedule = state.schedule.map { scheduled ->
                    scheduled.copy(id = recomposedBlockId)
                },
            ),
        )
        assertTrue(restartedAfterRecompose.isCanonicalExecutionStartBlocked(recomposedBlockId))
        assertEquals(
            deferredOutcome,
            restartedAfterRecompose.state.value.terminalExecutionOutcomes[deferred.id],
        )
    }

    @Test
    fun recurringDeferralInvalidatesPublishedReceiptWithItsInputDigest() {
        val occurrenceId = "66666666-6666-5666-8666-666666666666"
        val item = canonicalItem("planned", 7).copy(
            recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(occurrenceId = occurrenceId)
        val store = PlannerStore(
            publishedCanonicalState(item, block).copy(
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
                recurrenceOccurrenceSources = mapOf(occurrenceId to occurrenceSource()),
            ),
            nowEpochMillis = { 60_000L },
        )

        assertNotNull(
            store.deferLocalCanonicalSession(
                CANONICAL_BLOCK_ID,
                Instant.parse("1970-01-01T03:00:00Z"),
            ),
        )

        assertPublishedPlanInvalidated(store)
        val move = store.state.value.recurrenceMoves.getValue(occurrenceId)
        assertEquals("1970-01-01T03:00:00Z", move.startAt)
        assertEquals("1970-01-01T04:00:00Z", move.endAt)
        assertEquals(
            move,
            PlannerStore(store.state.value).state.value.recurrenceMoves[occurrenceId],
        )
    }

    @Test
    fun recurringDeferralAnchorsTheTappedSplitAtTheChosenTime() {
        val occurrenceId = "66666666-6666-5666-8666-666666666666"
        val first = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(
            occurrenceId = occurrenceId,
            durationMinutes = 60,
            isSplittable = true,
        )
        val second = first.copy(
            id = "77777777-7777-4777-8777-777777777777",
            startMinute = 3 * 60,
            sessionIndex = 1,
            absoluteStartAt = "1970-01-01T03:00:00Z",
            absoluteEndAt = "1970-01-01T04:00:00Z",
        )
        val item = canonicalItem("planned", 7).copy(
            recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
            splitPolicyJson = "{\"type\":\"splittable\"}",
        )
        val published = publishedCanonicalState(item, first)
        val proof = requireNotNull(published.publishedScheduleProof)
        val store = PlannerStore(
            published.copy(
                schedule = listOf(first, second),
                publishedScheduleProof = proof.copy(
                    blocks = listOf(
                        PublishedScheduleBlockProofSnapshot.from(first),
                        PublishedScheduleBlockProofSnapshot.from(second),
                    ),
                ),
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
                recurrenceOccurrenceSources = mapOf(occurrenceId to occurrenceSource()),
            ),
            nowEpochMillis = { 60_000L },
        )

        assertNotNull(
            store.deferLocalCanonicalSession(
                second.id,
                Instant.parse("1970-01-01T05:00:00Z"),
            ),
        )

        val move = store.state.value.recurrenceMoves.getValue(occurrenceId)
        assertEquals("1970-01-01T03:00:00Z", move.startAt)
        assertEquals("1970-01-01T06:00:00Z", move.endAt)
    }

    @Test
    fun recurringDeferralCannotShiftScheduledSiblingOfAuthoritativeOpenLease() {
        val occurrenceId = "66666666-6666-5666-8666-666666666666"
        val secondBlockId = "77777777-7777-4777-8777-777777777777"
        val first = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(
            occurrenceId = occurrenceId,
            durationMinutes = 30,
            isSplittable = true,
            absoluteEndAt = "1970-01-01T01:30:00Z",
        )
        val second = first.copy(
            id = secondBlockId,
            startMinute = 90,
            sessionIndex = 1,
            absoluteStartAt = "1970-01-01T01:30:00Z",
            absoluteEndAt = "1970-01-01T02:00:00Z",
        )
        val openLease = executionSession("active", 1).copy(
            occurrenceId = occurrenceId,
            sessionIndex = 1,
            plannedBlockId = secondBlockId,
        )
        val initial = DayWeaveUiState(
            canonicalItems = listOf(
                canonicalItem("planned", 7).copy(
                    recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
                    splitPolicyJson = "{\"type\":\"splittable\"}",
                ),
            ),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalExecutionSyncOrigin = CANONICAL_ORIGIN,
            canonicalExecutionConfigurationId = "connection-1",
            canonicalExecutionSession = openLease,
            schedule = listOf(first, second),
            occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            recurrenceOccurrenceSources = mapOf(occurrenceId to occurrenceSource()),
        )
        val store = PlannerStore(initial, nowEpochMillis = { 60_000L })

        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            store.deferLocalCanonicalSession(
                CANONICAL_BLOCK_ID,
                Instant.parse("1970-01-01T03:00:00Z"),
            )
        }

        assertEquals(initial.schedule, store.state.value.schedule)
        assertTrue(store.state.value.recurrenceMoves.isEmpty())
        assertEquals(openLease, store.state.value.canonicalExecutionSession)
    }

    @Test
    fun recurringDeferralRejectsPinnedSiblingAndUnscheduledOccurrenceRemainder() {
        val occurrenceId = "66666666-6666-5666-8666-666666666666"
        val focused = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(
            occurrenceId = occurrenceId,
            durationMinutes = 30,
            isSplittable = true,
            absoluteEndAt = "1970-01-01T01:30:00Z",
        )
        val pinnedSibling = focused.copy(
            id = "77777777-7777-4777-8777-777777777777",
            sessionIndex = 1,
            startMinute = 90,
            absoluteStartAt = "1970-01-01T01:30:00Z",
            absoluteEndAt = "1970-01-01T02:00:00Z",
            isFlexible = false,
            isHardConstraint = true,
            canonicalBlockKind = "pinned",
        )
        fun state(
            schedule: List<ScheduleItem>,
            unscheduled: List<UnscheduledWorkSnapshot> = emptyList(),
        ) = DayWeaveUiState(
            canonicalItems = listOf(
                canonicalItem("planned", 7).copy(
                    recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
                    splitPolicyJson = "{\"type\":\"splittable\"}",
                ),
            ),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            schedule = schedule,
            unscheduledWork = unscheduled,
            occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            recurrenceOccurrenceSources = mapOf(occurrenceId to occurrenceSource()),
        )
        val unsafeStates = listOf(
            state(listOf(focused, pinnedSibling)),
            state(
                listOf(focused),
                listOf(
                    UnscheduledWorkSnapshot(
                        itemId = "99999999-9999-4999-8999-999999999999",
                        occurrenceId = occurrenceId,
                        remainingMinutes = 15,
                        reason = "no_capacity",
                    ),
                ),
            ),
        )

        unsafeStates.forEach { unsafe ->
            val store = PlannerStore(unsafe, nowEpochMillis = { 60_000L })
            org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
                store.deferLocalCanonicalSession(
                    CANONICAL_BLOCK_ID,
                    Instant.parse("1970-01-01T03:00:00Z"),
                )
            }
            assertTrue(store.state.value.recurrenceMoves.isEmpty())
            assertEquals(unsafe.schedule, store.state.value.schedule)
        }
    }

    @Test
    fun malformedOrCustomRestoredIdentityCannotAuthorizeARecurrenceMove() {
        val occurrenceId = "66666666-6666-5666-8666-666666666666"
        val item = canonicalItem("planned", 7).copy(
            recurrenceJson = "{\"type\":\"custom\",\"rrule\":\"FREQ=DAILY;COUNT=10\"}",
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 7).copy(occurrenceId = occurrenceId)
        val validSource = occurrenceSource()
        val validMove = RecurrenceMoveSnapshot(
            itemId = CANONICAL_ITEM_ID,
            startAt = "1970-01-01T03:00:00Z",
            endAt = "1970-01-01T04:00:00Z",
            movedAt = "1970-01-01T00:01:00Z",
            source = validSource,
        )
        val base = publishedCanonicalState(item, block).copy(
            occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
        )

        listOf(
            validSource.copy(identityJson = "{\"type\":\"unknown\"}"),
            validSource.copy(identityJson = "{\"type\":\"custom\"}", localDate = null),
        ).forEach { invalidSource ->
            val restored = PlannerStore(
                base.copy(
                    recurrenceOccurrenceSources = mapOf(occurrenceId to invalidSource),
                    recurrenceMoves = mapOf(
                        occurrenceId to validMove.copy(source = invalidSource),
                    ),
                ),
            ).state.value

            if (invalidSource.identityJson?.contains("custom") == true) {
                assertEquals(invalidSource, restored.recurrenceOccurrenceSources[occurrenceId])
            } else {
                assertFalse(occurrenceId in restored.recurrenceOccurrenceSources)
            }
            assertFalse(occurrenceId in restored.recurrenceMoves)
            assertNull(restored.publishedScheduleProof)
            assertTrue(restored.scheduleMessage.contains("abandoned"))
        }
    }

    @Test
    fun durableExecutionRejectsFractionalDeferredMoveWindows() {
        val store = PlannerStore(publishedCanonicalState())
        val deferred = executionSession("active", 1).copy(
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            moveStart = "1970-01-01T02:00:00Z",
            moveEnd = "1970-01-01T03:00:00.500Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )

        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = deferred,
                message = "Invalid fractional defer",
            )
        }
        assertTrue(store.state.value.terminalExecutionOutcomes.isEmpty())
    }

    @Test
    fun provenDeletionDuringTerminalProjectionInvalidatesPublishedReceiptAndDigest() {
        val store = PlannerStore(publishedCanonicalState())
        val running = executionSession("active", 1, projectionEligible = true)
        assertNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                1,
                running,
                message = "Execution started",
            ),
        )
        val terminal = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        assertNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                terminal,
                message = "Execution completed",
            ),
        )
        val pending = canonicalMutation(
            targetStatus = "completed",
            displayStatus = ItemStatus.COMPLETED,
            terminalExecutionSessionId = EXECUTION_ID,
        )
        assertNotNull(store.stageCanonicalMutation(pending))

        assertNotNull(
            store.resolveDeletedPendingTerminalProjection(
                pending.idempotencyKey,
                EXECUTION_ID,
            ),
        )

        assertPublishedPlanInvalidated(store)
        assertTrue(store.state.value.canonicalItems.isEmpty())
        assertTrue(store.state.value.schedule.isEmpty())
    }

    @Test
    fun exactRemoteGenerationWaitsForItsOwnSaveWhileLaterUiMutationStaysNonBlocking() =
        runBlocking {
            val initial = DayWeaveUiState()
            val saveStarted = Channel<DayWeaveUiState>(Channel.UNLIMITED)
            val allowSave = Channel<Unit>(Channel.UNLIMITED)
            val repository = object : PlannerStateRepository {
                override suspend fun load(): DayWeaveUiState = initial

                override suspend fun save(state: DayWeaveUiState) {
                    saveStarted.send(state)
                    allowSave.receive()
                }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

            try {
                val store = PlannerStore(initial, repository, scope)
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }

                val receipt = requireNotNull(
                    store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
                )
                val durable = async { receipt.awaitDurable() }
                val exactSnapshot = withTimeout(3_000) { saveStarted.receive() }

                assertFalse(durable.isCompleted)
                assertTrue(store.quickCapture("Later UI edit", ItemKind.TASK))
                assertTrue(exactSnapshot.suggestions.any { it.id == "remote-proposal" })
                assertFalse(exactSnapshot.inbox.any { it.title == "Later UI edit" })

                allowSave.send(Unit)
                assertTrue(withTimeout(3_000) { durable.await() })
                val laterSnapshot = withTimeout(3_000) { saveStarted.receive() }
                assertTrue(laterSnapshot.inbox.any { it.title == "Later UI edit" })
                allowSave.send(Unit)
            } finally {
                scope.cancel()
            }
        }

    @Test
    fun exactRemoteGenerationReportsSaveFailure() = runBlocking {
        val initial = DayWeaveUiState()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                throw IllegalStateException("synthetic encrypted save failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val receipt = requireNotNull(
                store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
            )

            assertFalse(withTimeout(3_000) { receipt.awaitDurable() })
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
    fun cancellingOneWaiterDoesNotCancelTheExactEncryptedSave() = runBlocking {
        val initial = DayWeaveUiState()
        val saveStarted = CompletableDeferred<Unit>()
        val allowSave = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveStarted.complete(Unit)
                allowSave.await()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val receipt = requireNotNull(
                store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
            )
            val firstWaiter = async { receipt.awaitDurable() }
            withTimeout(3_000) { saveStarted.await() }

            firstWaiter.cancelAndJoin()
            allowSave.complete(Unit)

            assertTrue(withTimeout(3_000) { receipt.awaitDurable() })
            assertEquals(PlannerLoadState.READY, store.loadState.value)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun cancelledRepositorySaveFailsItsAcknowledgementInsteadOfHanging() = runBlocking {
        val initial = DayWeaveUiState()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                throw CancellationException("synthetic repository cancellation")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val receipt = requireNotNull(
                store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
            )

            assertFalse(withTimeout(3_000) { receipt.awaitDurable() })
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
    fun restoreBlocksInputUntilPersistedStateIsReadyAndThenAutosaves() = runBlocking {
        val restoredState = DayWeaveUiState.preview().copy(
            protectedFreeMinutes = 37,
            scheduleMessage = "Restored from disk",
        )
        val allowLoad = CompletableDeferred<Unit>()
        val savedStates = Channel<DayWeaveUiState>(Channel.UNLIMITED)
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState {
                allowLoad.await()
                return restoredState
            }

            override suspend fun save(state: DayWeaveUiState) {
                savedStates.send(state)
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState.preview(),
                repository = repository,
                scope = scope,
            )

            assertEquals(PlannerLoadState.LOADING, store.loadState.value)
            assertFalse(store.quickCapture("Capture during restore", ItemKind.TASK))
            allowLoad.complete(Unit)

            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertEquals(restoredState, store.state.value)
            assertTrue(store.quickCapture("Capture after restore", ItemKind.TASK))
            val savedState = withTimeout(3_000) { savedStates.receive() }

            assertEquals(store.state.value, savedState)
            assertEquals("Capture after restore", savedState.inbox.first().title)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun persistenceFailureBecomesReadOnlyWithoutReplacingVisibleState() = runBlocking {
        val initial = DayWeaveUiState.preview()
        val failure = IllegalStateException("synthetic encrypted storage failure")
        val reported = CompletableDeferred<Throwable>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = throw failure

            override suspend fun save(state: DayWeaveUiState) = Unit
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(
                initialState = initial,
                repository = repository,
                scope = scope,
                onPersistenceError = { reported.complete(it) },
            )

            withTimeout(3_000) {
                store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED }
            }
            assertEquals(failure, reported.await())
            assertEquals(initial, store.state.value)
            assertFalse(store.quickCapture("Must not be accepted", ItemKind.TASK))
            assertEquals(initial, store.state.value)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun blankQuickCaptureIsRejectedWithoutChangingInbox() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val before = store.state.value.inbox

        val accepted = store.quickCapture("   ", ItemKind.TASK)

        assertFalse(accepted)
        assertEquals(before, store.state.value.inbox)
    }

    @Test
    fun quickCaptureAddsReviewableInboxItemButDoesNotScheduleIt() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val scheduleBefore = store.state.value.schedule

        val accepted = store.quickCapture(
            title = "Call the dentist",
            kind = ItemKind.TASK,
            isSensitive = true,
        )

        assertTrue(accepted)
        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals("Call the dentist", store.state.value.inbox.first().title)
        assertEquals(InboxSource.QUICK_CAPTURE, store.state.value.inbox.first().source)
        assertTrue(store.state.value.inbox.first().requiresReview)
        assertTrue(store.state.value.inbox.first().isSensitive)
    }

    @Test
    fun abandoningCredentialBindingQuarantinesOnlyApiDerivedCaches() {
        val localBlock = canonicalBlock(ItemStatus.SCHEDULED, 1).copy(
            id = "local-block",
            title = "Local work",
            canonicalItemId = null,
            canonicalRevision = null,
            canonicalBlockKind = null,
        )
        val localSuggestion = remoteSuggestion().copy(
            id = "local-suggestion",
            remoteRevision = null,
            remotePayloadJson = null,
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem("planned", 1)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-1",
                schedule = listOf(
                    localBlock,
                    canonicalBlock(ItemStatus.SCHEDULED, 1),
                ),
                suggestions = listOf(localSuggestion, remoteSuggestion()),
                inbox = listOf(
                    InboxItem("local-draft", title = "Local draft", source = InboxSource.QUICK_CAPTURE),
                    InboxItem(
                        "remote-draft",
                        title = "Remote draft",
                        source = InboxSource.EXTERNAL_PROPOSAL,
                    ),
                ),
            ),
        )

        assertNotNull(store.abandonCanonicalConnection())

        val fenced = store.state.value
        assertEquals(listOf(localBlock), fenced.schedule)
        assertTrue(fenced.canonicalItems.isEmpty())
        assertNull(fenced.canonicalSyncOrigin)
        assertNull(fenced.canonicalConfigurationId)
        assertNull(fenced.canonicalDeltaCursor)
        assertEquals(listOf(localSuggestion), fenced.suggestions)
        assertEquals(listOf("local-draft"), fenced.inbox.map { it.id })
    }

    @Test
    fun ancestorSensitivityAcknowledgementImmediatelyProtectsCachedDescendantBlocks() {
        val parentId = "77777777-7777-4777-8777-777777777777"
        val parent = canonicalItem("planned", 1).copy(
            id = parentId,
            title = "SYNTHETIC-PRIVATE-PARENT",
            isExecutable = false,
        )
        val child = canonicalItem("planned", 1).copy(
            id = CANONICAL_ITEM_ID,
            title = "SYNTHETIC-PRIVATE-CHILD",
            parentId = parentId,
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 1).copy(isSensitive = false)
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(parent, child),
                schedule = listOf(block),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
            ),
        )
        val pending = PendingCanonicalMutation(
            idempotencyKey = "88888888-8888-4888-8888-888888888888",
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            itemId = parentId,
            expectedRevision = 1,
            targetStatus = "planned",
            targetIsSensitive = true,
            startedAt = "2026-08-29T08:00:00Z",
            replacementRequestJson = "{}",
            focusedBlockId = parentId,
            displayStatus = ItemStatus.SCHEDULED,
        )

        assertNotNull(store.stageCanonicalMutation(pending))
        assertTrue(store.state.value.schedule.single().isSensitive)
        val restarted = PlannerStore(
            store.state.value.copy(schedule = listOf(block.copy(isSensitive = false))),
        )
        assertTrue(restarted.state.value.schedule.single().isSensitive)
        assertTrue(
            requireNotNull(restarted.state.value.pendingCanonicalMutation).targetIsSensitive,
        )
        assertNotNull(
            store.reconcileCanonicalItemSensitivity(
                parent.copy(
                    isSensitive = true,
                    revision = 2,
                    updatedAt = "2026-08-29T08:01:00Z",
                ),
            ),
        )

        val current = store.state.value
        assertTrue(current.canonicalItems.first { it.id == parentId }.isSensitive)
        assertFalse(current.canonicalItems.first { it.id == CANONICAL_ITEM_ID }.isSensitive)
        assertTrue(current.schedule.single().isSensitive)
        assertEquals(1L, current.schedule.single().canonicalRevision)
        assertNull(current.pendingCanonicalMutation)
    }

    @Test
    fun approvingExternalSuggestionCannotMutateSchedule() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val scheduleBefore = store.state.value.schedule
        val suggestion = store.state.value.suggestions.first()

        store.approveSuggestion(suggestion.id)

        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals(
            SuggestionDisposition.APPROVED_FOR_INBOX,
            store.state.value.suggestions.first { it.id == suggestion.id }.disposition,
        )
        val proposalDraft = store.state.value.inbox.first()
        assertEquals(InboxSource.EXTERNAL_PROPOSAL, proposalDraft.source)
        assertTrue(proposalDraft.requiresReview)
    }

    @Test
    fun rejectingSuggestionLeavesPlanUntouched() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val scheduleBefore = store.state.value.schedule
        val suggestion = store.state.value.suggestions.first()

        store.rejectSuggestion(suggestion.id)

        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals(
            SuggestionDisposition.REJECTED,
            store.state.value.suggestions.first { it.id == suggestion.id }.disposition,
        )
    }

    @Test
    fun startingAnotherItemMaintainsSingleActiveSession() {
        val store = PlannerStore(DayWeaveUiState.preview())

        store.startItem("scheduler-tests")

        val state = store.state.value
        assertEquals("scheduler-tests", state.activeSession?.itemId)
        assertEquals(1, state.schedule.count { it.status == ItemStatus.ACTIVE })
        assertEquals(ItemStatus.PAUSED, state.schedule.first { it.id == "architecture" }.status)
    }

    @Test
    fun pauseCanBeTimedAndResumeClearsPausePlan() {
        val store = PlannerStore(DayWeaveUiState.preview())

        store.pauseActive(15)

        assertTrue(store.state.value.activeSession?.isPaused == true)
        assertEquals("15 minute break", store.state.value.activeSession?.pauseLabel)
        assertEquals(ItemStatus.PAUSED, store.state.value.activeItem?.status)

        store.resumeActive()

        assertFalse(store.state.value.activeSession?.isPaused ?: true)
        assertNull(store.state.value.activeSession?.pauseLabel)
        assertEquals(ItemStatus.ACTIVE, store.state.value.activeItem?.status)
    }

    @Test
    fun elapsedTimerAndTimedPauseUseMonotonicExecutionFields() {
        var now = 0L
        val block = ScheduleItem(
            id = "timed",
            title = "Timed focus",
            kind = ItemKind.TASK,
            startMinute = 9 * 60,
            durationMinutes = 30,
            status = ItemStatus.ACTIVE,
        )
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                schedule = listOf(block),
                activeSession = ActiveSession(
                    itemId = block.id,
                    elapsedMinutes = 0,
                    isPaused = false,
                    accumulatedSeconds = 0,
                    runningSinceEpochMillis = 0,
                ),
            ),
            nowEpochMillis = { now },
        )

        now = 61_000
        assertTrue(store.tickActiveSession())
        assertEquals(1, store.state.value.activeSession?.elapsedMinutes)
        store.pauseActive(1)
        assertFalse(store.timedPauseReady())

        now = 121_000
        assertTrue(store.timedPauseReady())
        assertTrue(store.tickActiveSession())
        assertTrue(store.state.value.activeSession?.timedBreakEnded == true)
        assertTrue(store.state.value.activeSession?.isPaused == true)

        store.pauseActive(1)
        assertFalse(store.state.value.activeSession?.timedBreakEnded ?: true)
        assertFalse(store.timedPauseReady())
        now = 181_000
        assertTrue(store.tickActiveSession())
        assertTrue(store.state.value.activeSession?.timedBreakEnded == true)
        store.resumeActive()
        now = 241_000
        store.tickActiveSession()

        assertEquals(2, store.state.value.activeSession?.elapsedMinutes)
        assertFalse(store.state.value.activeSession?.isPaused ?: true)
        assertNull(store.state.value.activeSession?.pauseUntilEpochMillis)
    }

    @Test
    fun authoritativePlanStatusTransitionsPreserveAContinuouslyCorrectTimer() {
        var now = 0L
        val activeItem = canonicalItem(status = "in_progress", revision = 7)
        val activeBlock = canonicalBlock(ItemStatus.ACTIVE, revision = 7)
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                canonicalItems = listOf(activeItem),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(activeBlock),
                activeSession = ActiveSession(
                    itemId = CANONICAL_BLOCK_ID,
                    elapsedMinutes = 0,
                    isPaused = false,
                    accumulatedSeconds = 0,
                    runningSinceEpochMillis = 0,
                ),
            ),
            nowEpochMillis = { now },
        )

        now = 61_000
        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = canonicalItem(status = "paused", revision = 8),
                block = canonicalBlock(ItemStatus.PAUSED, revision = 8),
                cursor = "cursor-1",
            ),
        )

        val paused = requireNotNull(store.state.value.activeSession)
        assertTrue(paused.isPaused)
        assertEquals(61L, paused.accumulatedSeconds)
        assertEquals(1, paused.elapsedMinutes)
        assertNull(paused.runningSinceEpochMillis)

        now = 121_000
        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = canonicalItem(status = "in_progress", revision = 9),
                block = canonicalBlock(ItemStatus.ACTIVE, revision = 9),
                cursor = "cursor-2",
            ),
        )
        assertFalse(requireNotNull(store.state.value.activeSession).isPaused)
        assertEquals(121_000L, store.state.value.activeSession?.runningSinceEpochMillis)

        now = 181_000
        store.tickActiveSession()
        assertEquals(2, store.state.value.activeSession?.elapsedMinutes)
        assertEquals(61L, store.state.value.activeSession?.accumulatedSeconds)
    }

    @Test
    fun confirmedCompleteSurvivesFreshScheduledCompositionAndRestartFence() {
        assertTerminalExecutionSurvivesComposition(
            wireStatus = "completed",
            displayStatus = ItemStatus.COMPLETED,
        )
    }

    @Test
    fun confirmedSkipSurvivesFreshScheduledCompositionAndRestartFence() {
        assertTerminalExecutionSurvivesComposition(
            wireStatus = "skipped",
            displayStatus = ItemStatus.SKIPPED,
        )
    }

    @Test
    fun compositionUsesNewestTerminalSessionForTheSameProjectionTarget() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val older = executionSession(status = "active", revision = 1).copy(
            id = "33333333-3333-4333-8333-333333333333",
            status = "skipped",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        val newer = executionSession(status = "active", revision = 1).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:02:00Z",
            updatedAt = "1970-01-01T01:02:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                older,
                message = "Older skip",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                4,
                null,
                newer,
                message = "Newer completion",
            ),
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-1",
                ),
            ),
        )

        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertEquals(2, store.state.value.terminalExecutionOutcomes.size)
    }

    @Test
    fun newerDeferredClosureSuppressesOlderTerminalPresentationAndRecurrenceProjection() {
        val occurrenceId = "66666666-6666-4666-8666-666666666666"
        val item = canonicalItem(status = "planned", revision = 7).copy(
            recurrenceJson = "{\"frequency\":\"daily\"}",
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7).copy(
            occurrenceId = occurrenceId,
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(item),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(block),
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            ),
        )
        val olderCompleted = executionSession(status = "active", revision = 1).copy(
            id = "33333333-3333-4333-8333-333333333333",
            occurrenceId = occurrenceId,
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        val newerDeferred = executionSession(status = "active", revision = 1).copy(
            occurrenceId = occurrenceId,
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:02:00Z",
            moveStart = "1970-01-01T02:00:00Z",
            moveEnd = "1970-01-01T03:00:00Z",
            updatedAt = "1970-01-01T01:02:00Z",
        )

        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                olderCompleted,
                message = "Older completion",
            ),
        )
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertEquals(ItemStatus.COMPLETED, store.state.value.recurrenceOutcomes[occurrenceId]?.status)
        assertEquals(
            olderCompleted.endedAt,
            store.state.value.recurrenceCompletionAnchors[CANONICAL_ITEM_ID],
        )

        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                4,
                null,
                newerDeferred,
                message = "Newer defer",
            ),
        )
        assertEquals(ItemStatus.SCHEDULED, store.state.value.schedule.single().status)
        assertTrue(store.state.value.recurrenceOutcomes.isEmpty())
        assertTrue(store.state.value.recurrenceCompletionAnchors.isEmpty())

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item,
                    block.copy(id = "77777777-7777-4777-8777-777777777777"),
                    "cursor-1",
                ),
            ),
        )

        val state = store.state.value
        assertEquals(ItemStatus.SCHEDULED, state.schedule.single().status)
        assertTrue(state.recurrenceOutcomes.isEmpty())
        assertTrue(state.recurrenceCompletionAnchors.isEmpty())
        assertEquals(2, state.terminalExecutionOutcomes.size)
        val deferredOutcome = state.terminalExecutionOutcomes.getValue(newerDeferred.id)
        assertFalse(deferredOutcome.requiresCanonicalItemProjection)
        assertNull(deferredOutcome.canonicalProjectionRevision)
        assertNull(deferredOutcome.canonicalProjectionResolution)
        assertNull(deferredOutcome.canonicalProjectionConflict)
        assertNull(deferredOutcome.canonicalProjectionRetryAuthorizedAt)
    }

    @Test
    fun newerDeferredClosureSuppressesNewProjectionButPreservesExistingMutationUncertainty() {
        val initial = DayWeaveUiState(
            canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalDeltaCursor = "cursor-0",
            schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
        )
        val baseStore = PlannerStore(initial)
        val running = executionSession(
            status = "active",
            revision = 1,
            projectionEligible = true,
        )
        requireNotNull(
            baseStore.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                1,
                running,
                message = "Older session active",
            ),
        )
        val olderCompleted = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        requireNotNull(
            baseStore.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                olderCompleted,
                message = "Older completion",
            ),
        )
        val beforeDeferred = baseStore.state.value
        assertTrue(
            beforeDeferred.terminalExecutionOutcomes.getValue(olderCompleted.id)
                .requiresCanonicalItemProjection,
        )
        val matchingPending = canonicalMutation(
            targetStatus = "completed",
            displayStatus = ItemStatus.COMPLETED,
            terminalExecutionSessionId = olderCompleted.id,
        )
        val newerDeferred = executionSession(status = "active", revision = 1).copy(
            id = "33333333-3333-4333-8333-333333333333",
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:02:00Z",
            moveStart = "1970-01-01T02:00:00Z",
            moveEnd = "1970-01-01T03:00:00Z",
            updatedAt = "1970-01-01T01:02:00Z",
        )

        val unstagedStore = PlannerStore(beforeDeferred)
        requireNotNull(
            unstagedStore.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                4,
                null,
                newerDeferred,
                message = "Newer defer",
            ),
        )
        assertFalse(
            unstagedStore.state.value.isNewestExecutionForProjection(olderCompleted),
        )
        assertTrue(
            unstagedStore.state.value.isNewestExecutionForProjection(newerDeferred),
        )
        assertFalse(unstagedStore.hasCredentialReplacementBlocker())
        assertTrue(
            runCatching { unstagedStore.stageCanonicalMutation(matchingPending) }.isFailure,
        )

        val matchingPendingStore = PlannerStore(beforeDeferred)
        requireNotNull(matchingPendingStore.stageCanonicalMutation(matchingPending))
        requireNotNull(
            matchingPendingStore.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                4,
                null,
                newerDeferred,
                message = "Newer defer",
            ),
        )
        // Staging precedes network I/O, so the exact journal may already be in flight and cannot
        // be discarded by a read-only execution refresh. It remains fenced for reconciliation.
        assertEquals(matchingPending, matchingPendingStore.state.value.pendingCanonicalMutation)
        assertEquals(ItemStatus.SCHEDULED, matchingPendingStore.state.value.schedule.single().status)

        val unrelatedPending = canonicalMutation(
            targetStatus = "planned",
            displayStatus = ItemStatus.SCHEDULED,
        )
        val unrelatedPendingStore = PlannerStore(beforeDeferred)
        requireNotNull(unrelatedPendingStore.stageCanonicalMutation(unrelatedPending))
        requireNotNull(
            unrelatedPendingStore.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                4,
                null,
                newerDeferred,
                message = "Newer defer",
            ),
        )
        assertEquals(unrelatedPending, unrelatedPendingStore.state.value.pendingCanonicalMutation)
    }

    @Test
    fun authoritativeOpenLeaseWinsProjectionDespiteOlderTimestampAndId() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val terminal = executionSession(status = "active", revision = 1).copy(
            id = "33333333-3333-4333-8333-333333333333",
            status = "completed",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        val active = executionSession(status = "active", revision = 1).copy(
            id = "22222222-2222-4222-8222-222222222221",
            startedAt = "1970-01-01T00:59:00Z",
            runningSince = "1970-01-01T00:59:00Z",
            createdAt = "1970-01-01T00:59:00Z",
            updatedAt = "1970-01-01T00:59:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                terminal,
                message = "Older completion",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                3,
                active,
                message = "New active lease",
            ),
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-1",
                ),
            ),
        )

        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
        assertEquals(active.id, store.state.value.activeSession?.canonicalExecutionSessionId)
        assertFalse(store.state.value.isNewestExecutionForProjection(terminal))
        assertTrue(store.state.value.isNewestExecutionForProjection(active))
    }

    @Test
    fun firstCanonicalPlanRetainsExecutionHistoryAlreadyBoundToTheSameCredentials() {
        val store = PlannerStore()
        val terminal = executionSession(status = "active", revision = 1).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Execution bootstrapped first",
            ),
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 7),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7),
                    cursor = "cursor-1",
                ).copy(configurationId = "connection-1"),
            ),
        )

        assertTrue(EXECUTION_ID in store.state.value.terminalExecutionOutcomes)
        assertFalse(
            store.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .requiresCanonicalItemProjection,
        )
        assertNull(
            store.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID).session
                .canonicalProjectionEligibleAtLeaseStart,
        )
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
    }

    @Test
    fun leaseEligibilitySurvivesRevisionAdvanceBeforeTerminalHistoryArrives() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val running = executionSession(status = "active", revision = 1, projectionEligible = true)
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = running,
                message = "Running",
            ),
        )
        assertEquals(
            true,
            store.state.value.canonicalExecutionSession
                ?.canonicalProjectionEligibleAtLeaseStart,
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 8),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 8),
                    cursor = "cursor-1",
                ),
            ),
        )
        val terminal = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:01:30Z",
            updatedAt = "1970-01-01T01:01:30Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Ended",
            ),
        )

        val outcome = store.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertTrue(outcome.requiresCanonicalItemProjection)
        assertEquals(7L, outcome.session.itemRevision)
        assertEquals(true, outcome.session.canonicalProjectionEligibleAtLeaseStart)
        assertTrue(store.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 8),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 8),
                    cursor = "cursor-2",
                ),
            ),
        )
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
    }

    @Test
    fun remoteLeaseFromOlderItemRevisionGetsAnActionableDurablePlaceholder() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 8)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 8)),
                schedulePlanningZoneId = "UTC",
            ),
        )
        val remote = executionSession(status = "active", revision = 1)

        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = remote,
                message = "Remote lease",
            ),
        )

        val placeholder = requireNotNull(store.state.value.activeItem)
        assertEquals(EXECUTION_ID, placeholder.id)
        assertEquals(7L, placeholder.canonicalRevision)
        assertEquals("remote_execution_lease", placeholder.canonicalBlockKind)
        assertEquals(ItemStatus.ACTIVE, placeholder.status)
        assertEquals(EXECUTION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
        assertNull(
            store.state.value.canonicalExecutionSession
                ?.canonicalProjectionEligibleAtLeaseStart,
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 9),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 9),
                    cursor = "cursor-new",
                ).copy(configurationId = "connection-1"),
            ),
        )
        assertEquals(EXECUTION_ID, store.state.value.activeItem?.id)
        assertEquals(2, store.state.value.schedule.size)
    }

    @Test
    fun keepLatestResolutionSurvivesRestartAndSuppressesSameRevisionHistoryOverlay() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val running = executionSession(status = "active", revision = 1, projectionEligible = true)
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                1,
                running,
                message = "Running",
            ),
        )
        val terminal = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                terminal,
                message = "Ended",
            ),
        )
        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-1",
                ).copy(
                    schedule = emptyList(),
                    unscheduledItemCount = 1,
                    unscheduledWork = listOf(
                        UnscheduledWorkSnapshot(
                            itemId = CANONICAL_ITEM_ID,
                            remainingMinutes = 60,
                            reason = "capacity",
                        ),
                    ),
                ),
            ),
        )
        requireNotNull(
            store.recordTerminalProjectionConflict(
                EXECUTION_ID,
                "The same-revision item is only partially scheduled.",
            ),
        )
        requireNotNull(store.keepLatestItemAfterTerminalConflict(EXECUTION_ID))

        val restarted = PlannerStore(
            store.state.value.copy(
                canonicalConfigurationId = "connection-1",
                canonicalExecutionHistoryContinuityEstablished = true,
                canonicalExecutionHistoryVerified = true,
            ),
        )
        requireNotNull(
            restarted.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-2",
                ).copy(configurationId = "connection-1"),
            ),
        )
        assertEquals(ItemStatus.SCHEDULED, restarted.state.value.schedule.single().status)
        // A fresh preview without a publication commit stays visible but cannot be started.
        assertTrue(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        requireNotNull(
            restarted.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                3,
                null,
                terminal,
                message = "History refreshed",
            ),
        )
        assertEquals(ItemStatus.SCHEDULED, restarted.state.value.schedule.single().status)
        assertTrue(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        assertEquals(
            "user_kept_latest_item",
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionResolution,
        )
    }

    @Test
    fun retryAuthorizationIsDurableAndOneShot() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val running = executionSession(status = "active", revision = 1, projectionEligible = true)
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                1,
                running,
                message = "Running",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                running.copy(
                    status = "completed",
                    revision = 2,
                    accumulatedSeconds = 60,
                    actualSeconds = 60,
                    runningSince = null,
                    endedAt = "1970-01-01T01:01:00Z",
                    updatedAt = "1970-01-01T01:01:00Z",
                ),
                message = "Ended",
            ),
        )
        requireNotNull(
            store.recordTerminalProjectionConflict(EXECUTION_ID, "Approval is required."),
        )
        requireNotNull(store.authorizeTerminalProjectionRetry(EXECUTION_ID))

        val restarted = PlannerStore(store.state.value)
        assertNotNull(
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionRetryAuthorizedAt,
        )
        requireNotNull(
            restarted.recordTerminalProjectionConflict(EXECUTION_ID, "Approval is required."),
        )
        assertNull(
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionRetryAuthorizedAt,
        )
    }

    @Test
    fun terminalLedgerNeverEvictsAnImmutableSessionOutcome() {
        val unresolvedSessionId = UUID.nameUUIDFromBytes("unresolved".toByteArray()).toString()
        val initial = DayWeaveUiState(
            canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
        )
        val store = PlannerStore(initial)
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminalExecution(
                    sessionId = unresolvedSessionId,
                    itemId = CANONICAL_ITEM_ID,
                    endedAt = "1970-01-01T00:00:01Z",
                ),
                message = "Unresolved",
            ),
        )
        val base = Instant.parse("1970-01-02T00:00:00Z")
        val historyIds = (0 until 256).map { index ->
            val sessionId = UUID.nameUUIDFromBytes("history-session-$index".toByteArray()).toString()
            val itemId = UUID.nameUUIDFromBytes("history-item-$index".toByteArray()).toString()
            requireNotNull(
                store.reconcileCanonicalExecution(
                    syncOrigin = CANONICAL_ORIGIN,
                    configurationId = "connection-1",
                    revision = 4L + index * 2L,
                    activeSession = null,
                    changedSession = terminalExecution(
                        sessionId = sessionId,
                        itemId = itemId,
                        endedAt = base.plusSeconds(maxOf(0, index - 1).toLong()).toString(),
                    ),
                    message = "History",
                ),
            )
            sessionId
        }

        val retained = store.state.value.terminalExecutionOutcomes
        assertEquals(257, retained.size)
        assertTrue(unresolvedSessionId in retained)
        assertTrue(historyIds.first() in retained)
        assertTrue(historyIds.last() in retained)
        assertTrue(retained.values.all { it.session.revision == 2L })
    }

    @Test
    fun terminalSplitDoesNotResolveOccurrenceWhileAuthoritativeMinutesRemainUnscheduled() {
        val occurrenceId = "66666666-6666-4666-8666-666666666666"
        val block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7).copy(
            occurrenceId = occurrenceId,
            durationMinutes = 30,
            isSplittable = true,
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(
                    canonicalItem(status = "planned", revision = 7).copy(
                        recurrenceJson = "{\"frequency\":\"daily\"}",
                        splitPolicyJson = "{\"type\":\"splittable\"}",
                    ),
                ),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                schedule = listOf(block),
                unscheduledWork = listOf(
                    UnscheduledWorkSnapshot(
                        itemId = CANONICAL_ITEM_ID,
                        occurrenceId = occurrenceId,
                        remainingMinutes = 90,
                        reason = "capacity",
                    ),
                ),
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            ),
        )
        val terminal = executionSession(status = "active", revision = 1).copy(
            occurrenceId = occurrenceId,
            plannedBlockId = CANONICAL_BLOCK_ID,
            status = "completed",
            revision = 2,
            accumulatedSeconds = 1_800,
            actualSeconds = 1_800,
            runningSince = null,
            endedAt = "1970-01-01T01:30:00Z",
            updatedAt = "1970-01-01T01:30:00Z",
        )

        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "One split ended",
            ),
        )

        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertFalse(occurrenceId in store.state.value.recurrenceOutcomes)
        assertFalse(CANONICAL_ITEM_ID in store.state.value.recurrenceCompletionAnchors)
    }

    @Test
    fun willDoLaterEndsSessionAndMovesItemOneHour() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val original = store.state.value.activeItem ?: error("Preview must have an active item")

        store.doActiveLater()

        val moved = store.state.value.schedule.first { it.id == original.id }
        assertNull(store.state.value.activeSession)
        assertEquals(ItemStatus.SCHEDULED, moved.status)
        assertEquals(original.startMinute + 60, moved.startMinute)
    }

    @Test
    fun invalidOrSupersededRestoredDeferIntentsAreAbandonedWithoutChangingLeaseTruth() {
        val valid = pendingExecutionDeferState()
        val intent = requireNotNull(valid.pendingExecutionDeferIntent)
        val session = requireNotNull(valid.canonicalExecutionSession)
        val currentProof = requireNotNull(valid.publishedScheduleProof)
        val invalidStates = listOf(
            "legacy local approval schema" to valid.copy(
                pendingExecutionDeferIntent = intent.copy(schemaVersion = 0),
            ),
            "malformed timestamp" to valid.copy(
                pendingExecutionDeferIntent = intent.copy(moveStart = "not-an-instant"),
            ),
            "missing lease" to valid.copy(canonicalExecutionSession = null),
            "mismatched lease" to valid.copy(
                canonicalExecutionSession = session.copy(
                    id = "66666666-6666-4666-8666-666666666666",
                ),
            ),
            "binding mismatch" to valid.copy(
                canonicalExecutionConfigurationId = "connection-2",
            ),
            "legacy item-only publication proof" to valid.copy(
                publishedScheduleProof = currentProof.copy(
                    schemaVersion = 1,
                    blocks = currentProof.blocks.map { it.copy(immutableDigest = null) },
                ),
            ),
        )

        invalidStates.forEach { (label, restored) ->
            val store = PlannerStore(restored, nowEpochMillis = { 3_600_000L })

            assertNull(label, store.state.value.pendingExecutionDeferIntent)
            assertEquals(
                label,
                restored.canonicalExecutionSession,
                store.state.value.canonicalExecutionSession,
            )
            assertTrue(label, store.state.value.scheduleMessage.contains("abandoned safely"))
        }
    }

    @Test
    fun legacyPublicationProofCannotStageExecutionDeferIntent() {
        val pending = pendingExecutionDeferState()
        val intent = requireNotNull(pending.pendingExecutionDeferIntent)
        val currentProof = requireNotNull(pending.publishedScheduleProof)
        val store = PlannerStore(
            pending.copy(
                pendingExecutionDeferIntent = null,
                publishedScheduleProof = currentProof.copy(
                    schemaVersion = 1,
                    blocks = currentProof.blocks.map { it.copy(immutableDigest = null) },
                ),
            ),
            nowEpochMillis = { 3_600_000L },
        )

        assertThrows(IllegalArgumentException::class.java) {
            store.stageExecutionDeferIntent(intent)
        }
        assertNull(store.state.value.pendingExecutionDeferIntent)
    }

    @Test
    fun abandonedInvalidDeferIntentCannotWedgeCredentialQuarantine() = runBlocking {
        val restored = pendingExecutionDeferState().copy(
            canonicalExecutionConfigurationId = "superseded-connection",
        )
        val store = PlannerStore(restored, nowEpochMillis = { 3_600_000L })

        assertNull(store.state.value.pendingExecutionDeferIntent)
        assertFalse(store.hasCredentialReplacementBlocker())
        val abandonment = requireNotNull(store.abandonCanonicalConnection())
        assertTrue(abandonment.awaitDurable())
        assertNull(store.state.value.canonicalSyncOrigin)
        assertNull(store.state.value.canonicalExecutionSession)
        assertTrue(store.state.value.schedule.isEmpty())
    }

    private fun remoteSuggestion() = PlanningSuggestion(
        id = "remote-proposal",
        title = "Protect recovery time",
        summary = "Keep an hour open",
        source = "Codex",
        kind = SuggestionKind.SCHEDULE_CHANGE,
        expiresInDays = 7,
        remoteRevision = 1,
        remotePayloadJson = "{}",
    )

    private fun canonicalItem(status: String, revision: Long) = CanonicalItemSnapshot(
        id = CANONICAL_ITEM_ID,
        kind = "task",
        status = status,
        title = "Canonical timer",
        timezoneName = "UTC",
        durationSeconds = 3_600,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = revision,
        createdAt = "1970-01-01T00:00:00Z",
        updatedAt = "1970-01-01T00:00:00Z",
    )

    private fun canonicalBlock(status: ItemStatus, revision: Long) = ScheduleItem(
        id = CANONICAL_BLOCK_ID,
        title = "Canonical timer",
        kind = ItemKind.TASK,
        startMinute = 60,
        durationMinutes = 60,
        status = status,
        canonicalItemId = CANONICAL_ITEM_ID,
        canonicalRevision = revision,
        sessionIndex = 0,
        absoluteStartAt = "1970-01-01T01:00:00Z",
        absoluteEndAt = "1970-01-01T02:00:00Z",
        planningZoneId = "UTC",
        canonicalBlockKind = "planned",
    )

    private fun canonicalUpdate(
        item: CanonicalItemSnapshot,
        block: ScheduleItem,
        cursor: String,
    ) = CanonicalPlanUpdate(
        items = listOf(item),
        schedule = listOf(block),
        syncOrigin = CANONICAL_ORIGIN,
        configurationId = "connection-1",
        deltaCursor = cursor,
        inputDigest = "sha256:${"a".repeat(64)}",
        generatedAt = "1970-01-01T00:00:00Z",
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
        planOccurrenceMembership = emptyList(),
        hasExactPlanOccurrenceMembership = true,
        message = "Updated",
    )

    private fun publication(candidate: CanonicalPlanUpdate): PendingSchedulePublication {
        val idempotencyKey = "88888888-8888-4888-8888-888888888888"
        val schedule = SchedulePreviewRequest(
            asOf = candidate.generatedAt,
            horizonStart = "1970-01-01T00:00:00Z",
            horizonEnd = "1970-01-02T00:00:00Z",
            timezoneName = candidate.planningZoneId,
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = "1970-01-01T00:00:00Z",
                    end = "1970-01-02T00:00:00Z",
                ),
            ),
        )
        val configuration = AuthenticatedApiConfiguration.createBound(
            CANONICAL_ORIGIN,
            "synthetic-token",
            "connection-1",
        )
        return PendingSchedulePublication(
            schemaVersion = 1,
            idempotencyKey = idempotencyKey,
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            preparedAt = "1970-01-01T00:00:00Z",
            request = buildSchedulePublishHttpRequest(
                configuration,
                SchedulePublishRequest(idempotencyKey, candidate.inputDigest, schedule),
            ),
            candidate = candidate,
        )
    }

    private fun habitOccurrenceForPublication(
        outcome: HabitOutcomeSnapshot? = null,
    ) = HabitOccurrenceSnapshot(
        evidence = HabitOccurrenceEvidenceSnapshot(
            id = "12121212-1212-4121-8121-121212121212",
            habitId = CANONICAL_ITEM_ID,
            plannerOccurrenceId = "13131313-1313-5131-8131-131313131313",
            sourceScheduleRevisionId = "14141414-1414-4141-8141-141414141414",
            sourceItemRevision = 7,
            policyFingerprint = "sha256:${"b".repeat(64)}",
            identity = JsonObject(
                mapOf(
                    "type" to JsonPrimitive("calendar_day"),
                    "date" to JsonPrimitive("1970-01-01"),
                    "bucket_ordinal" to JsonPrimitive(0),
                ),
            ),
            nominalStart = "1970-01-01T01:00:00Z",
            nominalEnd = "1970-01-01T01:30:00Z",
            windowStart = "1970-01-01T00:30:00Z",
            windowEnd = "1970-01-01T02:00:00Z",
            localDate = "1970-01-01",
            timezoneName = "UTC",
            expectedDurationSeconds = 1_800,
            expectedQuantity = null,
            expectedUnit = null,
        ),
        outcome = outcome,
    )

    private fun publishedRevision() = PublishedScheduleRevisionSnapshot(
        id = "77777777-7777-4777-8777-777777777777",
        revision = "1:77777777-7777-4777-8777-777777777777",
        revisionNumber = 1uL,
        inputDigest = "sha256:${"a".repeat(64)}",
        horizonStart = "1970-01-01T00:00:00Z",
        horizonEnd = "1970-01-02T00:00:00Z",
        timezoneName = "UTC",
        publishedAt = "1970-01-01T00:00:00Z",
    )

    private fun publishedCanonicalState(
        item: CanonicalItemSnapshot = canonicalItem("planned", 7),
        block: ScheduleItem = canonicalBlock(ItemStatus.SCHEDULED, 7),
    ): DayWeaveUiState {
        val revision = publishedRevision()
        return DayWeaveUiState(
            canonicalItems = listOf(item),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalDeltaCursor = "cursor-1",
            schedule = listOf(block),
            publishedScheduleRevision = revision,
            publishedScheduleProof = publishedProof(block, revision),
            publishedScheduleRevisionHint = PublishedScheduleRevisionHintSnapshot(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revisionNumber = revision.revisionNumber,
            ),
            scheduleInputDigest = revision.inputDigest,
            scheduleGeneratedAt = "1970-01-01T00:00:00Z",
            schedulePlanningZoneId = "UTC",
        )
    }

    private fun pendingExecutionDeferState(): DayWeaveUiState {
        val block = canonicalBlock(ItemStatus.PAUSED, 7)
        val session = executionSession("paused", 2).copy(
            runningSince = null,
            pausedAt = "1970-01-01T01:05:00Z",
            accumulatedSeconds = 300,
            updatedAt = "1970-01-01T01:05:00Z",
        )
        return publishedCanonicalState(block = block).copy(
            canonicalExecutionSyncOrigin = CANONICAL_ORIGIN,
            canonicalExecutionConfigurationId = "connection-1",
            canonicalExecutionRevision = session.revision,
            canonicalExecutionSession = session,
            pendingExecutionDeferIntent = PendingExecutionDeferIntent(
                schemaVersion = 1,
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                sessionId = session.id,
                itemId = session.itemId,
                itemRevision = session.itemRevision,
                sessionIndex = session.sessionIndex,
                plannedBlockId = requireNotNull(session.plannedBlockId),
                sourceDeviceId = session.sourceDeviceId,
                focusedBlockId = block.id,
                sourceStart = requireNotNull(block.absoluteStartAt),
                sourceEnd = requireNotNull(block.absoluteEndAt),
                moveStart = "1970-01-01T03:00:00Z",
                stagedAt = "1970-01-01T01:05:00Z",
            ),
        )
    }

    private fun occurrenceSource() = RecurrenceOccurrenceSourceSnapshot(
        itemId = CANONICAL_ITEM_ID,
        itemRevision = 7,
        identityJson =
            """{"type":"calendar_day","date":"1970-01-01","bucket_ordinal":0}""",
        nominalStart = "1970-01-01T01:00:00Z",
        nominalEnd = "1970-01-01T02:00:00Z",
        localDate = "1970-01-01",
        ordinal = 0,
    )

    private fun publishedProof(
        block: ScheduleItem,
        revision: PublishedScheduleRevisionSnapshot = publishedRevision(),
    ) = PublishedScheduleProofSnapshot(
        schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
        syncOrigin = CANONICAL_ORIGIN,
        configurationId = "connection-1",
        revision = revision,
        asOf = "1970-01-01T00:00:00Z",
        blocks = listOf(
            PublishedScheduleBlockProofSnapshot.from(block),
        ),
    )

    private fun canonicalMutation(
        targetStatus: String,
        displayStatus: ItemStatus,
        targetIsSensitive: Boolean = false,
        focusedBlockId: String = CANONICAL_BLOCK_ID,
        terminalExecutionSessionId: String? = null,
    ) = PendingCanonicalMutation(
        idempotencyKey = "99999999-9999-4999-8999-999999999999",
        syncOrigin = CANONICAL_ORIGIN,
        configurationId = "connection-1",
        itemId = CANONICAL_ITEM_ID,
        expectedRevision = 7,
        targetStatus = targetStatus,
        targetIsSensitive = targetIsSensitive,
        startedAt = "1970-01-01T00:00:00Z",
        replacementRequestJson = "{}",
        focusedBlockId = focusedBlockId,
        displayStatus = displayStatus,
        terminalExecutionSessionId = terminalExecutionSessionId,
    )

    private fun assertPublishedPlanInvalidated(store: PlannerStore) {
        assertNull(store.state.value.publishedScheduleRevision)
        assertNull(store.state.value.publishedScheduleProof)
        assertNull(store.state.value.scheduleInputDigest)
    }

    private fun assertTerminalExecutionSurvivesComposition(
        wireStatus: String,
        displayStatus: ItemStatus,
    ) {
        val initial = DayWeaveUiState(
            canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalDeltaCursor = "cursor-0",
            schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
        )
        val store = PlannerStore(initial)
        val running = executionSession(
            status = "active",
            revision = 1,
            projectionEligible = true,
        ).copy(
            plannedBlockId = "33333333-3333-4333-8333-333333333333",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = running,
                message = "Running",
            ),
        )
        val terminal = running.copy(
            status = wireStatus,
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:01:30Z",
            updatedAt = "1970-01-01T01:01:30Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Ended",
            ),
        )
        val outcome = requireNotNull(store.state.value.terminalExecutionOutcomes[EXECUTION_ID])
        assertTrue(outcome.requiresCanonicalItemProjection)
        assertEquals(displayStatus, store.state.value.schedule.single().status)

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 7),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7),
                    cursor = "cursor-1",
                ),
            ),
        )

        assertEquals(displayStatus, store.state.value.schedule.single().status)
        val restarted = PlannerStore(store.state.value)
        assertEquals(displayStatus, restarted.state.value.schedule.single().status)
        assertTrue(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
    }

    private fun executionSession(
        status: String,
        revision: Long,
        projectionEligible: Boolean = false,
    ) = CanonicalExecutionSessionSnapshot(
        id = EXECUTION_ID,
        itemId = CANONICAL_ITEM_ID,
        itemRevision = 7,
        sessionIndex = 0,
        plannedBlockId = CANONICAL_BLOCK_ID,
        sourceDeviceId = DEVICE_ID,
        status = status,
        revision = revision,
        accumulatedSeconds = 0,
        startedAt = "1970-01-01T01:00:00Z",
        runningSince = "1970-01-01T01:00:00Z",
        createdAt = "1970-01-01T01:00:00Z",
        updatedAt = "1970-01-01T01:00:00Z",
        canonicalProjectionEligibleAtLeaseStart = projectionEligible.takeIf { it },
    )

    private fun terminalExecution(
        sessionId: String,
        itemId: String,
        endedAt: String,
    ) = CanonicalExecutionSessionSnapshot(
        id = sessionId,
        itemId = itemId,
        itemRevision = 7,
        sessionIndex = 0,
        plannedBlockId = null,
        sourceDeviceId = DEVICE_ID,
        status = "completed",
        revision = 2,
        accumulatedSeconds = 60,
        actualSeconds = 60,
        startedAt = "1970-01-01T00:00:00Z",
        runningSince = null,
        endedAt = endedAt,
        createdAt = "1970-01-01T00:00:00Z",
        updatedAt = endedAt,
    )

    private companion object {
        const val CANONICAL_ORIGIN = "https://api.example.test/"
        const val CANONICAL_ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val CANONICAL_BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val EXECUTION_ID = "44444444-4444-4444-8444-444444444444"
        const val DEVICE_ID = "55555555-5555-4555-8555-555555555555"
    }
}
