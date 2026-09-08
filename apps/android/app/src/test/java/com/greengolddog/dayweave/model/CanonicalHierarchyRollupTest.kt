package com.greengolddog.dayweave.model

import java.io.File
import java.util.UUID
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CanonicalHierarchyRollupTest {
    @Test
    fun sharedCanonicalSecondFixturesMatchEverySubtreeInEitherInputOrder() {
        val relative = "fixtures/hierarchy-progress/projection-v1.json"
        val fixture = generateSequence(File(requireNotNull(System.getProperty("user.dir")))) { it.parentFile }
            .map { File(it, relative) }.first(File::isFile)
        val document = Json.parseToJsonElement(fixture.readText()).jsonObject
        assertEquals("dayweave.hierarchy-progress-fixtures/1", document.getValue("schema").jsonPrimitive.content)
        document.getValue("cases").jsonArray.forEach { case ->
            val fields = case.jsonObject
            val name = fields.getValue("name").jsonPrimitive.content
            val nodes = fields.getValue("items").jsonArray.map { node(it.jsonObject) }
            val expected = fields.getValue("expected").jsonObject.mapValues { totals(it.value.jsonObject) }
            assertEquals(name, expected, CanonicalHierarchyRollup.build(nodes))
            assertEquals("$name reversed", expected, CanonicalHierarchyRollup.build(nodes.reversed()))
        }
        document.getValue("invalid_cases").jsonArray.forEach { case ->
            val fields = case.jsonObject
            // Native exact integers reject out-of-range wire input before evaluating the forest.
            val result = runCatching {
                CanonicalHierarchyRollup.build(fields.getValue("items").jsonArray.map { node(it.jsonObject) })
            }.getOrNull()
            assertNull(fields.getValue("name").jsonPrimitive.content, result)
        }
    }

    @Test
    fun fiveThousandDeepForestAndRecurrenceAreIterative() {
        val nodes = (1..5_000).map { index ->
            HierarchyRollupNode(id(index), if (index == 1) null else id(index - 1),
                "task", "completed", hasOwnEffort = true,
                duration = HierarchyEffortEstimate(1, 30, 59))
        }
        val totals = requireNotNull(CanonicalHierarchyRollup.build(nodes.reversed()))
        assertEquals(5_000, totals.size)
        assertEquals(HierarchyRollupTotals(completedLeafItems = 1, minimumEstimateSeconds = 1,
            expectedEstimateSeconds = 30, maximumEstimateSeconds = 59), totals[id(1)])
        val recurring = requireNotNull(CanonicalHierarchyRollup.build(nodes.mapIndexed { index, node ->
            if (index == 0) node.copy(recurs = true) else node
        }))
        assertEquals(HierarchyRollupTotals(recurringLeafItems = 1), recurring[id(1)])
    }

    @Test
    fun futureKindsAndStatusesAndMalformedEstimatesHaveNoNumericalAnswer() {
        val node = HierarchyRollupNode(id(1), null, "task", "inbox")
        listOf(node.copy(kind = "future_kind"), node.copy(status = "future_status"),
            node.copy(duration = HierarchyEffortEstimate(0, 1, 1)),
            node.copy(duration = HierarchyEffortEstimate(2, 1, 3)),
        ).forEach { assertNull(CanonicalHierarchyRollup.build(listOf(it))) }
    }

    private fun node(fields: JsonObject) = HierarchyRollupNode(
        id = fields.text("id"), parentId = fields.getValue("parent_id").jsonPrimitive.contentOrNull,
        kind = fields.text("kind"), status = fields.text("status"),
        hasOwnEffort = fields.getValue("has_own_effort").jsonPrimitive.boolean,
        recurs = fields.getValue("recurs").jsonPrimitive.boolean,
        hasChildrenOutsidePlan = fields.getValue("has_children_outside_plan").jsonPrimitive.boolean,
        duration = fields.getValue("duration").takeUnless { it == JsonNull }?.jsonObject?.let {
            HierarchyEffortEstimate(it.number("minimum_seconds"), it.number("expected_seconds"), it.number("maximum_seconds"))
        },
    )

    private fun totals(fields: JsonObject) = HierarchyRollupTotals(
        fields.number("completed_leaf_items"), fields.number("skipped_leaf_items"),
        fields.number("cancelled_leaf_items"), fields.number("open_leaf_items"),
        fields.number("recurring_leaf_items"), fields.number("fixed_events"), fields.number("unknown_estimates"),
        fields.number("minimum_estimate_seconds"), fields.number("expected_estimate_seconds"),
        fields.number("maximum_estimate_seconds"),
    )

    private fun JsonObject.text(key: String) = getValue(key).jsonPrimitive.content
    private fun JsonObject.number(key: String) = getValue(key).jsonPrimitive.long
    private fun id(value: Int) = UUID(0, value.toLong()).toString()
}
