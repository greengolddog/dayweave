package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class RustScheduleComposerTest {
    @Test
    fun `strict response maps one exact composition`() {
        val composition = RustScheduleComposer(bridge = { error("not called") })
            .decodeResponse(validResponse())

        assertEquals(LOCAL_FINGERPRINT, composition.localInputFingerprint)
        assertEquals(0, composition.sourceItemCount)
        assertTrue(composition.sourceItemRevisions.isEmpty())
        assertTrue(composition.plan.blocks.isEmpty())
    }

    @Test
    fun `request redacts notes and retains the complete canonical shape`() {
        val encoded = RustScheduleComposer(bridge = { error("not called") })
            .encodeRequest(listOf(item(notes = PRIVATE_NOTE)), request())
            .toString(Charsets.UTF_8)

        assertFalse(encoded.contains(PRIVATE_NOTE))
        assertTrue(encoded.contains("\"notes\":null"))
        assertTrue(encoded.contains("\"operation\":\"compose\""))
        assertTrue(encoded.contains("\"split_policy\":{\"type\":\"indivisible\"}"))
    }

    @Test
    fun `request preserves ranged duration provenance and general habit spacing`() {
        val richHabit = item().copy(
            kind = "habit",
            durationSeconds = 1_800,
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 1_200,
            durationMaxSeconds = 2_400,
            durationSource = CanonicalDurationSource.LEARNED,
            flexibleConstraintsJson = "{\"habit_minimum_spacing_minutes\":90}",
            hasExplicitStructuralMetadata = true,
        )

        val encoded = RustScheduleComposer(bridge = { error("not called") })
            .encodeRequest(listOf(richHabit), request())
            .toString(Charsets.UTF_8)

        assertTrue(encoded.contains("\"duration_kind\":\"range\""))
        assertTrue(encoded.contains("\"duration_min_seconds\":1200"))
        assertTrue(encoded.contains("\"duration_seconds\":1800"))
        assertTrue(encoded.contains("\"duration_max_seconds\":2400"))
        assertTrue(encoded.contains("\"duration_source\":\"learned\""))
        assertTrue(encoded.contains("\"habit_minimum_spacing_minutes\":90"))
    }

    @Test
    fun `future duration provenance remains outside the local composer boundary`() {
        val future = item().copy(
            durationSource = CanonicalDurationSource("future_estimator_v2"),
            hasExplicitStructuralMetadata = true,
        )

        try {
            RustScheduleComposer(bridge = { error("not called") })
                .encodeRequest(listOf(future), request())
            fail("Future duration provenance was composed")
        } catch (error: LocalScheduleCompositionRequestException) {
            assertNull(error.cause)
        }
    }

    @Test
    fun `incremental request encoding accepts the exact bound and rejects one byte over`() {
        val composer = RustScheduleComposer(bridge = { error("not called") })
        val emptyTitleSize = composer.encodeRequest(listOf(item(title = "")), request()).size
        val byteLimit = emptyTitleSize + 1_024

        val exact = composer.encodeRequest(
            listOf(item(title = "x".repeat(1_024))),
            request(),
            byteLimit = byteLimit,
        )
        assertEquals(byteLimit, exact.size)

        val thrown = try {
            composer.encodeRequest(
                listOf(item(title = "private-${"x".repeat(1_017)}")),
                request(),
                byteLimit = byteLimit,
            )
            fail("An over-limit native request was accepted")
            null
        } catch (error: LocalScheduleCompositionRequestTooLargeException) {
            error
        }
        assertEquals(
            "Bundled scheduler request exceeds the fixed local limit",
            requireNotNull(thrown).message,
        )
        assertNull(thrown.cause)
        assertFalse(thrown.toString().contains("private-"))
    }

    @Test
    fun `malformed native response never survives in exception chain`() {
        val malformed =
            "{\"protocol\":\"$PRIVATE_NOTE\",\"protocol\":\"x\"}\n".toByteArray()

        val thrown = try {
            RustScheduleComposer(bridge = { error("not called") }).decodeResponse(malformed)
            fail("Malformed response was accepted")
            null
        } catch (error: LocalScheduleCompositionProtocolException) {
            error
        }

        assertFalse(requireNotNull(thrown).toString().contains(PRIVATE_NOTE))
        assertFalse(thrown.message.orEmpty().contains(PRIVATE_NOTE))
        assertNull(thrown.cause)
    }

    @Test
    fun `unknown response field fails closed`() {
        val malformed = validResponse().toString(Charsets.UTF_8)
            .replace("\"version\":1", "\"version\":1,\"extra\":true")
            .toByteArray()

        expectProtocolFailure {
            RustScheduleComposer(bridge = { error("not called") }).decodeResponse(malformed)
        }
    }

    @Test
    fun `null JNI result is a fixed sanitized failure`() = runBlocking {
        val composer = RustScheduleComposer(bridge = { null })

        val thrown = try {
            composer.compose(emptyList(), request())
            fail("Null JNI response was accepted")
            null
        } catch (error: LocalScheduleCompositionProtocolException) {
            error
        }

        var current: Throwable? = requireNotNull(thrown)
        while (current != null) {
            assertTrue(current is LocalScheduleCompositionProtocolException)
            assertEquals("Bundled scheduler returned an invalid response", current.message)
            current = current.cause
        }
    }

    @Test
    fun `cancellation during native call discards the returned bytes`() = runBlocking {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val composer = RustScheduleComposer(bridge = {
            entered.countDown()
            check(release.await(5, TimeUnit.SECONDS))
            validResponse()
        })
        val operation = async(Dispatchers.Default) { composer.compose(emptyList(), request()) }
        assertTrue(entered.await(5, TimeUnit.SECONDS))

        operation.cancel()
        release.countDown()

        try {
            operation.await()
            fail("Cancelled composition produced a result")
        } catch (_: CancellationException) {
            Unit
        }
    }

    @Test
    fun `cancellation after encoding never enters native bridge`() = runBlocking {
        var bridgeCalls = 0
        val composer = RustScheduleComposer(
            bridge = {
                bridgeCalls += 1
                validResponse()
            },
            beforeBridge = {
                currentCoroutineContext().cancel()
            },
        )
        val operation = async(Dispatchers.Default) { composer.compose(emptyList(), request()) }

        try {
            operation.await()
            fail("Cancelled pre-bridge composition produced a result")
        } catch (_: CancellationException) {
            Unit
        }
        assertEquals(0, bridgeCalls)
    }

    private fun expectProtocolFailure(action: () -> Unit) {
        try {
            action()
            fail("Invalid response was accepted")
        } catch (error: LocalScheduleCompositionProtocolException) {
            assertNull(error.cause)
        }
    }

    private fun request() = SchedulePreviewRequest(
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

    private fun item(
        notes: String? = null,
        title: String = "Private task",
    ) = CanonicalItemSnapshot(
        id = "11111111-1111-4111-8111-111111111111",
        kind = "task",
        status = "scheduled",
        title = title,
        notes = notes,
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-31T08:00:00Z",
        updatedAt = "2026-08-31T08:00:00Z",
    )

    private fun validResponse(): ByteArray = (
        """
        {"protocol":"dayweave.scheduler.helper","version":1,"result":{"type":"composition","composition":{"local_input_fingerprint":"$LOCAL_FINGERPRINT","source_item_count":0,"source_item_revisions":{},"accepted_item_count":0,"rejected_items":[],"ignored_previous_assignments":[],"plan":{"as_of":"2026-08-31T08:00:00Z","horizon_start":"2026-08-31T00:00:00Z","horizon_end":"2026-09-01T00:00:00Z","blocks":[],"unscheduled":[],"decisions":[],"violations":[],"score":{"scheduled_minutes":0,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}}}
        """.trimIndent() + "\n"
        ).toByteArray()

    private companion object {
        const val PRIVATE_NOTE = "secret-native-response-title"
        const val LOCAL_FINGERPRINT =
            "local-sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
}
