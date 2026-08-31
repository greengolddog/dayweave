package com.greengolddog.dayweave.scheduler

import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RustScheduleComposerInstrumentedTest {
    @Test
    fun packagedNativeSchedulerIsLoadableStrictAndDeterministic() = runBlocking {
        val request = SchedulePreviewRequest(
            asOf = "2026-08-31T08:00:00Z",
            horizonStart = "2026-08-31T00:00:00Z",
            horizonEnd = "2026-09-01T00:00:00Z",
            timezoneName = "UTC",
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = "2026-08-31T07:00:00Z",
                    end = "2026-08-31T22:00:00Z",
                ),
            ),
        )
        val composer = RustScheduleComposer()
        val recurringSplitHabit = CanonicalItemSnapshot(
            id = "11111111-1111-4111-8111-111111111111",
            kind = "habit",
            status = "planned",
            title = "Practice deliberately",
            timezoneName = "UTC",
            durationSeconds = 3_600,
            recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
            flexibleConstraintsJson =
                "{\"maximum_sessions\":4,\"minimum_gap_minutes\":5}",
            splitPolicyJson =
                "{\"type\":\"splittable\",\"minimum_chunk_seconds\":900," +
                    "\"maximum_chunk_seconds\":1800}",
            importance = 70,
            urgency = 60,
            siblingOrder = 0,
            isExecutable = true,
            revision = 3,
            createdAt = "2026-08-30T08:00:00Z",
            updatedAt = "2026-08-31T07:00:00Z",
        )

        val first = composer.compose(listOf(recurringSplitHabit), request)
        val second = composer.compose(listOf(recurringSplitHabit), request)

        assertEquals(first, second)
        assertTrue(first.localInputFingerprint.startsWith("local-sha256:"))
        assertTrue(first.scheduleRequestFingerprint.startsWith("sha256:"))
        assertEquals(request.asOf, first.plan.asOf)
        assertEquals(1, first.sourceItemCount)
        assertEquals(mapOf(recurringSplitHabit.id to 3L), first.sourceItemRevisions)
        assertEquals(1, first.acceptedItemCount)
        assertTrue(first.plan.occurrences.isNotEmpty())
    }
}
