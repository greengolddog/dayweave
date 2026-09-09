package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.network.RemoteItemDeltaChange
import com.greengolddog.dayweave.network.RemoteItemTombstone
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Test

class NativeProgressConvergenceDeltaTest {
    @Test fun duplicateAndIncreasingUpsertsFoldToOneCurrentRecord() {
        val records = linkedMapOf<String, RemoteCanonicalItem>()
        val first = item()
        val second = first.copy(revision = 2, title = "Synthetic updated goal")
        listOf(first, first, second, second).forEach {
            foldNativeConvergenceCanonicalChange(records, RemoteItemDeltaChange("upsert", item = it))
        }
        assertEquals(listOf(second), records.values.toList())
    }

    @Test fun equalRevisionForkAndRegressingRevisionCannotBeHiddenByFolding() {
        val current = item().copy(revision = 2)
        for (invalid in listOf(current.copy(title = "Synthetic conflicting goal"), current.copy(revision = 1))) {
            val records = linkedMapOf(current.id to current)
            assertThrows(IllegalArgumentException::class.java) {
                foldNativeConvergenceCanonicalChange(records, RemoteItemDeltaChange("upsert", item = invalid))
            }
            assertEquals(listOf(current), records.values.toList())
        }
    }

    @Test fun tombstoneMustBeNewerThanCurrentRecordBeforeRemoval() {
        val current = item().copy(revision = 2)
        val records = linkedMapOf(current.id to current)
        val tombstone = RemoteItemTombstone(current.id, 2, INSTANT)
        assertThrows(IllegalArgumentException::class.java) {
            foldNativeConvergenceCanonicalChange(records, RemoteItemDeltaChange("tombstone", tombstone = tombstone))
        }
        assertEquals(current, records[current.id])
        foldNativeConvergenceCanonicalChange(records, RemoteItemDeltaChange("tombstone", tombstone = tombstone.copy(revision = 3)))
        assertTrue(records.isEmpty())
    }

    @Test fun mixedDeltaVariantIsRejectedWithoutChangingState() {
        val records = linkedMapOf<String, RemoteCanonicalItem>()
        val current = item()
        val tombstone = RemoteItemTombstone(current.id, 2, INSTANT)
        for (type in listOf("upsert", "tombstone")) {
            assertThrows(IllegalArgumentException::class.java) {
                foldNativeConvergenceCanonicalChange(records, RemoteItemDeltaChange(type, current, tombstone))
            }
        }
        assertTrue(records.isEmpty())
    }

    @Test fun onlyBothAbsentOptInSkipsAndPartialOrBlankConfigurationFails() {
        assertFalse(nativeConvergenceOptIn(null, null))
        assertTrue(nativeConvergenceOptIn("/synthetic/config.json", "prepare"))
        for ((config, phase) in listOf(null to "prepare", "/synthetic/config.json" to null,
                "" to "prepare", "/synthetic/config.json" to "")) {
            assertThrows(IllegalArgumentException::class.java) { nativeConvergenceOptIn(config, phase) }
        }
    }

    private fun item() = RemoteCanonicalItem(
        id = "00000000-0000-4000-8000-000000000001", isSensitive = false,
        kind = "goal", status = "ready", title = "Synthetic goal", timezoneName = "UTC",
        durationKind = CanonicalDurationKind.UNKNOWN, deadlineKind = CanonicalDeadlineKind.NONE,
        flexibleConstraints = buildJsonObject {}, splitPolicy = buildJsonObject { put("type", "indivisible") },
        importance = 1, urgency = 1, siblingOrder = 0, hasOwnEffort = false,
        isExecutable = false, revision = 1, createdAt = INSTANT, updatedAt = INSTANT,
    )

    private companion object { const val INSTANT = "2026-09-09T07:00:00Z" }
}
