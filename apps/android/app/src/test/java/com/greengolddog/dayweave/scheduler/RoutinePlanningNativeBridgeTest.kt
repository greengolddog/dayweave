package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.*
import java.io.File
import java.time.Instant
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/**
 * Opt-in host JNI integration, not an Android-device or provider acceptance test.
 * The root-owned harness builds the actual Rust cdylib and supplies its library path.
 * No byte bridge, clock, network, canonical-store or scheduler-result double is used.
 */
class RoutinePlanningNativeBridgeTest {
    @Test fun actualJniAndNativeCodecRetainEveryQualifiedProducerPlan(): Unit = runBlocking {
        assumeTrue(System.getenv("DAYWEAVE_ROUTINE_NATIVE_BRIDGE_TEST") == "1")
        val root = generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
            .map { File(it, "fixtures/routine-planning-witness/wire-v1.json") }.first(File::isFile)
        val samples = Json.parseToJsonElement(root.readText()).jsonObject.getValue("qualified").jsonArray
        assertTrue(samples.isNotEmpty())
        for (entry in samples) {
            val sample = entry.jsonObject
            val requestJson = sample.getValue("request").toString()
            val request = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessRequest>(requestJson)
            val response = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(sample.getValue("response").toString())
            response.requireValid()
            val witness = (response.result as RoutinePlanningWitnessResult.Qualified).witness
            // Capsule custody uses ID order; the helper request-byte binding
            // must use that retained order, not the corpus container order.
            val items = sample.getValue("canonical_items").jsonArray.map(::canonical).sortedBy { it.id }
            witness.requireMatches(request, witness.workspaceId, witness.userId, items)

            // Deliberately use the production default JNI bridge here.
            val composition = RustScheduleComposer().compose(items, witness)
            assertEquals(witness.occurrenceLifecycle.snapshotRevision, composition.occurrenceSnapshotRevision)
            assertEquals(witness.localInputFingerprint, composition.composition.localInputFingerprint)
            assertEquals(witness.sourceItemRevisions, composition.composition.sourceItemRevisions)
            assertEquals(items.size, composition.composition.acceptedItemCount)
            assertTrue(composition.composition.rejectedItems.isEmpty())
            val raw = requireNotNull(composition.helperResponseJson)
            assertEquals(sample.getValue("helper_response"), Json.parseToJsonElement(raw))

            // Exercise complete display artifact custody against the real result,
            // without admitting it to any live state, timer or publication path.
            val input = DayWeaveUiState(canonicalItems = items,
                canonicalSyncOrigin = "https://synthetic.example", canonicalConfigurationId = "synthetic-native-binding")
            val capsule = RoutinePlanningInputCapsule.create(input, requestJson, witness, request.schedule.asOf)
            val display = RoutinePlanningDisplaySnapshot.create(capsule, composition,
                Instant.parse(capsule.capturedAt).plusSeconds(1).toString())
            val saved = ROUTINE_PLANNING_JSON.encodeToString(RoutinePlanningDisplaySnapshot.serializer(), display)
            val restored = ROUTINE_PLANNING_JSON.decodeFromString(RoutinePlanningDisplaySnapshot.serializer(), saved)
            assertEquals(display, restored)
            assertEquals(raw, restored.helperResponseJson)
            assertEquals(composition, restored.validateAndDecode(capsule))
            assertEquals(request.schedule.asOf, restored.capturedAt)
        }
    }

    private fun canonical(value: JsonElement): CanonicalItemSnapshot {
        val fields = value.jsonObject.map { (name, entry) ->
            when (name) {
                "recurrence" -> "recurrenceJson" to if (entry == JsonNull) JsonNull else JsonPrimitive(entry.toString())
                "flexible_constraints" -> "flexibleConstraintsJson" to JsonPrimitive(entry.toString())
                "split_policy" -> "splitPolicyJson" to JsonPrimitive(entry.toString())
                else -> name.split('_').let { parts ->
                    parts.first() + parts.drop(1).joinToString("") { it.replaceFirstChar(Char::uppercaseChar) }
                } to entry
            }
        }.toMap() + ("hasExplicitStructuralMetadata" to JsonPrimitive(true))
        return ROUTINE_PLANNING_JSON.decodeFromJsonElement(JsonObject(fields))
    }
}
