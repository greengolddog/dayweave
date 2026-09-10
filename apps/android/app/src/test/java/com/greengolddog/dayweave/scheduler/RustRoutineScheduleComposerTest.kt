package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.*
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RustRoutineScheduleComposerTest {
    @Test fun explicitV2PreservesTheHeadAndFullScheduleWhileV1StaysUnchanged(): Unit = runBlocking {
        val witness = planningTestWitness()
        val composer = RustScheduleComposer(bridge = { bytes ->
            val root = Json.parseToJsonElement(bytes.toString(Charsets.UTF_8)).jsonObject
            assertEquals(2, root.getValue("version").jsonPrimitive.int)
            val request = root.getValue("request").jsonObject
            assertEquals(ROUTINE_PLANNING_JSON.encodeToJsonElement(witness.schedule), request["schedule"])
            assertEquals(ROUTINE_PLANNING_JSON.encodeToJsonElement(witness.occurrenceLifecycle), request["occurrence_lifecycle"])
            assertFalse(bytes.toString(Charsets.UTF_8).contains("Synthetic private note"))
            planningTestHelperResponse()
        })
        val composed = composer.compose(planningTestItems(), witness)
        assertEquals(2L, composed.occurrenceSnapshotRevision)
        assertEquals(witness.localInputFingerprint, composed.composition.localInputFingerprint)
        val v1 = composer.encodeRequest(planningTestItems(), com.greengolddog.dayweave.network.SchedulePreviewRequest(
            ROUTINE_NOW, witness.schedule.horizonStart, witness.schedule.horizonEnd, "UTC", emptyList()))
        assertEquals(1, Json.parseToJsonElement(v1.toString(Charsets.UTF_8)).jsonObject.getValue("version").jsonPrimitive.int)
        assertFalse(v1.toString(Charsets.UTF_8).contains("occurrence_lifecycle"))
    }

    @Test fun helperV1FallbackMissingHeadWrongHeadAndMismatchedFingerprintAreRejected() {
        val witness = planningTestWitness(); val original = planningTestHelperResponse().toString(Charsets.UTF_8)
        val invalid = listOf(original.replace("\"version\":2", "\"version\":1"),
            original.replace("\"occurrence_snapshot_revision\":2,", ""),
            original.replace("\"occurrence_snapshot_revision\":2", "\"occurrence_snapshot_revision\":3"),
            original.replace("\"occurrence_snapshot_revision\":2", "\"occurrence_snapshot_revision\":\"2\""),
            original.replace("\"source_item_count\":3", "\"source_item_count\":\"3\""),
            original.replace("\"accepted_item_count\":3", "\"accepted_item_count\":2"),
            original.replace(witness.localInputFingerprint, "local-sha256:" + "e".repeat(64)),
            original.replace("\"bucket_ordinal\":0", "\"bucket_ordinal\":0.0"), original.trimEnd())
        invalid.forEach { body ->
            val error = assertThrows(LocalScheduleCompositionProtocolException::class.java) {
                RustScheduleComposer(bridge = { error("not called") }).decodeV2Response(body.toByteArray(), witness)
            }
            assertNull(error.cause)
        }
        assertThrows(LocalScheduleCompositionProtocolException::class.java) {
            RustScheduleComposer(bridge = { error("not called") }).decodeResponse(planningTestHelperResponse())
        }
    }

    @Test fun omittedInboxAndStaleCurrentRevisionsAreRejectedBeforeCrossingTheBridge() {
        val called = AtomicInteger()
        val composer = RustScheduleComposer(bridge = { called.incrementAndGet(); planningTestHelperResponse() })
        for (items in listOf(planningTestItems().dropLast(1), planningTestItems().map { it.copy(revision = 8) })) {
            assertThrows(LocalScheduleCompositionRequestException::class.java) { runBlocking { composer.compose(items, planningTestWitness()) } }
        }
        assertEquals(0, called.get())
    }

    @Test fun positiveEmptyHeadIsNotDowngradedAndFixedByteLimitIsEnforced(): Unit = runBlocking {
        val witness = planningTestWitness().copy(occurrenceLifecycle = RoutinePlanningLifecycle(99, emptyList()))
        val composer = RustScheduleComposer(bridge = { planningTestHelperResponse(witness) })
        assertEquals(99L, composer.compose(planningTestItems(), witness).occurrenceSnapshotRevision)
        val exact = composer.encodeWitnessRequest(planningTestItems(), witness)
        assertArrayEquals(exact, composer.encodeWitnessRequest(planningTestItems(), witness, exact.size))
        assertThrows(LocalScheduleCompositionRequestTooLargeException::class.java) { composer.encodeWitnessRequest(planningTestItems(), witness, exact.size - 1) }
    }

    @Test fun cancellationAcrossHelperAdmissionNeverFallsBackOrReturnsEvidence(): Unit = runBlocking {
        val admitted = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>(); val calls = AtomicInteger()
        val composer = RustScheduleComposer(bridge = { calls.incrementAndGet(); planningTestHelperResponse() }, beforeBridge = { admitted.complete(Unit); release.await() })
        val attempt = async { composer.compose(planningTestItems(), planningTestWitness()) }
        withTimeout(2_000) { admitted.await() }; attempt.cancel(); release.complete(Unit)
        withTimeout(2_000) { attempt.join() }
        assertTrue(attempt.isCancelled); assertEquals(0, calls.get())
    }
}
