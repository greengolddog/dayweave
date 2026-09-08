package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.sync.toCanonicalItemReplacement
import com.greengolddog.dayweave.sync.toCreateCanonicalItemRequest
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorForm
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class CanonicalStructuralAuthoringTest {
    private val wire = Json { encodeDefaults = true; explicitNulls = false }

    @Test
    fun allSharedModernCreatesAndReplacementsRoundTripThroughEditorAndRealWireEncoder() {
        val cases = fixture().getValue("cases").jsonArray
        assertEquals(11, cases.size)
        cases.forEach { entry ->
            val case = entry.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            val raw = case.getValue("create").jsonObject
            val snapshot = item(raw)
            if (name == "event_fixed_bounds_are_not_a_deadline") {
                snapshot.requireCanonicalAuthoringShape()
                assertEquals(CanonicalDeadlineKind.NONE, snapshot.deadlineKind)
                assertEquals(raw["deadline_at"]?.jsonPrimitive?.content, snapshot.deadlineAt)
                // Server-valid imported events remain identity-only/read-only natively.
                assertThrows(IllegalArgumentException::class.java) { snapshot.toCanonicalDraft() }
                return@forEach
            }
            val draft = snapshot.toCanonicalDraft()
            assertEquals(name, draft, CanonicalItemEditorForm.from(draft).draft(snapshot.id).getOrThrow())
            val create = wire.encodeToJsonElement(draft.toCreateCanonicalItemRequest(snapshot.id, 2, 2)).jsonObject
            val replace = wire.encodeToJsonElement(draft.toCanonicalItemReplacement(snapshot.id, 2, 2)).jsonObject
            // The old optional keys retain their existing omission behavior. All five modern
            // structural keys, including null companions, must be physically present.
            val expected = JsonObject(raw.filter { (key, value) -> value != JsonNull || key in STRUCTURAL_KEYS })
            assertEquals(name, expected, create)
            assertEquals(name, JsonObject(expected - "id"), replace)
            assertTrue(name, create.keys.containsAll(STRUCTURAL_KEYS))
            val normalized = case["normalized_flexible_constraints"]?.let {
                snapshot.copy(flexibleConstraintsJson = it.toString())
            } ?: snapshot
            assertTrue(name, draft.matches(normalized))
            assertFalse(name, draft.matches(normalized.copy(title = "Different authoritative content")))
        }
    }

    @Test
    fun allSharedInvalidStructuralShapesRemainReadOnly() {
        val cases = fixture().getValue("invalid_cases").jsonArray
        assertEquals(11, cases.size)
        cases.forEach { entry ->
            val case = entry.jsonObject
            assertTrue(case.getValue("name").jsonPrimitive.content,
                runCatching { item(case.getValue("create").jsonObject).toCanonicalDraft() }.isFailure)
        }
    }

    @Test
    fun projectDemandRequiresExplicitOwnEffortAndNoEffectiveChildren() {
        val draft = CanonicalItemDraft(title = "Project", timezoneName = "UTC", kind = ItemKind.PROJECT,
            placement = CanonicalDraftPlacement.PLANNED, durationSeconds = 1800)
        assertFalse(draft.createsPlanningDemand(ID))
        val own = draft.copy(hasOwnEffort = true)
        assertTrue(own.createsPlanningDemand(ID))
        assertFalse(own.createsPlanningDemand(ID, hasChildren = true))
        assertNull(own.constraints.hasOwnEffort)
        assertThrows(IllegalArgumentException::class.java) { own.toCreateCanonicalItemRequest(ID, 2, 1) }
    }

    @Test
    fun civilDateDeadlineUsesStrictNextMidnightIncludingGapFoldAndCalendarBounds() {
        fun draft(date: String, timezone: String = "UTC", earliest: String? = null) = CanonicalItemDraft(
            title = "Date only", timezoneName = timezone, kind = ItemKind.PROJECT,
            deadlineKind = CanonicalDeadlineKind.DATE, deadlineDate = date,
            deadlineStrength = CanonicalDeadlineStrength.HARD, earliestStartAt = earliest)
        listOf("0001-01-01", "2024-02-29", "9999-12-30").forEach { draft(it).requireValid(ID) }
        listOf("0000-01-01", "9999-12-31", "2025-02-29", "2026-2-01").forEach {
            assertThrows(IllegalArgumentException::class.java) { draft(it).requireValid(ID) }
        }
        assertThrows(IllegalArgumentException::class.java) { draft("2011-12-29", "Pacific/Apia").requireValid(ID) }
        // Havana's next midnight is folded: choose 04:00Z, never the later 05:00Z boundary.
        draft("2020-10-31", "America/Havana", "2020-11-01T03:59:59Z").requireValid(ID)
        assertThrows(IllegalArgumentException::class.java) {
            draft("2020-10-31", "America/Havana", "2020-11-01T04:00:00Z").requireValid(ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            draft("2026-10-01", earliest = "2026-10-02T00:00:00Z").requireValid(ID)
        }
    }

    @Test
    fun reviewedOwnEffortUpdatesExistingMirrorWithoutInventingOne() {
        val bare = CanonicalItemDraft(title = "Goal", timezoneName = "UTC", kind = ItemKind.GOAL)
        val modern = CanonicalItemEditorForm.from(bare).copy(hasOwnEffort = true, hasOwnEffortSpecified = true)
            .draft(ID).getOrThrow()
        assertTrue(modern.hasOwnEffort)
        assertNull(modern.constraints.hasOwnEffort)
        val mirrored = bare.copy(constraints = bare.constraints.copy(hasOwnEffort = false))
        val edited = CanonicalItemEditorForm.from(mirrored).copy(hasOwnEffort = true, hasOwnEffortSpecified = true)
            .draft(ID).getOrThrow()
        assertTrue(edited.hasOwnEffort)
        assertEquals(true, edited.constraints.hasOwnEffort)
    }

    @Test
    fun ownedFixedEventRetainsEndWithoutAcquiringTaskDeadline() {
        val event = CanonicalItemDraft(title = "Owned event", timezoneName = "UTC", kind = ItemKind.EVENT,
            placement = CanonicalDraftPlacement.PLANNED, durationSeconds = 1800,
            earliestStartAt = "2026-10-02T12:00:00Z", deadlineAt = "2026-10-02T12:30:00Z",
            eventTiming = CanonicalEventTimingDraft("2026-10-02T12:00:00Z", "2026-10-02T12:30:00Z"))
        val edited = CanonicalItemEditorForm.from(event).copy(title = "Renamed event").draft(ID).getOrThrow()
        assertEquals(event.deadlineAt, edited.deadlineAt)
        assertEquals(CanonicalDeadlineKind.NONE, edited.deadlineKind)
        assertNull(edited.deadlineStrength)
        assertTrue(edited.createsPlanningDemand(ID, hasChildren = true))
        val body = wire.encodeToJsonElement(edited.toCreateCanonicalItemRequest(ID, 2, 2)).jsonObject
        assertEquals(JsonPrimitive("none"), body["deadline_kind"])
        assertEquals(JsonPrimitive(event.deadlineAt), body["deadline_at"])
        assertEquals(JsonNull, body["deadline_strength"])
    }

    private fun fixture(): JsonObject {
        val relative = "fixtures/structural-authoring/requests-v1.json"
        val file = generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
            .map { File(it, relative) }.first(File::isFile)
        return Json.parseToJsonElement(file.readText()).jsonObject.also {
            assertEquals("dayweave.structural-authoring-fixtures/1", it.getValue("schema").jsonPrimitive.content)
        }
    }

    private fun item(raw: JsonObject): CanonicalItemSnapshot {
        fun string(key: String) = raw[key]?.jsonPrimitive?.contentOrNull
        fun long(key: String) = raw[key]?.jsonPrimitive?.longOrNull
        return CanonicalItemSnapshot(
            id = requireNotNull(string("id")), isSensitive = raw.getValue("is_sensitive").jsonPrimitive.boolean,
            kind = requireNotNull(string("kind")), status = requireNotNull(string("status")),
            title = requireNotNull(string("title")), notes = string("notes"), timezoneName = requireNotNull(string("timezone_name")),
            durationSeconds = long("duration_seconds"), durationKind = CanonicalDurationKind(requireNotNull(string("duration_kind"))),
            durationMinSeconds = long("duration_min_seconds"), durationMaxSeconds = long("duration_max_seconds"),
            durationSource = string("duration_source")?.let(::CanonicalDurationSource),
            deadlineAt = string("deadline_at"), deadlineKind = CanonicalDeadlineKind(requireNotNull(string("deadline_kind"))),
            deadlineDate = string("deadline_date"), deadlineStrength = string("deadline_strength")?.let(::CanonicalDeadlineStrength),
            deadlineSoftWeight = long("deadline_soft_weight"), earliestStartAt = string("earliest_start_at"),
            recurrenceJson = raw["recurrence"]?.takeUnless { it == JsonNull }?.toString(),
            flexibleConstraintsJson = raw.getValue("flexible_constraints").toString(), splitPolicyJson = raw.getValue("split_policy").toString(),
            hasOwnEffort = raw.getValue("has_own_effort").jsonPrimitive.boolean,
            importance = requireNotNull(long("importance")).toInt(), urgency = requireNotNull(long("urgency")).toInt(),
            parentId = string("parent_id"), siblingOrder = requireNotNull(long("sibling_order")), isExecutable = true,
            revision = 1, createdAt = "2026-09-01T00:00:00Z", updatedAt = "2026-09-01T00:00:00Z",
            hasExplicitStructuralMetadata = true,
        )
    }

    private companion object {
        const val ID = "00000000-0000-4000-8000-000000001001"
        val STRUCTURAL_KEYS = setOf("deadline_kind", "deadline_date", "deadline_strength", "deadline_soft_weight", "has_own_effort")
    }
}
