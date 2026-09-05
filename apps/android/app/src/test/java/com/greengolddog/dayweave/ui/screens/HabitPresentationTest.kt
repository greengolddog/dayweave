package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsBucketSnapshot
import com.greengolddog.dayweave.model.HabitAnalyticsSnapshot
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitMissedPolicySnapshot
import com.greengolddog.dayweave.model.HabitMissedResolutionActionSnapshot
import com.greengolddog.dayweave.model.HabitMissedResolutionSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceEvidenceSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.HabitPauseSnapshot
import com.greengolddog.dayweave.model.HabitQuantityTotalSnapshot
import com.greengolddog.dayweave.model.HabitSupportiveFactCodeSnapshot
import com.greengolddog.dayweave.model.HabitTrendBucketSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import com.greengolddog.dayweave.model.PublishedOccurrenceMembershipProofSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceMembershipSnapshot
import com.greengolddog.dayweave.model.PublishedOccurrenceStateSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionHintSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.ScheduleItemPresentationSlice
import com.greengolddog.dayweave.model.habitPolicyFingerprintOrNull
import java.time.Instant
import java.time.LocalDate
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HabitPresentationTest {
    @Test
    fun analyticsFreshnessUsesBoundedFiveMinuteWindows() {
        val start = Instant.parse("2026-09-03T09:00:00Z")

        assertEquals(
            habitAnalyticsFreshnessWindow(start),
            habitAnalyticsFreshnessWindow(start.plusSeconds(299)),
        )
        assertEquals(
            habitAnalyticsFreshnessWindow(start) + 1,
            habitAnalyticsFreshnessWindow(start.plusSeconds(300)),
        )
    }

    @Test
    fun analyticsRefreshRetriesBusyAndGateRejectionOnlyUntilAdmission() = runBlocking {
        var busy = true
        var launches = 0
        var waits = 0

        val admitted = awaitHabitAnalyticsRefreshAdmission(
            shouldContinue = { true },
            actionBusy = { busy },
            launch = {
                launches += 1
                launches == 2
            },
            waitForRetry = {
                waits += 1
                busy = false
            },
        )

        assertTrue(admitted)
        assertEquals(2, launches)
        assertEquals(2, waits)
    }

    @Test
    fun analyticsRefreshStopsWithLifecycleBeforeAdmission() = runBlocking {
        var active = true
        var launches = 0

        val admitted = awaitHabitAnalyticsRefreshAdmission(
            shouldContinue = { active },
            actionBusy = { false },
            launch = {
                launches += 1
                false
            },
            waitForRetry = { active = false },
        )

        assertFalse(admitted)
        assertEquals(1, launches)
        assertNull(habitActionAdmissionMessage(admitted = true))
        assertTrue(habitActionAdmissionMessage(admitted = false).orEmpty().contains("not submitted"))
    }

    @Test
    fun analyticsRefreshAdmissionRetryIsBounded() = runBlocking {
        var launches = 0
        var waits = 0

        val admitted = awaitHabitAnalyticsRefreshAdmission(
            shouldContinue = { true },
            actionBusy = { false },
            launch = {
                launches += 1
                false
            },
            maxAttempts = 3,
            waitForRetry = { waits += 1 },
        )

        assertFalse(admitted)
        assertEquals(3, launches)
        assertEquals(2, waits)
    }

    @Test
    fun splitScheduleBlocksCollapseOntoExactLedgerOccurrenceIdentity() {
        val occurrence = occurrence(
            localDate = TODAY.toString(),
            expectedDurationSeconds = 2_700,
        )
        val schedule = listOf(
            slice("block-a", "08:00", 20),
            slice("block-b", "17:00", 25, sessionIndex = 1),
        )

        val rows = projectTodayHabits(
            schedule = schedule,
            canonicalItems = emptyList(),
            ledger = ledger(occurrences = listOf(occurrence)),
            date = TODAY,
        )

        assertEquals(1, rows.size)
        assertTrue(rows.single().hasCanonicalEvidence)
        assertEquals(LEDGER_OCCURRENCE_ID, rows.single().ledgerOccurrenceId)
        assertEquals("08:00 · 17:00", rows.single().timeLabel)
        assertEquals(45, rows.single().plannedMinutes)
    }

    @Test
    fun plannerOccurrenceIdIsNeverUsedAsTheWriteTarget() {
        val row = projectTodayHabits(
            schedule = listOf(slice("block-a", "08:00", 20)),
            canonicalItems = emptyList(),
            ledger = ledger(occurrences = listOf(occurrence(localDate = TODAY.toString()))),
            date = TODAY,
        ).single()

        assertEquals(PLANNER_OCCURRENCE_ID, row.occurrence?.evidence?.plannerOccurrenceId)
        assertEquals(LEDGER_OCCURRENCE_ID, row.ledgerOccurrenceId)
        assertFalse(row.ledgerOccurrenceId == PLANNER_OCCURRENCE_ID)
    }

    @Test
    fun harmlessRevisionBumpKeepsStableOccurrenceEvidenceAttached() {
        val stale = occurrence(
            localDate = TODAY.toString(),
            sourceItemRevision = 6,
        )

        val row = projectTodayHabits(
            schedule = listOf(slice("block-a", "08:00", 20, sourceRevision = 7)),
            canonicalItems = emptyList(),
            ledger = ledger(occurrences = listOf(stale)),
            date = TODAY,
        ).single()

        assertTrue(row.hasCanonicalEvidence)
        assertEquals(LEDGER_OCCURRENCE_ID, row.ledgerOccurrenceId)
        assertNull(row.fallback)
    }

    @Test
    fun evidenceFromANewerItemRevisionStillFailsClosed() {
        val future = occurrence(
            localDate = TODAY.toString(),
            sourceItemRevision = 8,
        )

        val row = projectTodayHabits(
            schedule = listOf(slice("block-a", "08:00", 20, sourceRevision = 7)),
            canonicalItems = emptyList(),
            ledger = ledger(occurrences = listOf(future)),
            date = TODAY,
        ).single()

        assertFalse(row.hasCanonicalEvidence)
        assertNull(row.ledgerOccurrenceId)
        assertEquals(HabitEvidenceFallback.AWAITING_CANONICAL_EVIDENCE, row.fallback)
    }

    @Test
    fun ambiguousEvidenceFailsClosedWithoutChoosingEitherLedgerId() {
        val first = occurrence(
            ledgerOccurrenceId = LEDGER_OCCURRENCE_ID,
            localDate = TODAY.plusDays(1).toString(),
        )
        val second = occurrence(
            ledgerOccurrenceId = SECOND_LEDGER_OCCURRENCE_ID,
            localDate = TODAY.plusDays(1).toString(),
        )

        val row = projectTodayHabits(
            schedule = listOf(slice("block-a", "08:00", 20)),
            canonicalItems = emptyList(),
            ledger = ledger(occurrences = listOf(first, second)),
            date = TODAY,
        ).single()

        assertEquals(HabitEvidenceFallback.AMBIGUOUS_CANONICAL_EVIDENCE, row.fallback)
        assertNull(row.ledgerOccurrenceId)
    }

    @Test
    fun splitBlocksWithMixedSourceRevisionsFailClosed() {
        val row = projectTodayHabits(
            schedule = listOf(
                slice("block-a", "08:00", 20, sourceRevision = 7),
                slice("block-b", "17:00", 20, sessionIndex = 1, sourceRevision = 8),
            ),
            canonicalItems = emptyList(),
            ledger = ledger(
                occurrences = listOf(
                    occurrence(localDate = TODAY.toString()),
                ),
            ),
            date = TODAY,
        ).single()

        assertEquals(HabitEvidenceFallback.AMBIGUOUS_CANONICAL_EVIDENCE, row.fallback)
        assertNull(row.ledgerOccurrenceId)
    }

    @Test
    fun unboundLedgerLeavesScheduleHabitReadOnly() {
        val row = projectTodayHabits(
            schedule = listOf(slice("block-a", "08:00", 20)),
            canonicalItems = emptyList(),
            ledger = HabitLedgerSnapshot(),
            date = TODAY,
        ).single()

        assertEquals(HabitEvidenceFallback.LEDGER_NOT_READY, row.fallback)
        assertFalse(row.hasCanonicalEvidence)
    }

    @Test
    fun ledgerOnlyOccurrenceUsesPrivateFallbackWhenCanonicalTitleIsUnavailable() {
        val rows = projectTodayHabits(
            schedule = emptyList(),
            canonicalItems = emptyList(),
            ledger = ledger(occurrences = listOf(occurrence(localDate = TODAY.toString()))),
            date = TODAY,
        )

        assertEquals("Private habit", rows.single().title)
        assertTrue(rows.single().isSensitive)
        assertTrue(rows.single().hasCanonicalEvidence)
    }

    @Test
    fun missedReviewOnlyOffersCurrentEffectiveActiveDecision() {
        val decision = occurrence(
            localDate = TODAY.toString(),
            outcome = outcome(),
            missedResolution = missedResolution(),
        )
        val active = canonicalHabit()

        assertEquals(
            listOf(LEDGER_OCCURRENCE_ID),
            missedHabitDecisions(listOf(active), ledger(occurrences = listOf(decision)))
                .map { it.occurrence.evidence.id },
        )
        assertTrue(
            missedHabitDecisions(
                listOf(active),
                ledger(
                    occurrences = listOf(decision),
                    pauses = listOf(pause(PAUSE_ID, revision = 1, endedAt = null)),
                ),
            ).isEmpty(),
        )
        assertTrue(
            missedHabitDecisions(
                listOf(active),
                ledger(
                    occurrences = listOf(
                        decision.copy(
                            outcome = outcome().copy(
                                status = HabitOutcomeStatusSnapshot.COMPLETED,
                                progressBasisPoints = 10_000,
                            ),
                        ),
                    ),
                ),
            ).isEmpty(),
        )
        assertEquals(
            listOf(LEDGER_OCCURRENCE_ID),
            missedHabitDecisions(
                listOf(active.copy(revision = 8, title = "Renamed habit", importance = 99)),
                ledger(occurrences = listOf(decision)),
            ).map { it.occurrence.evidence.id },
        )
        listOf(
            emptyList(),
            listOf(active.copy(
                revision = 8,
                recurrenceJson = """{"type":"daily","times_per_day":2}""",
            )),
            listOf(active.copy(deletedAt = NOW)),
            listOf(active.copy(kind = "task")),
            listOf(active.copy(status = "cancelled")),
            listOf(active.copy(status = "blocked")),
            listOf(active.copy(status = "future_status")),
            listOf(active.copy(isExecutable = false)),
            listOf(active.copy(revision = decision.evidence.sourceItemRevision - 1)),
        ).forEach { canonical ->
            assertTrue(
                missedHabitDecisions(
                    canonical,
                    ledger(occurrences = listOf(decision)),
                ).isEmpty(),
            )
        }

        val targetEvidence = decision.evidence.copy(
            id = SECOND_LEDGER_OCCURRENCE_ID,
            plannerOccurrenceId = "aaaaaaaa-aaaa-5aaa-8aaa-aaaaaaaaaaaa",
            identity = buildJsonObject {
                put("type", JsonPrimitive("calendar_day"))
                put("date", JsonPrimitive("2026-09-04"))
                put("bucket_ordinal", JsonPrimitive(0))
            },
            nominalStart = "2026-09-04T06:00:00Z",
            nominalEnd = "2026-09-04T06:20:00Z",
            windowStart = "2026-09-04T05:00:00Z",
            windowEnd = "2026-09-04T20:00:00Z",
            localDate = "2026-09-04",
        )
        val targetDecision = decision.copy(
            evidence = targetEvidence,
            outcome = null,
            missedResolution = requireNotNull(decision.missedResolution).copy(
                occurrenceEvidenceId = targetEvidence.id,
                sourcePlannerOccurrenceId = targetEvidence.plannerOccurrenceId,
            ),
        )
        val sourceReduction = decision.copy(
            missedResolution = requireNotNull(decision.missedResolution).copy(
                revision = 2,
                action = HabitMissedResolutionActionSnapshot.ReduceFrequency(
                    listOf(targetEvidence.plannerOccurrenceId),
                ),
            ),
        )
        assertEquals(
            listOf(targetEvidence.id),
            missedHabitDecisions(
                listOf(active),
                ledger(occurrences = listOf(targetDecision, sourceReduction)),
            ).map { it.occurrence.evidence.id },
        )
        val revision = PublishedScheduleRevisionSnapshot(
            id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            revisionNumber = 11uL,
            revision = "11:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = "2026-09-03T00:00:00Z",
            horizonEnd = "2026-09-05T00:00:00Z",
            timezoneName = "Europe/Paris",
            publishedAt = "2026-09-03T00:00:00Z",
        )
        val matchingHint = PublishedScheduleRevisionHintSnapshot(
            syncOrigin = SYNC_ORIGIN,
            configurationId = CONFIGURATION_ID,
            revisionNumber = revision.revisionNumber,
        )
        fun decisionsWithMembership(
            state: PublishedOccurrenceStateSnapshot?,
            hint: PublishedScheduleRevisionHintSnapshot = matchingHint,
            configurationId: String = CONFIGURATION_ID,
        ): List<String> {
            val proof = PublishedOccurrenceMembershipProofSnapshot(
                schemaVersion = PublishedOccurrenceMembershipProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = SYNC_ORIGIN,
                configurationId = CONFIGURATION_ID,
                revision = revision,
                occurrences = state?.let {
                    listOf(
                        PublishedOccurrenceMembershipSnapshot(
                            plannerOccurrenceId = targetEvidence.plannerOccurrenceId,
                            seriesItemId = HABIT_ID,
                            state = it,
                        ),
                    )
                }.orEmpty(),
            )
            return missedHabitDecisions(
                canonicalItems = listOf(active),
                ledger = ledger(occurrences = listOf(targetDecision, sourceReduction)),
                publishedOccurrenceMembershipProof = proof,
                publishedScheduleRevisionHint = hint,
                syncOrigin = SYNC_ORIGIN,
                configurationId = configurationId,
            ).map { it.occurrence.evidence.id }
        }
        listOf(
            PublishedOccurrenceStateSnapshot.GENERATED,
            PublishedOccurrenceStateSnapshot.SKIPPED,
        ).forEach { state ->
            assertTrue(decisionsWithMembership(state).isEmpty())
        }
        listOf(
            PublishedOccurrenceStateSnapshot.COMPLETED,
            PublishedOccurrenceStateSnapshot.PAUSED,
        ).forEach { state ->
            assertEquals(listOf(targetEvidence.id), decisionsWithMembership(state))
        }
        assertEquals(listOf(targetEvidence.id), decisionsWithMembership(null))
        assertEquals(
            listOf(targetEvidence.id),
            decisionsWithMembership(
                PublishedOccurrenceStateSnapshot.GENERATED,
                hint = matchingHint.copy(revisionNumber = 12uL),
            ),
        )
        assertEquals(
            listOf(targetEvidence.id),
            decisionsWithMembership(
                PublishedOccurrenceStateSnapshot.GENERATED,
                configurationId = "replacement-configuration",
            ),
        )
        assertEquals(
            listOf(targetEvidence.id),
            missedHabitDecisions(
                listOf(active),
                ledger(occurrences = listOf(
                    targetDecision,
                    sourceReduction.copy(
                        outcome = outcome().copy(
                            status = HabitOutcomeStatusSnapshot.COMPLETED,
                            progressBasisPoints = 10_000,
                        ),
                    ),
                )),
            ).map { it.occurrence.evidence.id },
        )
    }

    @Test
    fun partialDraftAcceptsExactProgressSignedQuantityDurationAndPrivateNote() {
        val validation = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressPercent = "55.55",
            quantity = "-12",
            unit = "pages",
            actualMinutes = "1.5",
            note = "Adjusted after review",
            actualMinutesEdited = true,
        ).validate(NOW)

        val outcome = requireNotNull(validation.outcome)
        assertNull(validation.message)
        assertEquals(5_555, outcome.progressBasisPoints)
        assertEquals(-12L, outcome.quantity)
        assertEquals("pages", outcome.unit)
        assertEquals(90L, outcome.actualSeconds)
        assertEquals("Adjusted after review", outcome.note)
    }

    @Test
    fun draftAcceptsTheServersUnicodeScalarTextLimits() {
        val note = "😀".repeat(10_000)
        val unit = "💧".repeat(200)

        val validation = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressPercent = "100",
            quantity = "1",
            unit = unit,
            actualMinutes = "",
            note = note,
        ).validate(NOW)

        assertNull(validation.message)
        assertEquals(unit, validation.outcome?.unit)
        assertEquals(note, validation.outcome?.note)
    }

    @Test
    fun skippedDraftRetainsPartialEvidence() {
        val validation = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.SKIPPED,
            progressPercent = "22.25",
            quantity = "3",
            unit = "sets",
            actualMinutes = "4",
            note = "Stopped early",
            actualMinutesEdited = true,
        ).validate(NOW)

        val outcome = requireNotNull(validation.outcome)
        assertEquals(HabitOutcomeStatusSnapshot.SKIPPED, outcome.status)
        assertEquals(2_225, outcome.progressBasisPoints)
        assertEquals(3L, outcome.quantity)
        assertEquals(240L, outcome.actualSeconds)
    }

    @Test
    fun clearingAnOutcomeDropsEveryEvidenceField() {
        val validation = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.UNRESOLVED,
            progressPercent = "invalid hidden value",
            quantity = "9",
            unit = "sets",
            actualMinutes = "invalid hidden value",
            note = "hidden draft",
        ).validate(NOW)

        val outcome = requireNotNull(validation.outcome)
        assertEquals(0, outcome.progressBasisPoints)
        assertNull(outcome.quantity)
        assertNull(outcome.unit)
        assertNull(outcome.actualSeconds)
        assertNull(outcome.note)
    }

    @Test
    fun completedOutcomeAlwaysUsesFullProgress() {
        val outcome = requireNotNull(
            HabitOutcomeDraft(
                status = HabitOutcomeStatusSnapshot.COMPLETED,
                progressPercent = "3",
                quantity = "",
                unit = "",
                actualMinutes = "",
                note = "",
            ).validate(NOW).outcome,
        )

        assertEquals(10_000, outcome.progressBasisPoints)
    }

    @Test
    fun switchingCorrectionStatusChoosesAValidProgressDefault() {
        val completed = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressPercent = "100",
            quantity = "",
            unit = "",
            actualMinutes = "",
            note = "",
        )

        assertEquals("50", completed.selectStatus(HabitOutcomeStatusSnapshot.PARTIAL).progressPercent)
        assertEquals("0", completed.selectStatus(HabitOutcomeStatusSnapshot.SKIPPED).progressPercent)
        assertEquals(
            "25.25",
            completed.copy(progressPercent = "25.25")
                .selectStatus(HabitOutcomeStatusSnapshot.PARTIAL)
                .progressPercent,
        )
    }

    @Test
    fun quantityAndUnitMustBeEnteredTogether() {
        val validation = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressPercent = "50",
            quantity = "2",
            unit = "",
            actualMinutes = "",
            note = "",
        ).validate(NOW)

        assertNull(validation.outcome)
        assertTrue(validation.message.orEmpty().contains("both quantity and unit"))
    }

    @Test
    fun progressPrecisionBeyondBasisPointsIsRejected() {
        val validation = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressPercent = "12.345",
            quantity = "",
            unit = "",
            actualMinutes = "",
            note = "",
        ).validate(NOW)

        assertNull(validation.outcome)
        assertTrue(validation.message.orEmpty().contains("two decimals"))
    }

    @Test
    fun correctionPreservesExactActualSecondsUntilDurationIsEdited() {
        val existing = outcome(actualSeconds = 61)
        val untouched = HabitOutcomeDraft.correcting(existing).validate(existing.occurredAt)

        assertEquals(61L, untouched.outcome?.actualSeconds)

        val edited = HabitOutcomeDraft.correcting(existing).copy(
            actualMinutes = "1.5",
            actualMinutesEdited = true,
        ).validate(existing.occurredAt)
        assertEquals(90L, edited.outcome?.actualSeconds)
    }

    @Test
    fun durationMustResolveToWholeSecondsAndStayBounded() {
        val base = HabitOutcomeDraft(
            status = HabitOutcomeStatusSnapshot.PARTIAL,
            progressPercent = "50",
            quantity = "",
            unit = "",
            actualMinutes = "1.001",
            note = "",
            actualMinutesEdited = true,
        )

        assertNull(base.validate(NOW).outcome)
        assertNull(
            base.copy(actualMinutes = "527041")
                .validate(NOW)
                .outcome,
        )
    }

    @Test
    fun pendingLookupUsesLedgerOccurrenceIdAndReviewedQueueExcludesPending() {
        val pending = pendingMutation(
            targetId = LEDGER_OCCURRENCE_ID,
            idempotencyKey = OPERATION_ID,
            disposition = PendingHabitMutationDisposition.PENDING,
        )
        val reviewed = pendingMutation(
            targetId = SECOND_LEDGER_OCCURRENCE_ID,
            idempotencyKey = SECOND_OPERATION_ID,
            disposition = PendingHabitMutationDisposition.CONFLICT,
        )
        val ledger = ledger(pendingMutations = listOf(pending, reviewed))

        assertEquals(pending, pendingMutationForOccurrence(ledger, LEDGER_OCCURRENCE_ID))
        assertNull(pendingMutationForOccurrence(ledger, PLANNER_OCCURRENCE_ID))
        assertEquals(listOf(reviewed), reviewedHabitMutations(ledger))
    }

    @Test
    fun newestOpenPauseIsSelectedAndClosedPausesAreIgnored() {
        val older = pause(PAUSE_ID, revision = 1, endedAt = null)
        val newer = pause(SECOND_PAUSE_ID, revision = 2, endedAt = null)
        val closed = pause(THIRD_PAUSE_ID, revision = 3, endedAt = "2026-09-03T10:00:00Z")
        val ledger = ledger(pauses = listOf(older, newer, closed))

        assertEquals(newer, activePauseForHabit(ledger, HABIT_ID))
    }

    @Test
    fun statisticsRangesAreInclusiveAndNeverExceedApiLimit() {
        assertEquals(
            LocalDate.parse("2025-09-04"),
            HabitStatisticsRange.ONE_YEAR.bounds(TODAY).start,
        )
        assertEquals(TODAY, HabitStatisticsRange.ONE_YEAR.bounds(TODAY).endInclusive)
        assertEquals(
            364,
            HabitStatisticsRange.ONE_YEAR.bounds(TODAY).endInclusive.toEpochDay() -
                HabitStatisticsRange.ONE_YEAR.bounds(TODAY).start.toEpochDay(),
        )
    }

    @Test
    fun statisticsRangeClampsToServerSupportedYears() {
        assertEquals(
            LocalDate.parse("1900-01-01"),
            HabitStatisticsRange.NINETY_DAYS.bounds(LocalDate.parse("1800-01-01")).start,
        )
        assertEquals(
            LocalDate.parse("2200-12-31"),
            HabitStatisticsRange.NINETY_DAYS.bounds(LocalDate.parse("2300-01-01")).endInclusive,
        )
    }

    @Test
    fun analyticsSelectionRequiresExactHabitRangeAndBucket() {
        val analytics = analytics()
        val ledger = ledger(analytics = listOf(analytics))
        val bounds = LocalDate.parse(analytics.startDate)..LocalDate.parse(analytics.endDate)

        assertEquals(
            analytics,
            analyticsFor(ledger, HABIT_ID, bounds, HabitAnalyticsBucketSnapshot.WEEK),
        )
        assertNull(
            analyticsFor(ledger, HABIT_ID, bounds, HabitAnalyticsBucketSnapshot.DAY),
        )
        assertNull(
            analyticsFor(
                ledger,
                HABIT_ID,
                bounds.start.plusDays(1)..bounds.endInclusive,
                HabitAnalyticsBucketSnapshot.WEEK,
            ),
        )
    }

    @Test
    fun supportiveFactsUseEncouragingNonJudgmentalCopy() {
        val messages = supportiveHabitMessages(analytics())

        assertEquals(4, messages.size)
        assertTrue(messages.any { it.contains("clean restart") })
        assertTrue(messages.any { it.contains("does not erase") })
        assertTrue(messages.none { it.contains("failed", ignoreCase = true) })
    }

    @Test
    fun formattingPreservesBasisPointPrecisionAndCompactDuration() {
        assertEquals("55.55", formatBasisPoints(5_555))
        assertEquals("100", formatBasisPoints(10_000))
        assertEquals("1d 2h 3m", formatHabitDuration(93_780))
        assertEquals("0m", formatHabitDuration(0))
    }

    private fun slice(
        id: String,
        time: String,
        durationMinutes: Int,
        sessionIndex: Int = 0,
        sourceRevision: Long = 7,
    ): ScheduleItemPresentationSlice = ScheduleItemPresentationSlice(
        item = ScheduleItem(
            id = id,
            title = "Morning practice",
            kind = ItemKind.HABIT,
            startMinute = 8 * 60,
            durationMinutes = durationMinutes,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = HABIT_ID,
            occurrenceId = PLANNER_OCCURRENCE_ID,
            canonicalRevision = sourceRevision,
            sessionIndex = sessionIndex,
        ),
        clippedStart = Instant.parse("2026-09-03T06:00:00Z").plusSeconds(
            sessionIndex * 32_400L,
        ),
        clippedEnd = Instant.parse("2026-09-03T06:00:00Z").plusSeconds(
            sessionIndex * 32_400L + durationMinutes * 60L,
        ),
        startTimeLabel = time,
        weekStartLabel = time,
        durationMinutes = durationMinutes,
        durationLabel = "${durationMinutes}m",
    )

    private fun occurrence(
        ledgerOccurrenceId: String = LEDGER_OCCURRENCE_ID,
        localDate: String,
        sourceItemRevision: Long = 7,
        expectedDurationSeconds: Long? = 1_200,
        outcome: HabitOutcomeSnapshot? = null,
        missedResolution: HabitMissedResolutionSnapshot? = null,
    ) = HabitOccurrenceSnapshot(
        evidence = HabitOccurrenceEvidenceSnapshot(
            id = ledgerOccurrenceId,
            habitId = HABIT_ID,
            plannerOccurrenceId = PLANNER_OCCURRENCE_ID,
            sourceScheduleRevisionId = SCHEDULE_REVISION_ID,
            sourceItemRevision = sourceItemRevision,
            policyFingerprint = requireNotNull(canonicalHabit().habitPolicyFingerprintOrNull()),
            identity = buildJsonObject {
                put("type", JsonPrimitive("calendar_day"))
                put("date", JsonPrimitive(localDate))
                put("bucket_ordinal", JsonPrimitive(0))
            },
            nominalStart = "2026-09-03T06:00:00Z",
            nominalEnd = "2026-09-03T06:20:00Z",
            windowStart = "2026-09-03T05:00:00Z",
            windowEnd = "2026-09-03T20:00:00Z",
            localDate = localDate,
            timezoneName = "Europe/Paris",
            expectedDurationSeconds = expectedDurationSeconds,
            expectedQuantity = null,
            expectedUnit = null,
        ),
        outcome = outcome,
        missedResolution = missedResolution,
    )

    private fun missedResolution() = HabitMissedResolutionSnapshot(
        occurrenceEvidenceId = LEDGER_OCCURRENCE_ID,
        habitId = HABIT_ID,
        sourcePlannerOccurrenceId = PLANNER_OCCURRENCE_ID,
        revision = 1,
        configuredPolicy = HabitMissedPolicySnapshot.ASK,
        action = HabitMissedResolutionActionSnapshot.DecisionRequired,
        createdAt = "2026-09-03T20:01:00Z",
        updatedAt = "2026-09-03T20:01:00Z",
    )

    private fun canonicalHabit() = CanonicalItemSnapshot(
        id = HABIT_ID,
        kind = "habit",
        status = "planned",
        title = "Morning practice",
        timezoneName = "Europe/Paris",
        durationSeconds = 1_200,
        recurrenceJson = """{"type":"daily","times_per_day":1}""",
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 5,
        urgency = 5,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = "2026-09-01T00:00:00Z",
        updatedAt = "2026-09-01T00:00:00Z",
    )

    private fun outcome(actualSeconds: Long? = 600) = HabitOutcomeSnapshot(
        revision = 2,
        status = HabitOutcomeStatusSnapshot.PARTIAL,
        progressBasisPoints = 5_555,
        quantity = -2,
        unit = "sets",
        actualSeconds = actualSeconds,
        note = "Private correction",
        occurredAt = "2026-09-03T07:00:00Z",
        updatedAt = "2026-09-03T07:05:00Z",
    )

    private fun pause(
        id: String,
        revision: Long,
        endedAt: String?,
    ) = HabitPauseSnapshot(
        id = id,
        habitId = HABIT_ID,
        revision = revision,
        startedAt = "2026-09-03T08:00:00Z",
        endedAt = endedAt,
        preservesStreak = true,
        createdAt = "2026-09-03T08:00:00Z",
        updatedAt = endedAt ?: "2026-09-03T08:00:00Z",
    )

    private fun pendingMutation(
        targetId: String,
        idempotencyKey: String,
        disposition: PendingHabitMutationDisposition,
    ) = PendingHabitMutation(
        schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
        kind = PendingHabitMutationKind.OUTCOME,
        habitId = HABIT_ID,
        targetId = targetId,
        expectedRevision = 0,
        idempotencyKey = idempotencyKey,
        requestJson = "{}",
        createdAt = "2026-09-03T08:00:00Z",
        syncOrigin = SYNC_ORIGIN,
        configurationId = CONFIGURATION_ID,
        disposition = disposition,
    )

    private fun analytics() = HabitAnalyticsSnapshot(
        habitId = HABIT_ID,
        startDate = "2026-06-06",
        endDate = TODAY.toString(),
        bucket = HabitAnalyticsBucketSnapshot.WEEK,
        expected = 4,
        eligible = 3,
        completed = 1,
        partial = 1,
        skipped = 0,
        missed = 1,
        excused = 1,
        unresolved = 0,
        adherenceBasisPoints = 5_555,
        actualSecondsTotal = 3_600,
        quantityTotals = listOf(HabitQuantityTotalSnapshot("sets", -2)),
        currentStreak = 2,
        longestStreak = 5,
        trends = listOf(
            HabitTrendBucketSnapshot(
                startDate = "2026-08-31",
                endDate = TODAY.toString(),
                expected = 4,
                eligible = 3,
                completed = 1,
                partial = 1,
                skipped = 0,
                missed = 1,
                excused = 1,
                unresolved = 0,
                adherenceBasisPoints = 5_555,
                actualSecondsTotal = 3_600,
                quantityTotals = listOf(HabitQuantityTotalSnapshot("sets", -2)),
            ),
        ),
        supportiveFactCodes = HabitSupportiveFactCodeSnapshot.entries,
    )

    private fun ledger(
        occurrences: List<HabitOccurrenceSnapshot> = emptyList(),
        pauses: List<HabitPauseSnapshot> = emptyList(),
        analytics: List<HabitAnalyticsSnapshot> = emptyList(),
        pendingMutations: List<PendingHabitMutation> = emptyList(),
    ) = HabitLedgerSnapshot(
        syncOrigin = SYNC_ORIGIN,
        configurationId = CONFIGURATION_ID,
        occurrences = occurrences.associateBy { it.evidence.id },
        pauses = pauses.associateBy { it.id },
        analytics = analytics.associateBy { it.cacheKey },
        pendingMutations = pendingMutations,
    )

    private companion object {
        val TODAY: LocalDate = LocalDate.parse("2026-09-03")
        const val NOW = "2026-09-03T09:00:00Z"
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val PLANNER_OCCURRENCE_ID = "22222222-2222-5222-8222-222222222222"
        const val LEDGER_OCCURRENCE_ID = "33333333-3333-4333-8333-333333333333"
        const val SECOND_LEDGER_OCCURRENCE_ID = "44444444-4444-4444-8444-444444444444"
        const val SCHEDULE_REVISION_ID = "55555555-5555-4555-8555-555555555555"
        const val OPERATION_ID = "66666666-6666-4666-8666-666666666666"
        const val SECOND_OPERATION_ID = "77777777-7777-4777-8777-777777777777"
        const val PAUSE_ID = "88888888-8888-4888-8888-888888888888"
        const val SECOND_PAUSE_ID = "99999999-9999-4999-8999-999999999999"
        const val THIRD_PAUSE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val SYNC_ORIGIN = "https://planner.example.test/"
        const val CONFIGURATION_ID = "test-configuration"
    }
}
