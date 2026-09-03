package com.greengolddog.dayweave.onboarding

import android.content.Context
import java.io.DataOutputStream
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AtomicFileOnboardingCheckpointStoreTest {
    private lateinit var noBackupDirectory: File
    private lateinit var store: AtomicFileOnboardingCheckpointStore

    @Before
    fun setUp() {
        noBackupDirectory = Files.createTempDirectory("dayweave-onboarding-test").toFile()
        store = AtomicFileOnboardingCheckpointStore(noBackupDirectory)
    }

    @After
    fun tearDown() {
        noBackupDirectory.deleteRecursively()
    }

    @Test
    fun missingFileIsFreshHealthyWelcomeAndContextConstructorUsesNoBackupDirectory() {
        assertEquals(
            OnboardingCheckpointLoadResult.Loaded(OnboardingCheckpoint.fresh()),
            store.load(),
        )
        assertEquals(noBackupDirectory, store.recordFile.parentFile)

        val context: Context = RuntimeEnvironment.getApplication()
        val contextStore = AtomicFileOnboardingCheckpointStore(context)
        assertEquals(context.noBackupFilesDir, contextStore.recordFile.parentFile)
    }

    @Test
    fun exactClosedCheckpointRoundTripsAcrossStoreInstances() {
        val acknowledged = OnboardingCheckpoint.fresh().copy(privacyAcknowledged = true)
        assertTrue(store.saveIfCurrent(OnboardingCheckpoint.fresh(), acknowledged))

        val api = acknowledged.copy(
            currentStep = OnboardingStep.API,
            furthestStep = OnboardingStep.API,
        )
        assertTrue(store.saveIfCurrent(acknowledged, api))

        assertEquals(AtomicFileOnboardingCheckpointStore.ENCODED_RECORD_BYTES, store.recordFile.length())
        assertEquals(
            OnboardingCheckpointLoadResult.Loaded(api),
            AtomicFileOnboardingCheckpointStore(noBackupDirectory).load(),
        )
    }

    @Test
    fun everyStepAndExactReadyCompletionPersistThroughTheAtomicStore() {
        val controller = OnboardingController(store)
        assertTrue(controller.acknowledgePrivacy())
        assertTrue(controller.advance())
        OnboardingStep.entries.drop(2).forEach { expected ->
            assertTrue(controller.advance(prerequisiteReady = true))
            assertEquals(expected, (controller.state as OnboardingControllerState.Active).currentStep)
        }
        assertTrue(controller.complete(allPrerequisitesReady = true))

        val completed = (controller.state as OnboardingControllerState.Active).checkpoint
        assertTrue(completed.completed)
        assertEquals(
            OnboardingCheckpointLoadResult.Loaded(completed),
            AtomicFileOnboardingCheckpointStore(noBackupDirectory).load(),
        )
    }

    @Test
    fun staleWriterAndNonAdjacentJumpCannotReplaceCheckpoint() {
        val fresh = OnboardingCheckpoint.fresh()
        val acknowledged = fresh.copy(privacyAcknowledged = true)
        assertTrue(store.saveIfCurrent(fresh, acknowledged))

        val invalidJump = acknowledged.copy(
            currentStep = OnboardingStep.GOOGLE,
            furthestStep = OnboardingStep.GOOGLE,
        )
        assertFalse(store.saveIfCurrent(acknowledged, invalidJump))
        assertFalse(store.saveIfCurrent(fresh, acknowledged))
        assertEquals(OnboardingCheckpointLoadResult.Loaded(acknowledged), store.load())
    }

    @Test
    fun atomicBackupRecoversLastCompleteCheckpointAfterTornWrite() {
        val acknowledged = OnboardingCheckpoint.fresh().copy(privacyAcknowledged = true)
        assertTrue(store.saveIfCurrent(OnboardingCheckpoint.fresh(), acknowledged))
        val backup = File(store.recordFile.path + ".bak")
        Files.copy(
            store.recordFile.toPath(),
            backup.toPath(),
            StandardCopyOption.REPLACE_EXISTING,
        )
        store.recordFile.writeBytes(byteArrayOf(0x44, 0x57))

        assertEquals(OnboardingCheckpointLoadResult.Loaded(acknowledged), store.load())
        assertFalse(backup.exists())
    }

    @Test
    fun unfinishedInitialNewArtifactIsCorruptRatherThanAFirstInstall() {
        val newArtifact = File(store.recordFile.path + ".new")
        newArtifact.writeBytes(byteArrayOf(0x44, 0x57))

        assertTrue(store.load() is OnboardingCheckpointLoadResult.Corrupt)
    }

    @Test
    fun strictDecoderRejectsMalformedFutureExpandedAndInvalidInvariantRecords() {
        val corruptWriters: List<(DataOutputStream) -> Unit> = listOf(
            { writeRawRecord(it, magic = 0x01020304) },
            { writeRawRecord(it, version = 0) },
            { writeRawRecord(it, version = OnboardingCheckpoint.CURRENT_VERSION + 1) },
            { writeRawRecord(it, currentStep = 99) },
            { writeRawRecord(it, furthestStep = 99) },
            {
                writeRawRecord(
                    it,
                    currentStep = OnboardingStep.GOOGLE.wireValue,
                    furthestStep = OnboardingStep.API.wireValue,
                    privacy = 1,
                )
            },
            {
                writeRawRecord(
                    it,
                    currentStep = OnboardingStep.API.wireValue,
                    furthestStep = OnboardingStep.API.wireValue,
                    privacy = 0,
                )
            },
            { writeRawRecord(it, privacy = 2) },
            { writeRawRecord(it, completed = 2) },
            {
                writeRawRecord(
                    it,
                    privacy = 1,
                    completed = 1,
                )
            },
            {
                writeRawRecord(
                    it,
                    currentStep = OnboardingStep.READY.wireValue,
                    furthestStep = OnboardingStep.READY.wireValue,
                    privacy = 1,
                    completed = 1,
                    trailingByte = 1,
                )
            },
        )

        corruptWriters.forEach { writer ->
            removeArtifacts()
            DataOutputStream(store.recordFile.outputStream()).use(writer)
            assertTrue(store.load() is OnboardingCheckpointLoadResult.Corrupt)
        }
    }

    @Test
    fun recordsOverTwoKiBAreRejectedWithoutUnboundedParsing() {
        store.recordFile.writeBytes(
            ByteArray(AtomicFileOnboardingCheckpointStore.MAX_RECORD_BYTES.toInt() + 1) { 0x41 },
        )

        assertTrue(store.load() is OnboardingCheckpointLoadResult.Corrupt)
    }

    @Test
    fun corruptAndFutureRecordsAreNotOverwrittenWithoutExactApprovedReset() {
        DataOutputStream(store.recordFile.outputStream()).use {
            writeRawRecord(it, version = OnboardingCheckpoint.CURRENT_VERSION + 1)
        }
        val originalBytes = store.recordFile.readBytes()
        val corrupt = store.load() as OnboardingCheckpointLoadResult.Corrupt
        val acknowledged = OnboardingCheckpoint.fresh().copy(privacyAcknowledged = true)

        assertFalse(store.saveIfCurrent(OnboardingCheckpoint.fresh(), acknowledged))
        assertArrayEquals(originalBytes, store.recordFile.readBytes())

        // A same-length replacement still changes the bounded content digest and invalidates CAS.
        store.recordFile.writeBytes(originalBytes.copyOf().also { bytes ->
            bytes[0] = (bytes[0].toInt() xor 0x01).toByte()
        })
        val replacement = store.load() as OnboardingCheckpointLoadResult.Corrupt
        assertNotEquals(corrupt.artifactIdentity, replacement.artifactIdentity)
        assertFalse(store.resetCorruptExact(corrupt.artifactIdentity))
        assertTrue(store.resetCorruptExact(replacement.artifactIdentity))
        assertEquals(
            OnboardingCheckpointLoadResult.Loaded(OnboardingCheckpoint.fresh()),
            store.load(),
        )
        assertTrue(store.recordFile.exists())
    }

    @Test
    fun writeFailureLeavesExistingArtifactAndCallerCanRemainAtOldState() {
        val regularFileInsteadOfDirectory = File(noBackupDirectory, "not-a-directory")
        regularFileInsteadOfDirectory.writeText("occupied")
        val unavailableStore = AtomicFileOnboardingCheckpointStore(regularFileInsteadOfDirectory)
        val fresh = OnboardingCheckpoint.fresh()

        assertEquals(OnboardingCheckpointLoadResult.Loaded(fresh), unavailableStore.load())
        assertFalse(
            unavailableStore.saveIfCurrent(
                fresh,
                fresh.copy(privacyAcknowledged = true),
            ),
        )
        assertEquals("occupied", regularFileInsteadOfDirectory.readText())
    }

    private fun removeArtifacts() {
        AtomicFileOnboardingCheckpointStore.RECORD_ARTIFACT_SUFFIXES.forEach { suffix ->
            File(store.recordFile.path + suffix).delete()
        }
    }

    private fun writeRawRecord(
        output: DataOutputStream,
        magic: Int = AtomicFileOnboardingCheckpointStore.RECORD_MAGIC,
        version: Int = OnboardingCheckpoint.CURRENT_VERSION,
        currentStep: Int = OnboardingStep.WELCOME.wireValue,
        furthestStep: Int = OnboardingStep.WELCOME.wireValue,
        privacy: Int = 0,
        completed: Int = 0,
        trailingByte: Int? = null,
    ) {
        output.writeInt(magic)
        output.writeInt(version)
        output.writeByte(currentStep)
        output.writeByte(furthestStep)
        output.writeByte(privacy)
        output.writeByte(completed)
        trailingByte?.let(output::writeByte)
    }
}
