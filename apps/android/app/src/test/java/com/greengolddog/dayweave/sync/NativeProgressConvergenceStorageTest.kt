package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.nio.file.Files
import javax.crypto.AEADBadTagException
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** Always-on, network-free checks of the opt-in harness's physical restart and custody adapter. */
class NativeProgressConvergenceStorageTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test fun terminalDeltaCursorKeepsHydratedGoalAndChildAdmittedAfterEncryptedRestart() = runBlocking {
        val goal = RemoteCanonicalItem(
            id = "00000000-0000-4000-8000-000000000011", isSensitive = false,
            kind = "goal", status = "planned", title = "Synthetic admitted goal", timezoneName = "UTC",
            durationKind = CanonicalDurationKind.UNKNOWN, deadlineKind = CanonicalDeadlineKind.NONE,
            flexibleConstraints = buildJsonObject {}, splitPolicy = buildJsonObject { put("type", "indivisible") },
            importance = 1, urgency = 1, siblingOrder = 0, hasOwnEffort = false,
            isExecutable = false, revision = 2, createdAt = PROGRESS_NOW, updatedAt = PROGRESS_NOW,
        )
        val child = goal.copy(id = "00000000-0000-4000-8000-000000000012", kind = "task",
            title = "Synthetic admitted child", parentId = goal.id, isExecutable = true, revision = 1,
            durationKind = CanonicalDurationKind.EXACT, durationSeconds = 60,
            durationMinSeconds = 60, durationMaxSeconds = 60, durationSource = CanonicalDurationSource.USER)
        val terminalCursor = "synthetic-actual-terminal-delta-cursor"
        val original = NativeConvergenceCanonicalRead(listOf(goal, child), terminalCursor)
            .initialState(PROGRESS_ORIGIN, PROGRESS_CONFIGURATION)
        assertNull(original.copy(canonicalDeltaCursor = null).progressItem(goal.id))
        val directory = temporary.root.toPath().resolve("android")
        val initialRepository = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
            NativeConvergenceDisk(directory, "synthetic-admission-binding", prepare = true)))
        initialRepository.save(original)
        val restartRepository = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
            NativeConvergenceDisk(directory, "synthetic-admission-binding", prepare = false)))
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(DayWeaveUiState(), restartRepository, scope)
            withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } }
            assertEquals(PlannerLoadState.READY, store.loadState.value)
            assertEquals(terminalCursor, store.state.value.canonicalDeltaCursor)
            assertEquals(original.canonicalItems, store.state.value.canonicalItems)
            assertNotNull(store.state.value.progressItem(goal.id))
            assertNotNull(store.state.value.progressItem(child.id))
            assertEquals(terminalCursor, restartRepository.load()?.canonicalDeltaCursor)
        } finally { scope.cancel() }
    }

    @Test fun productionSnapshotCodecRestartsWithExactEncryptedCustody() = runBlocking {
        val directory = temporary.root.toPath().resolve("android")
        val first = NativeConvergenceDisk(directory, "synthetic-binding", prepare = true)
        val pending = progressTestMutation(submitted = true).let { it.copy(requestJson = " \n${it.requestJson}\n ") }
        val original = progressTestState().copy(
            canonicalItems = listOf(progressTestItem().copy(status = "ready", splitPolicyJson = "{\"type\":\"indivisible\"}")),
            itemProgressLedger = progressTestLedger().copy(pending = listOf(pending)),
        )
        RoomPlannerStateRepository(NativeConvergenceSnapshotDao(first)).save(original)
        val encrypted = Files.readAllBytes(directory.resolve("snapshot.aesgcm")).toString(Charsets.ISO_8859_1)
        assertFalse(encrypted.contains(pending.operationId))
        assertFalse(encrypted.contains("itemProgressLedger"))
        val second = NativeConvergenceDisk(directory, "synthetic-binding", prepare = false)
        val restored = requireNotNull(RoomPlannerStateRepository(NativeConvergenceSnapshotDao(second)).load())
        assertEquals(original.canonicalItems, restored.canonicalItems)
        assertEquals(original.itemProgressLedger, restored.itemProgressLedger)
        assertEquals(pending.requestJson, restored.itemProgressLedger.pending.single().requestJson)
        requirePrivateConvergencePath(directory, directory = true)
        requirePrivateConvergencePath(directory.resolve("synthetic-test-key.bin"))
        requirePrivateConvergencePath(directory.resolve("snapshot.aesgcm"))
    }

    @Test fun changedRunBindingAndTamperedCiphertextFailClosed() {
        val directory = temporary.root.toPath().resolve("android")
        val disk = NativeConvergenceDisk(directory, "synthetic-first-binding", prepare = true)
        disk.write("snapshot", "synthetic exact payload")
        val wrong = NativeConvergenceDisk(directory, "synthetic-other-binding", prepare = false)
        assertThrows(AEADBadTagException::class.java) { wrong.read("snapshot") }
        val path = directory.resolve("snapshot.aesgcm")
        val tampered = Files.readAllBytes(path).also { it[it.lastIndex] = (it.last().toInt() xor 1).toByte() }
        atomicPrivateConvergenceWrite(path, tampered)
        assertThrows(AEADBadTagException::class.java) { disk.read("snapshot") }
    }

    @Test fun repeatedPrepareCannotOverwriteAnExistingRun() {
        val directory = temporary.root.toPath().resolve("android")
        val disk = NativeConvergenceDisk(directory, "synthetic-binding", prepare = true)
        disk.write("snapshot", "synthetic retained exact payload")
        val original = Files.readAllBytes(directory.resolve("snapshot.aesgcm"))
        assertThrows(IllegalArgumentException::class.java) { NativeConvergenceDisk(directory, "synthetic-binding", prepare = true) }
        assertArrayEquals(original, Files.readAllBytes(directory.resolve("snapshot.aesgcm")))
    }
}
