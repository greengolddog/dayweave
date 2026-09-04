package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.HabitApiException
import com.greengolddog.dayweave.network.HabitTransport
import com.greengolddog.dayweave.network.RemoteHabitAnalytics
import com.greengolddog.dayweave.network.RemoteHabitAnalyticsBucket
import com.greengolddog.dayweave.network.RemoteHabitDeltaPage
import com.greengolddog.dayweave.network.RemoteHabitMutation
import com.greengolddog.dayweave.network.RemoteHabitOccurrence
import com.greengolddog.dayweave.network.RemoteHabitOccurrenceEvidence
import com.greengolddog.dayweave.network.RemoteHabitOccurrencePage
import com.greengolddog.dayweave.network.RemoteHabitOutcome
import com.greengolddog.dayweave.network.RemoteHabitOutcomeStatus
import com.greengolddog.dayweave.network.RemoteHabitPause
import com.greengolddog.dayweave.network.RemoteHabitSupportiveFactCode
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import java.time.LocalDate
import java.util.UUID
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HabitSyncManagerTest {
    @Test
    fun outcomeStageIsDurableBeforeSuccessAndDoesNotTouchTheNetwork() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport()
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.stageOutcome(
                habitId = HABIT_ID,
                occurrenceId = OCCURRENCE_ID,
                observedOutcomeRevision = 0,
                outcome = completedInput(),
            ),
        )

        val pending = store.durableState.value?.habitLedger?.pendingMutations?.single()
        assertEquals(OPERATION_ID, pending?.idempotencyKey)
        assertEquals(0L, pending?.expectedRevision)
        assertEquals(emptyList<String>(), transport.outcomeBodies)
    }

    @Test
    fun outcomeStageRejectsTheRevisionThatWasObservedBeforeABackgroundRefresh() = runBlocking {
        val store = boundStore(outcome = completedOutcome())
        val transport = FakeHabitTransport()
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.INVALID_LOCAL_STATE,
            manager.stageOutcome(
                habitId = HABIT_ID,
                occurrenceId = OCCURRENCE_ID,
                observedOutcomeRevision = 0,
                outcome = completedInput(),
            ),
        )

        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        assertTrue(transport.outcomeBodies.isEmpty())
        assertEquals(
            1L,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID).outcome?.revision,
        )
    }

    @Test
    fun ambiguousOutcomeReplaysTheExactDurableBodyAndKey() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport()
        var attempts = 0
        transport.outcomeHandler = { _, _, key, body ->
            attempts += 1
            if (attempts == 1) throw IOException("synthetic response loss")
            assertEquals(OPERATION_ID, key)
            assertTrue(body.contains("\"progress_basis_points\":10000"))
            RemoteHabitMutation(
                remoteOccurrence(outcome = completedOutcome()),
                replayed = true,
            )
        }
        val first = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            first.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        val pending = store.state.value.habitLedger.pendingMutations.single()

        val relaunched = manager(store, transport, emptyList())
        assertEquals(HabitSyncOutcome.SUCCESS, relaunched.refresh())

        assertEquals(listOf(pending.requestJson, pending.requestJson), transport.outcomeBodies)
        assertEquals(listOf(OPERATION_ID, OPERATION_ID), transport.outcomeKeys)
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
        assertEquals(
            HabitOutcomeStatusSnapshot.COMPLETED,
            store.state.value.habitLedger.occurrences.getValue(OCCURRENCE_ID).outcome?.status,
        )
    }

    @Test
    fun deterministicConflictRemainsEncryptedForReviewAndIsNotReplayed() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ -> throw HabitApiException.Conflict() }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.CONFLICT,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        assertEquals(
            PendingHabitMutationDisposition.CONFLICT,
            store.state.value.habitLedger.pendingMutations.single().disposition,
        )
        assertEquals(HabitSyncOutcome.CONFLICT, manager.refresh())
        assertEquals(1, transport.outcomeBodies.size)

        assertEquals(HabitSyncOutcome.SUCCESS, manager.discardReviewedMutation(OPERATION_ID))
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun pauseStartAndResumeUseIndependentExactRevisionedCommands() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            startPauseHandler = { _, key, _ ->
                assertEquals(OPERATION_ID, key)
                RemoteHabitMutation(remotePause(revision = 1), replayed = false)
            }
            resumePauseHandler = { _, pauseId, key, _ ->
                assertEquals(PAUSE_ID, pauseId)
                assertEquals(SECOND_OPERATION_ID, key)
                RemoteHabitMutation(
                    remotePause(revision = 2, endedAt = RESUMED_AT),
                    replayed = false,
                )
            }
        }
        val manager = manager(
            store,
            transport,
            listOf(OPERATION_ID, PAUSE_ID, SECOND_OPERATION_ID),
            times = listOf(Instant.parse(PAUSED_AT), Instant.parse(RESUMED_AT)),
        )

        assertEquals(HabitSyncOutcome.SUCCESS, manager.startPause(HABIT_ID))
        assertNull(store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt)
        assertEquals(HabitSyncOutcome.SUCCESS, manager.resumePause(HABIT_ID, PAUSE_ID))
        assertEquals(RESUMED_AT, store.state.value.habitLedger.pauses.getValue(PAUSE_ID).endedAt)
        assertEquals(1, transport.startPauseBodies.size)
        assertEquals(1, transport.resumePauseBodies.size)
        assertTrue(transport.startPauseBodies.single().contains("\"expected_revision\":0"))
        assertTrue(transport.resumePauseBodies.single().contains("\"expected_revision\":1"))
    }

    @Test
    fun paginatedOccurrenceLoadMergesEveryPageWithoutMovingTheDeltaCursor() = runBlocking {
        val store = boundStore()
        val second = remoteOccurrence(id = SECOND_OCCURRENCE_ID)
        val transport = FakeHabitTransport().apply {
            occurrencePages += RemoteHabitOccurrencePage(
                occurrences = listOf(remoteOccurrence()),
                nextCursor = "page-2",
                hasMore = true,
            )
            occurrencePages += RemoteHabitOccurrencePage(
                occurrences = listOf(second),
                nextCursor = null,
                hasMore = false,
            )
        }
        val manager = manager(store, transport, emptyList())

        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager.loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(setOf(OCCURRENCE_ID, SECOND_OCCURRENCE_ID),
            store.state.value.habitLedger.occurrences.keys)
        assertEquals("cursor-0", store.state.value.habitLedger.deltaCursor)
        assertEquals(listOf(null, "page-2"), transport.occurrenceCursors)
    }

    @Test
    fun unresolvedHabitWriteBlocksCredentialReplacement() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ -> throw IOException("offline") }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        assertTrue(store.hasCredentialReplacementBlocker())
        assertFalse(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    @Test
    fun laterOfflineActionIsDurableBeforeAnOlderAmbiguousWriteIsRetried() = runBlocking {
        val store = boundStore().also { bound ->
            bound.applyHabitDeltaPage(
                ORIGIN,
                CONFIGURATION_ID,
                listOf(HabitOccurrenceSnapshot.fromRemote(remoteOccurrence(SECOND_OCCURRENCE_ID))),
                emptyList(),
                "cursor-1",
            )
        }
        val transport = FakeHabitTransport().apply {
            outcomeHandler = { _, _, _, _ -> throw IOException("offline") }
        }
        val manager = manager(store, transport, listOf(OPERATION_ID, SECOND_OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.recordOutcome(HABIT_ID, OCCURRENCE_ID, 0, completedInput()),
        )
        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager.recordOutcome(HABIT_ID, SECOND_OCCURRENCE_ID, 0, completedInput()),
        )

        assertEquals(
            setOf(OCCURRENCE_ID, SECOND_OCCURRENCE_ID),
            store.state.value.habitLedger.pendingMutations.mapTo(mutableSetOf()) { it.targetId },
        )
        assertEquals(
            listOf(OPERATION_ID, OPERATION_ID),
            transport.outcomeKeys,
        )
    }

    @Test
    fun rejectedDurableDeltaCursorIsClearedOnceAndReplayedFromGenesis() = runBlocking {
        val store = boundStore()
        var attempts = 0
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                attempts += 1
                if (attempts == 1) {
                    assertEquals("cursor-0", cursor)
                    throw HabitApiException.Validation(400)
                }
                assertNull(cursor)
                RemoteHabitDeltaPage(emptyList(), "cursor-repaired", hasMore = false)
            }
        }

        assertEquals(HabitSyncOutcome.SUCCESS, manager(store, transport, emptyList()).refresh())

        assertEquals(listOf("cursor-0", null), transport.deltaCursors)
        assertEquals("cursor-repaired", store.state.value.habitLedger.deltaCursor)
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun intermediateDeltaCheckpointSurvivesPageTwoFailureButIsNotCaughtUp() = runBlocking {
        val store = boundStore()
        assertTrue(store.state.value.habitLedger.deltaCaughtUp)
        var failSecondPage = true
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                when (cursor) {
                    "cursor-0" -> RemoteHabitDeltaPage(
                        emptyList(),
                        "cursor-1",
                        hasMore = true,
                    )
                    "cursor-1" -> if (failSecondPage) {
                        throw IOException("synthetic page-two failure")
                    } else {
                        RemoteHabitDeltaPage(emptyList(), "cursor-2", hasMore = false)
                    }
                    else -> error("unexpected cursor")
                }
            }
        }

        assertEquals(
            HabitSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            manager(store, transport, emptyList()).refresh(),
        )
        val durableIntermediate = requireNotNull(store.durableState.value)
        assertEquals("cursor-1", durableIntermediate.habitLedger.deltaCursor)
        assertFalse(durableIntermediate.habitLedger.deltaCaughtUp)

        failSecondPage = false
        val relaunchedStore = PlannerStore(durableIntermediate)
        assertEquals(
            HabitSyncOutcome.SUCCESS,
            manager(relaunchedStore, transport, emptyList()).refresh(),
        )
        assertEquals(listOf("cursor-0", "cursor-1", "cursor-1"), transport.deltaCursors)
        assertEquals("cursor-2", relaunchedStore.state.value.habitLedger.deltaCursor)
        assertTrue(relaunchedStore.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun occurrencePaginationRejectsOpaqueCursorCyclesBeforeRepeatingARequest() = runBlocking {
        val transport = FakeHabitTransport().apply {
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence()),
                "cycle_A",
                hasMore = true,
            )
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence()),
                "cycle_B",
                hasMore = true,
            )
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence()),
                "cycle_A",
                hasMore = true,
            )
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(boundStore(), transport, emptyList()).loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(listOf(null, "cycle_A", "cycle_B"), transport.occurrenceCursors)
    }

    @Test
    fun deltaPaginationRejectsOpaqueCursorCyclesBeforePersistingTheRepeat() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                RemoteHabitDeltaPage(
                    emptyList(),
                    when (cursor) {
                        "cursor-0" -> "cycle_A"
                        "cycle_A" -> "cycle_B"
                        "cycle_B" -> "cycle_A"
                        else -> error("unexpected cursor")
                    },
                    hasMore = true,
                )
            }
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).refresh(),
        )
        assertEquals(listOf("cursor-0", "cycle_A", "cycle_B"), transport.deltaCursors)
        assertEquals("cycle_B", store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun occurrencePaginationRejectsAMissingContinuingCursorBeforeMerging() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            occurrencePages += RemoteHabitOccurrencePage(
                listOf(remoteOccurrence(id = SECOND_OCCURRENCE_ID)),
                nextCursor = null,
                hasMore = true,
            )
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).loadHabit(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-02"),
            ),
        )
        assertEquals(setOf(OCCURRENCE_ID), store.state.value.habitLedger.occurrences.keys)
    }

    @Test
    fun terminalDeltaCannotMoveTheDurableCursorBackward() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            deltaHandler = { cursor ->
                when (cursor) {
                    "cursor-0" -> RemoteHabitDeltaPage(emptyList(), "cursor-a", hasMore = true)
                    "cursor-a" -> RemoteHabitDeltaPage(emptyList(), "cursor-0", hasMore = false)
                    else -> error("unexpected cursor")
                }
            }
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).refresh(),
        )
        assertEquals(listOf("cursor-0", "cursor-a"), transport.deltaCursors)
        assertEquals("cursor-a", store.state.value.habitLedger.deltaCursor)
        assertFalse(store.state.value.habitLedger.deltaCaughtUp)
    }

    @Test
    fun analyticsResponseMustMatchTheRequestedIdentityBeforeCaching() = runBlocking {
        val store = boundStore()
        val transport = FakeHabitTransport().apply {
            analyticsHandler = { _, start, end, bucket ->
                remoteAnalytics(
                    habitId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    startDate = start,
                    endDate = end,
                    bucket = bucket,
                )
            }
        }

        assertEquals(
            HabitSyncOutcome.PROTOCOL_FAILURE,
            manager(store, transport, emptyList()).refreshAnalytics(
                HABIT_ID,
                LocalDate.parse("2026-09-01"),
                LocalDate.parse("2026-09-01"),
                com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot.DAY,
            ),
        )
        assertTrue(store.state.value.habitLedger.analytics.isEmpty())
    }

    @Test
    fun mutationTimeMustUseMicrosecondsAndTheServerWindow() = runBlocking {
        val store = boundStore()
        val manager = manager(store, FakeHabitTransport(), listOf(OPERATION_ID))

        assertEquals(
            HabitSyncOutcome.INVALID_LOCAL_STATE,
            manager.recordOutcome(
                HABIT_ID,
                OCCURRENCE_ID,
                0,
                completedInput().copy(occurredAt = "2026-09-01T07:30:00.000000001Z"),
            ),
        )
        assertTrue(store.state.value.habitLedger.pendingMutations.isEmpty())
    }

    private fun boundStore(
        outcome: RemoteHabitOutcome? = null,
    ) = PlannerStore(DayWeaveUiState()).also { store ->
        store.bindHabitLedger(ORIGIN, CONFIGURATION_ID)
        store.applyHabitDeltaPage(
            ORIGIN,
            CONFIGURATION_ID,
            listOf(HabitOccurrenceSnapshot.fromRemote(remoteOccurrence(outcome = outcome))),
            emptyList(),
            "cursor-0",
            hasMore = false,
        )
    }

    private fun manager(
        store: PlannerStore,
        transport: FakeHabitTransport,
        uuids: List<String>,
        times: List<Instant> = listOf(NOW),
    ): HabitSyncManager {
        val uuidIterator = uuids.iterator()
        val timeIterator = times.iterator()
        var lastTime = times.last()
        return HabitSyncManager(
            plannerStore = store,
            credentialStore = GenerationBoundCredentialStore().apply {
                configurationId = CONFIGURATION_ID
            },
            transport = transport,
            now = {
                if (timeIterator.hasNext()) lastTime = timeIterator.next()
                lastTime
            },
            newUuid = { UUID.fromString(uuidIterator.next()) },
        )
    }

    private fun completedInput() = HabitOutcomeInputSnapshot(
        status = HabitOutcomeStatusSnapshot.COMPLETED,
        progressBasisPoints = 10_000,
        quantity = 8,
        unit = "pages",
        actualSeconds = 600,
        note = "Finished",
        occurredAt = OCCURRED_AT,
    )

    private fun completedOutcome() = RemoteHabitOutcome(
        revision = 1,
        status = RemoteHabitOutcomeStatus.COMPLETED,
        progressBasisPoints = 10_000,
        quantity = 8,
        unit = "pages",
        actualSeconds = 600,
        note = "Finished",
        occurredAt = OCCURRED_AT,
        updatedAt = UPDATED_AT,
    )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-01T07:29:00Z")
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "configuration-a"
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val OCCURRENCE_ID = "22222222-2222-4222-8222-222222222222"
        const val SECOND_OCCURRENCE_ID = "88888888-8888-4888-8888-888888888888"
        const val PLANNER_OCCURRENCE_ID = "33333333-3333-5333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PAUSE_ID = "55555555-5555-4555-8555-555555555555"
        const val OPERATION_ID = "66666666-6666-4666-8666-666666666666"
        const val SECOND_OPERATION_ID = "77777777-7777-4777-8777-777777777777"
        const val OCCURRED_AT = "2026-09-01T07:30:00Z"
        const val UPDATED_AT = "2026-09-01T07:31:00Z"
        const val PAUSED_AT = "2026-09-02T08:00:00Z"
        const val RESUMED_AT = "2026-09-03T08:00:00Z"
    }
}

private class FakeHabitTransport : HabitTransport {
    val occurrencePages = ArrayDeque<RemoteHabitOccurrencePage>()
    val occurrenceCursors = mutableListOf<String?>()
    val outcomeKeys = mutableListOf<String>()
    val outcomeBodies = mutableListOf<String>()
    val startPauseBodies = mutableListOf<String>()
    val resumePauseBodies = mutableListOf<String>()
    val deltaCursors = mutableListOf<String?>()

    var outcomeHandler: suspend (String, String, String, String) ->
        RemoteHabitMutation<RemoteHabitOccurrence> = { _, _, _, _ -> error("not configured") }
    var startPauseHandler: suspend (String, String, String) ->
        RemoteHabitMutation<RemoteHabitPause> = { _, _, _ -> error("not configured") }
    var resumePauseHandler: suspend (String, String, String, String) ->
        RemoteHabitMutation<RemoteHabitPause> = { _, _, _, _ -> error("not configured") }
    var deltaHandler: suspend (String?) -> RemoteHabitDeltaPage = { cursor ->
        RemoteHabitDeltaPage(emptyList(), cursor ?: "cursor-0", hasMore = false)
    }
    var analyticsHandler: suspend (
        String,
        LocalDate,
        LocalDate,
        RemoteHabitAnalyticsBucket,
    ) -> RemoteHabitAnalytics = { _, _, _, _ -> error("not configured") }

    override suspend fun listOccurrences(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        cursor: String?,
        limit: Int,
    ): RemoteHabitOccurrencePage {
        occurrenceCursors += cursor
        return occurrencePages.removeFirst()
    }

    override suspend fun putOutcome(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        occurrenceId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitOccurrence> {
        outcomeKeys += idempotencyKey
        outcomeBodies += requestJson
        return outcomeHandler(habitId, occurrenceId, idempotencyKey, requestJson)
    }

    override suspend fun delta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
        limit: Int,
    ): RemoteHabitDeltaPage {
        deltaCursors += cursor
        return deltaHandler(cursor)
    }

    override suspend fun startPause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        startPauseBodies += requestJson
        return startPauseHandler(habitId, idempotencyKey, requestJson)
    }

    override suspend fun resumePause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        pauseId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        resumePauseBodies += requestJson
        return resumePauseHandler(habitId, pauseId, idempotencyKey, requestJson)
    }

    override suspend fun analytics(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        bucket: RemoteHabitAnalyticsBucket,
    ): RemoteHabitAnalytics = analyticsHandler(habitId, startDate, endDate, bucket)
}

private fun remoteAnalytics(
    habitId: String,
    startDate: LocalDate,
    endDate: LocalDate,
    bucket: RemoteHabitAnalyticsBucket,
) = RemoteHabitAnalytics(
    habitId = habitId,
    startDate = startDate.toString(),
    endDate = endDate.toString(),
    bucket = bucket,
    expected = 0,
    eligible = 0,
    completed = 0,
    partial = 0,
    skipped = 0,
    missed = 0,
    excused = 0,
    unresolved = 0,
    adherenceBasisPoints = 0,
    actualSecondsTotal = 0,
    quantityTotals = emptyList(),
    currentStreak = 0,
    longestStreak = 0,
    trends = emptyList(),
    supportiveFactCodes = listOf(RemoteHabitSupportiveFactCode.NO_DATA),
)

private fun remoteOccurrence(
    id: String = "22222222-2222-4222-8222-222222222222",
    outcome: RemoteHabitOutcome? = null,
) = RemoteHabitOccurrence(
    evidence = RemoteHabitOccurrenceEvidence(
        id = id,
        habitId = "11111111-1111-4111-8111-111111111111",
        plannerOccurrenceId = if (id == "22222222-2222-4222-8222-222222222222") {
            "33333333-3333-5333-8333-333333333333"
        } else {
            "99999999-9999-5999-8999-999999999999"
        },
        sourceScheduleRevisionId = "44444444-4444-4444-8444-444444444444",
        sourceItemRevision = 7,
        policyFingerprint = "sha256:${"a".repeat(64)}",
        identity = JsonObject(
            mapOf(
                "type" to JsonPrimitive("calendar_day"),
                "date" to JsonPrimitive("2026-09-01"),
                "bucket_ordinal" to JsonPrimitive(0),
            ),
        ),
        nominalStart = "2026-09-01T07:00:00Z",
        nominalEnd = "2026-09-01T07:30:00Z",
        windowStart = "2026-09-01T06:00:00Z",
        windowEnd = "2026-09-01T09:00:00Z",
        localDate = "2026-09-01",
        timezoneName = "Europe/Paris",
        expectedDurationSeconds = 1_800,
        expectedQuantity = 20,
        expectedUnit = "pages",
    ),
    outcome = outcome,
)

private fun remotePause(
    revision: Long,
    endedAt: String? = null,
) = RemoteHabitPause(
    id = "55555555-5555-4555-8555-555555555555",
    habitId = "11111111-1111-4111-8111-111111111111",
    revision = revision,
    startedAt = "2026-09-02T08:00:00Z",
    endedAt = endedAt,
    preservesStreak = true,
    createdAt = "2026-09-02T08:00:00Z",
    updatedAt = endedAt ?: "2026-09-02T08:00:00Z",
)
