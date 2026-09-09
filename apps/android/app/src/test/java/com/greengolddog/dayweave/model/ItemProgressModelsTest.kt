package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.ui.authoring.ItemProgressComponentForm
import com.greengolddog.dayweave.ui.authoring.ItemProgressFormKind
import java.io.File
import java.math.BigDecimal
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ItemProgressModelsTest {
    @Test fun sharedComponentCollectionsMatchStrictNativeContract() {
        val fixture = fixture("components-v1.json")
        for (valid in listOf(true, false)) for (case in fixture.getValue(if (valid) "valid" else "invalid").jsonArray) {
            val row = case.jsonObject
            val parsed = runCatching { decodeExactItemProgress<List<ItemProgressComponent>>(row.getValue("components").toString())
                .also(::requireProgressComponents) }
            assertEquals(row.getValue("name").jsonPrimitive.content, valid, parsed.isSuccess)
            if (valid) parsed.getOrThrow().forEach { assertEquals(it, ItemProgressComponentForm.from(it).component()) }
        }
    }

    @Test fun sharedDecimalAndUnicodeScalarFixturesRemainExact() {
        val fixture = fixture("values-v1.json")
        fixture.getValue("decimals").jsonArray.forEach { raw ->
            val row = raw.jsonObject
            val value = row.getValue("value").jsonPrimitive.content
            val valid = row.getValue("valid").jsonPrimitive.boolean
            assertEquals(value, valid, runCatching { requireCanonicalProgressDecimal(value) }.isSuccess)
            if (valid) assertEquals(row.getValue("scaled_millionths").jsonPrimitive.content,
                BigDecimal(value).movePointRight(6).toBigIntegerExact().toString())
        }
        fixture.getValue("labels").jsonArray.forEach { raw ->
            val row = raw.jsonObject
            val value = row.getValue("value").jsonPrimitive.content
            val maximum = row.getValue("max_scalars").jsonPrimitive.int
            assertEquals(value, row.getValue("valid").jsonPrimitive.boolean,
                runCatching { requireProgressText(value, maximum) }.isSuccess)
        }
    }

    @Test fun rawDuplicateKeysEscapedAliasesAndNoncanonicalIntegerTokensAreRejected() {
        val json = ITEM_PROGRESS_JSON.encodeToString(progressTestSnapshot())
        val invalid = listOf(json.replace("\"revision\":0", "\"revision\":0,\"revision\":0"),
            json.replace("\"revision\":0", "\"revision\":0,\"rev\\u0069sion\":0")) +
            listOf("0.0", "0e0", "-0", "\"0\"", "00").map { json.replace("\"revision\":0", "\"revision\":$it") }
        invalid.forEach { assertTrue(runCatching { decodeExactItemProgress<ItemProgressSnapshot>(it) }.isFailure) }
        val body = progressTestMutation().requestJson
        assertTrue(runCatching { decodeExactItemProgress<ItemProgressRequest>(body.replace("\"basis_points\":4250", "\"basis_points\":0,\"basis_points\":4250")) }.isFailure)
    }

    @Test fun revisionZeroRequiresExplicitEmptyInitialObservationAndKnownVersion() {
        listOf(progressTestSnapshot().copy(components = progressTestComponents()),
            progressTestSnapshot().copy(updatedAt = PROGRESS_NOW), progressTestSnapshot(1).copy(updatedAt = null),
            progressTestSnapshot().copy(schemaVersion = 2), progressTestSnapshot(1).copy(updatedAt = "2026-09-08T09:00:00.000000001Z"))
            .forEach { assertThrows(IllegalArgumentException::class.java, it::requireValid) }
    }

    @Test fun explicitFormReviewNormalizesDecimalWithoutChangingStableIdentityOrUnknownRemaining() {
        val form = ItemProgressComponentForm(PROGRESS_COMPONENT, "Chapters", ItemProgressFormKind.QUANTITY,
            current = "0003.5000", unit = "chapters", target = "0012.000", direction = ItemProgressDirection.AT_MOST)
        assertEquals(ItemProgressComponent(PROGRESS_COMPONENT, "Chapters", ItemProgressValue.Quantity("3.5", "chapters",
            ItemProgressTarget("12", ItemProgressDirection.AT_MOST))), form.component())
        assertEquals(ItemProgressValue.Time(0, null), ItemProgressComponentForm(PROGRESS_COMPONENT, "Reading", ItemProgressFormKind.TIME).component().value)
        assertEquals(ItemProgressValue.Percentage(4250), ItemProgressComponentForm(PROGRESS_COMPONENT, "Draft", ItemProgressFormKind.PERCENTAGE, percentage = "42.50").component().value)
    }

    @Test fun custodyBoundsAndWrongBindingAreRejected() {
        val original = progressTestMutation()
        listOf(original.copy(expectedItemRevision = 8), original.copy(operationId = PROGRESS_COMPONENT),
            original.copy(expectedProgressRevision = 2)).forEach { assertThrows(IllegalArgumentException::class.java, it::requireValid) }
        assertThrows(IllegalArgumentException::class.java) { progressTestLedger().copy(pending = listOf(original.copy(configurationId = "other"))).requireValid() }
        assertThrows(IllegalArgumentException::class.java) { ItemProgressLedger(pending = listOf(original)).requireValid() }
        assertThrows(IllegalArgumentException::class.java) { progressTestLedger().copy(pending = listOf(original, original)).requireValid() }
    }

    private fun fixture(name: String): JsonObject = Json.parseToJsonElement(generateSequence(File(requireNotNull(System.getProperty("user.dir")))) {
        it.parentFile }.map { File(it, "fixtures/item-progress/$name") }.first(File::isFile).readText()).jsonObject
}
