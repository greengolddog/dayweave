package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.*
import java.io.File
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

/** Real Rust producer output; the injected byte bridge does not claim a device/JNI execution. */
class RoutinePlanningWitnessCorpusTest {
    @Test fun realProducerRequestResponseAndHelperEnvelopesAreLosslessAcrossAllQualifiedCases(): Unit = runBlocking {
        val fixture = corpus()
        assertEquals(1, fixture.getValue("schema_version").jsonPrimitive.int)
        fixture.getValue("qualified").jsonArray.forEach { row ->
            val sample = row.jsonObject
            val request = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessRequest>(sample.getValue("request").toString())
            val response = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(sample.getValue("response").toString())
            val witness = (response.result as RoutinePlanningWitnessResult.Qualified).witness
            val items = sample.getValue("canonical_items").jsonArray.map(::canonical)
            response.requireValid()
            witness.requireMatches(request, witness.workspaceId, witness.userId, items)
            assertEquals(sample.getValue("response"), ROUTINE_PLANNING_JSON.encodeToJsonElement(response))
            val composer = RustScheduleComposer(bridge = { encoded ->
                assertEquals(sample.getValue("name").jsonPrimitive.content, sample.getValue("helper_request"), Json.parseToJsonElement(encoded.toString(Charsets.UTF_8)))
                (sample.getValue("helper_response").toString() + "\n").toByteArray()
            })
            val composed = composer.compose(items, witness)
            assertEquals(witness.occurrenceLifecycle.snapshotRevision, composed.occurrenceSnapshotRevision)
            assertEquals(witness.localInputFingerprint, composed.composition.localInputFingerprint)
            assertEquals(witness.sourceItemRevisions, composed.composition.sourceItemRevisions)
            assertEquals(sample.getValue("helper_response").jsonObject.getValue("result").jsonObject.getValue("composition").jsonObject.getValue("plan"),
                ROUTINE_PLANNING_JSON.encodeToJsonElement(composed.composition.plan))
        }
    }

    @Test fun allProducerRemoteAndRejectedCasesAreDiscoveredWithoutHardCodedCounts() {
        val fixture = corpus()
        val bases = fixture.getValue("qualified").jsonArray.associate { row -> row.jsonObject.getValue("name").jsonPrimitive.content to row.jsonObject }
        fixture.getValue("remote_required").jsonArray.forEach { row ->
            val response = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(row.jsonObject.getValue("response").toString())
            response.requireValid(); assertTrue(response.result is RoutinePlanningWitnessResult.RemoteRequired)
        }
        for (group in listOf("invalid", "raw_invalid")) fixture.getValue(group).jsonArray.forEach { row ->
            val sample = row.jsonObject; val base = bases.getValue(sample.getValue("base").jsonPrimitive.content)
            val original = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessRequest>(base.getValue("request").toString())
            val originalResponse = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(base.getValue("response").toString())
            val expected = (originalResponse.result as RoutinePlanningWitnessResult.Qualified).witness
            val body = if (group == "invalid") sample.getValue("response").toString() else sample.getValue("raw").jsonPrimitive.content
            val accepted = runCatching {
                val decoded = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(body)
                decoded.requireValid()
                (decoded.result as RoutinePlanningWitnessResult.Qualified).witness.requireMatches(original, expected.workspaceId, expected.userId,
                    base.getValue("canonical_items").jsonArray.map(::canonical))
            }.isSuccess
            assertFalse(sample.getValue("name").jsonPrimitive.content, accepted)
        }
    }

    private fun corpus() = Json.parseToJsonElement(generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
        .map { File(it, "fixtures/routine-planning-witness/wire-v1.json") }.first(File::isFile).readText()).jsonObject

    private fun canonical(value: JsonElement): CanonicalItemSnapshot {
        val renamed = value.jsonObject.map { (key, entry) ->
            when (key) {
                "recurrence" -> "recurrenceJson" to if (entry == JsonNull) JsonNull else JsonPrimitive(entry.toString())
                "flexible_constraints" -> "flexibleConstraintsJson" to JsonPrimitive(entry.toString())
                "split_policy" -> "splitPolicyJson" to JsonPrimitive(entry.toString())
                else -> key.split('_').let { pieces -> pieces.first() + pieces.drop(1).joinToString("") { it.replaceFirstChar(Char::uppercaseChar) } } to entry
            }
        }.toMap() + ("hasExplicitStructuralMetadata" to JsonPrimitive(true))
        return ROUTINE_PLANNING_JSON.decodeFromJsonElement(JsonObject(renamed))
    }
}
