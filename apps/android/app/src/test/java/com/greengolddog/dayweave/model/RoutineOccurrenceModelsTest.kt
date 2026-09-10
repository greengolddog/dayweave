package com.greengolddog.dayweave.model

import java.io.File
import java.util.UUID
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RoutineOccurrenceModelsTest {
    @Test fun sharedClosedWireFixturesMatchProducerSemantics() {
        val fixture = Json.parseToJsonElement(generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
            .map { File(it, "fixtures/routine-occurrences/wire-v1.json") }.first(File::isFile).readText()).jsonObject
        assertEquals(1, fixture.getValue("schema_version").jsonPrimitive.int)
        for (group in listOf("valid", "invalid")) fixture.getValue(group).jsonArray.forEach { row ->
            val value = row.jsonObject
            val result = runCatching { when (value.getValue("kind").jsonPrimitive.content) {
                "snapshot" -> decodeExactRoutineOccurrence<RoutineOccurrenceSnapshot>(value.getValue("value").toString()).requireValid()
                "command" -> decodeExactRoutineOccurrence<RoutineOccurrenceRequest>(value.getValue("value").toString()).requireValid("00000000-0000-0000-0000-000000000003")
                "mutation" -> decodeExactRoutineOccurrence<RoutineOccurrenceMutationResult>(value.getValue("value").toString()).requireValid()
                "page" -> decodeExactRoutineOccurrence<RoutineOccurrencePage>(value.getValue("value").toString()).requireValid()
                else -> error("Unknown fixture kind")
            } }
            assertEquals(value.getValue("name").jsonPrimitive.content, group == "valid", result.isSuccess)
        }
    }

    @Test fun rawAliasesFractionsExponentsUnknownFieldsAndMissingNullablesAreRejected() {
        val original = ITEM_PROGRESS_JSON.encodeToString(routineTestSnapshot())
        val invalid = listOf(original.replaceFirst("\"revision\":1", "\"revision\":1,\"rev\\u0069sion\":1"),
            original.replaceFirst("\"completed_at\":null,", ""), original.replaceFirst("\"parent_id\":null,", ""),
            original.replaceFirst("\"provenance\":null,", ""), original.replaceFirst("\"schema_version\":1", "\"schema_version\":1,\"future\":true")) +
            listOf("1.0", "1e0", "-0", "\"1\"", "9223372036854775808").map { original.replaceFirst("\"revision\":1", "\"revision\":$it") }
        invalid.forEach { assertTrue(runCatching { decodeExactRoutineOccurrence<RoutineOccurrenceSnapshot>(it).requireValid() }.isFailure) }
        assertTrue(runCatching { decodeExactRoutineOccurrence<RoutineOccurrenceSnapshot>(original + " ".repeat(MAX_ROUTINE_OCCURRENCE_BYTES)).requireValid() }.isFailure)
    }

    @Test fun completeOptionalTreeRetainsBlockersAndChecksFixedPointAndCounts() {
        val initial = routineTestSnapshot(); initial.requireValid()
        val completed = routineTestSnapshot(true); completed.requireValid()
        assertEquals("Synthetic waiting for input", completed.aggregate.members.first().provenance?.reopen?.blockedReason)
        val parentStillOpen = completed.copy(aggregate = completed.aggregate.copy(members = listOf(initial.aggregate.members.first()) + completed.aggregate.members.drop(1)))
        val fakeCounts = initial.copy(members = initial.members.map { it.copy(counts = ItemCompletionCounts(0,0,0,0)) })
        val falseProof = initial.copy(members = initial.members.map { it.copy(occurrenceEvidenceRequired = true) })
        val unknown = initial.copy(aggregate = initial.aggregate.copy(members = initial.aggregate.members.map { it.copy(status = "future") }))
        listOf(parentStillOpen, fakeCounts, falseProof, unknown).forEach { assertTrue(runCatching { it.requireValid() }.isFailure) }
    }

    @Test fun nestedRecurringBranchRequiresItsOwnEvidenceWithoutVacuousParentCompletion() {
        val initial = routineTestSnapshot()
        val snapshot = initial.copy(aggregate = initial.aggregate.copy(manifest = initial.aggregate.manifest.copy(
            members = initial.aggregate.manifest.members.map { if (it.itemId == ROUTINE_CHILD) it.copy(recurs = true) else it })),
            members = initial.members.map { when (it.itemId) {
                ROUTINE_ROOT -> it.copy(counts = ItemCompletionCounts(1,0,0,1))
                ROUTINE_CHILD -> it.copy(occurrenceEvidenceRequired = true, reason = RoutineOccurrenceReason.OCCURRENCE_EVIDENCE_REQUIRED)
                else -> it
            } })
        snapshot.requireValid()
        assertTrue(runCatching { snapshot.copy(members = initial.members).requireValid() }.isFailure)
    }

    @Test fun identityUsesNominalContextNotClippedOrMovedWindowsAndPortableAnchors() {
        val initial = routineTestSnapshot()
        initial.copy(aggregate = initial.aggregate.copy(manifest = initial.aggregate.manifest.copy(windowStart = "2026-09-20T00:00:00Z", windowEnd = "2026-09-21T00:00:00Z"))).requireValid()
        val rolling = Json.parseToJsonElement("""{"type":"rolling_minutes","index":4294967295,"anchor":"2026-09-10T00:00:00.123456+23:59"}""").jsonObject
        initial.copy(aggregate = initial.aggregate.copy(manifest = initial.aggregate.manifest.copy(identity = rolling))).requireValid()
        val invalid = listOf(initial.aggregate.manifest.copy(occurrenceId = ROUTINE_INSTANCE),
            initial.aggregate.manifest.copy(id = ROUTINE_PLANNER_ID),
            initial.aggregate.manifest.copy(nominalEnd = "2026-09-12T00:00:00Z"),
            initial.aggregate.manifest.copy(timezoneName = "Unknown/Synthetic"),
            initial.aggregate.manifest.copy(windowEnd = initial.aggregate.manifest.windowStart),
            initial.aggregate.manifest.copy(nominalStart = "2026-09-10T00:00:00.1234567Z"))
        invalid.forEach { assertTrue(runCatching { it.requireValid() }.isFailure) }
    }

    @Test fun fullDeepTreeValidationIsIterativeAndMissingParentsCyclesAndCountsFailClosed() {
        val base = routineTestSnapshot()
        val count = 5_000
        val ids = (1..count).map { UUID(0,it.toLong()).toString() }
        val open = ItemCompletionReopening("planned",null,null,null)
        val definitions = ids.mapIndexed { index,id -> RoutineOccurrenceMemberDefinition(id, if (index == 0) null else ids[index-1],
            Long.MAX_VALUE, "Synthetic member", if (index == 0) "routine" else "task", index == 0, 0, true, open) }
        val states = definitions.map { RoutineOccurrenceMemberState(it.itemId,Long.MAX_VALUE,"planned",true,ItemCompletionMode.AUTOMATIC,open,null,null,ROUTINE_NOW) }
        val snapshot = base.copy(aggregate = RoutineOccurrenceAggregate(base.aggregate.manifest.copy(members = definitions),Long.MAX_VALUE,states),
            members = ids.mapIndexed { index,id -> RoutineOccurrenceMemberEvaluation(id,ItemCompletionCounts((count-index-1).toLong(),0,(count-index-1).toLong(),0),false,RoutineOccurrenceReason.UNCHANGED) })
        snapshot.requireValid()
        decodeExactRoutineOccurrence<RoutineOccurrenceSnapshot>(ITEM_PROGRESS_JSON.encodeToString(snapshot)).requireValid()
        val missing = snapshot.aggregate.manifest.copy(members = definitions.dropLast(1))
        assertTrue(runCatching { snapshot.copy(aggregate = snapshot.aggregate.copy(manifest = missing)).requireValid() }.isFailure)
        val cycle = base.aggregate.manifest.copy(members = base.aggregate.manifest.members.map { when (it.itemId) {
            ROUTINE_CHILD -> it.copy(parentId = ROUTINE_OPTIONAL)
            ROUTINE_OPTIONAL -> it.copy(parentId = ROUTINE_CHILD)
            else -> it
        } })
        assertTrue(runCatching { cycle.requireValid() }.isFailure)
    }

    @Test fun mutationBindsLedgerRouteMemberBothCasRevisionsAndAllThreeActions() {
        val request = routineTestRequest()
        routineTestMutation().requireMatches(ROUTINE_INSTANCE,ROUTINE_CHILD,request)
        listOf(request.copy(expectedInstanceRevision = Long.MAX_VALUE),request.copy(expectedMemberRevision = Long.MAX_VALUE),
            request.copy(operationId = ROUTINE_ROOT),request.copy(action = RoutineOccurrenceAction.SetOutcome("skipped")))
            .forEach { assertTrue(runCatching { routineTestMutation().requireMatches(ROUTINE_INSTANCE,ROUTINE_CHILD,it) }.isFailure) }
        assertTrue(runCatching { routineTestMutation().requireMatches(ROUTINE_PLANNER_ID,ROUTINE_CHILD,request) }.isFailure)
        val actions = listOf(RoutineOccurrenceAction.SetOutcome("skipped"),RoutineOccurrenceAction.Reopen(ItemCompletionReopening("planned",null,null,null)),
            RoutineOccurrenceAction.SetPolicy(false,ItemCompletionMode.KEEP_OPEN))
        actions.forEach { decodeExactRoutineOccurrence<RoutineOccurrenceRequest>(ITEM_PROGRESS_JSON.encodeToString(request.copy(action=it))).requireValid(ROUTINE_CHILD) }
    }

    @Test fun actionReceiptsRequireTheProducerTargetReasonEvenForHistoricalReplay() {
        val outcome = routineTestMutation(replayed = true)
        val opened = routineTestSnapshot().let { initial -> initial.copy(
            aggregate = initial.aggregate.copy(revision = 3, members = initial.aggregate.members.map {
                if (it.itemId == ROUTINE_OPTIONAL) it else it.copy(revision = 3)
            }), members = initial.members.map { it.copy(reason = when (it.itemId) {
                ROUTINE_CHILD -> RoutineOccurrenceReason.REOPENED
                ROUTINE_ROOT -> RoutineOccurrenceReason.AUTOMATICALLY_REOPENED
                else -> it.reason
            }) }) }
        val reopen = RoutineOccurrenceMutationResult(ROUTINE_OPERATION, true, opened)
        val reopenRequest = routineTestRequest().copy(expectedInstanceRevision = 2, expectedMemberRevision = 2,
            action = RoutineOccurrenceAction.Reopen(ItemCompletionReopening("planned", null, null, null)))
        for ((receipt, request, expected) in listOf(
            Triple(outcome, routineTestRequest(), RoutineOccurrenceReason.OUTCOME_RECORDED),
            Triple(reopen, reopenRequest, RoutineOccurrenceReason.REOPENED),
        )) {
            receipt.requireMatches(ROUTINE_INSTANCE, ROUTINE_CHILD, request)
            RoutineOccurrenceReason.entries.filter { it != expected }.forEach { reason ->
                val contradictory = receipt.copy(occurrence = receipt.occurrence.copy(members = receipt.occurrence.members.map {
                    if (it.itemId == ROUTINE_CHILD) it.copy(reason = reason) else it
                }))
                contradictory.requireValid() // Standalone GET reasons do not establish command binding.
                assertTrue(runCatching { contradictory.requireMatches(ROUTINE_INSTANCE, ROUTINE_CHILD, request) }.isFailure)
            }
        }
    }

    @Test fun orderedWholeInstancePagesDistinguishListAndDeltaAndRejectNonadvancingProof() {
        val page = routineTestPage(); page.requireValid(true)
        val delta = page.copy(changes = page.changes + RoutineOccurrenceChange(2,routineTestSnapshot(true)))
        delta.requireValid()
        assertTrue(runCatching { delta.requireValid(true) }.isFailure)
        listOf(page.copy(changes = listOf(RoutineOccurrenceChange(0,routineTestSnapshot()))),
            page.copy(hasMore = true,changes = emptyList()),page.copy(cursor = ""),delta.copy(changes = delta.changes.reversed()))
            .forEach { assertTrue(runCatching { it.requireValid() }.isFailure) }
        val later = routineTestSnapshot(true)
        val changedManifest = later.copy(aggregate = later.aggregate.copy(manifest = later.aggregate.manifest.copy(
            members = later.aggregate.manifest.members.map { it.copy(title = "Different first-capture title") })))
        changedManifest.requireValid() // Each snapshot alone is coherent; the immutable sequence is not.
        assertTrue(runCatching { page.copy(changes = page.changes + RoutineOccurrenceChange(2, changedManifest)).requireValid() }.isFailure)
    }

    @Test fun programmaticModelsAndWholePagesEnforceCompactUtf8ByteBudget() {
        fun wide(count: Int): RoutineOccurrenceSnapshot {
            val base = routineTestSnapshot()
            val open = ItemCompletionReopening("planned", null, null, null)
            val ids = (1..count).map { UUID(0, it.toLong()).toString() }
            val title = "🧪".repeat(500) // Legal scalar length, four UTF-8 bytes per scalar.
            val definitions = ids.mapIndexed { index, id -> RoutineOccurrenceMemberDefinition(id,
                if (index == 0) null else ids.first(), 1, title, if (index == 0) "routine" else "task",
                index == 0, 0, true, open) }
            return base.copy(aggregate = RoutineOccurrenceAggregate(base.aggregate.manifest.copy(members = definitions), 2,
                ids.map { RoutineOccurrenceMemberState(it, 1, "planned", true, ItemCompletionMode.AUTOMATIC, open, null, null, ROUTINE_NOW) }),
                members = ids.mapIndexed { index, id -> RoutineOccurrenceMemberEvaluation(id,
                    if (index == 0) ItemCompletionCounts((count - 1).toLong(), 0, (count - 1).toLong(), 0) else ItemCompletionCounts(0, 0, 0, 0),
                    false, RoutineOccurrenceReason.UNCHANGED) })
        }
        val oversized = wide(5_000)
        oversized.requireStructure() // Count, strings, topology and aggregate fixed point are valid.
        assertTrue(runCatching { oversized.aggregate.manifest.requireValid() }.isFailure)
        assertTrue(runCatching { oversized.aggregate.requireValid() }.isFailure)
        assertTrue(runCatching { oversized.requireValid() }.isFailure)
        assertTrue(runCatching { RoutineOccurrenceMutationResult(ROUTINE_OPERATION, false, oversized).requireValid() }.isFailure)

        val bounded = wide(1_000)
        bounded.requireValid()
        val page = RoutineOccurrencePage(1, (1..4).map { sequence -> RoutineOccurrenceChange(sequence.toLong(),
            bounded.copy(aggregate = bounded.aggregate.copy(manifest = bounded.aggregate.manifest.copy(id = UUID(1, sequence.toLong()).toString())))) },
            "DWR1.synthetic-checkpoint", false)
        page.changes.forEach { it.occurrence.requireValid() }
        assertTrue(runCatching { page.requireValid(isCurrentList = true) }.isFailure)
    }
}
