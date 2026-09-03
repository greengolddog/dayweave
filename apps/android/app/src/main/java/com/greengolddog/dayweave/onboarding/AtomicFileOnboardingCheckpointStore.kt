package com.greengolddog.dayweave.onboarding

import android.content.Context
import android.util.AtomicFile
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.security.MessageDigest

/**
 * Strict onboarding checkpoint stored beneath Android's no-backup directory.
 *
 * The fixed binary payload has exactly five semantic fields: version, current step, furthest step,
 * privacy acknowledgement, and completion. Unlike an open JSON/object decoder, unknown or
 * duplicate keys cannot be represented. The exact length check rejects every expanded/trailing
 * shape. AtomicFile provides rollback for a torn write, while read-back verification prevents a
 * transition from becoming visible to its controller before it is durable and decodable.
 */
class AtomicFileOnboardingCheckpointStore(
    noBackupDirectory: File,
) : OnboardingCheckpointStore {
    constructor(context: Context) : this(context.noBackupFilesDir)

    internal val recordFile = File(noBackupDirectory, RECORD_FILE_NAME)
    private val atomicFile = AtomicFile(recordFile)

    override fun load(): OnboardingCheckpointLoadResult = synchronized(STORE_LOCK) {
        readRecord()
    }

    override fun saveIfCurrent(
        expected: OnboardingCheckpoint,
        replacement: OnboardingCheckpoint,
    ): Boolean = synchronized(STORE_LOCK) {
        if (!replacement.isPermittedReplacementOf(expected)) return@synchronized false
        val current = readRecord() as? OnboardingCheckpointLoadResult.Loaded
            ?: return@synchronized false
        if (current.checkpoint != expected) return@synchronized false
        if (replacement == expected) return@synchronized true
        writeAndVerify(replacement)
    }

    override fun resetCorruptExact(expected: OnboardingCorruptArtifactIdentity): Boolean =
        synchronized(STORE_LOCK) {
            val current = readRecord() as? OnboardingCheckpointLoadResult.Corrupt
                ?: return@synchronized false
            if (current.artifactIdentity != expected) return@synchronized false

            // Reset is itself an atomic replacement. A crash or failed write retains the prior
            // corrupt base/backup instead of turning an established install into an absent record.
            writeAndVerify(OnboardingCheckpoint.fresh())
        }

    private fun readRecord(): OnboardingCheckpointLoadResult {
        if (!hasRecordArtifact()) {
            return OnboardingCheckpointLoadResult.Loaded(OnboardingCheckpoint.fresh())
        }
        if (
            RECORD_ARTIFACT_SUFFIXES.any { suffix ->
                File(recordFile.path + suffix).let { artifact ->
                    artifact.exists() && artifact.length() > MAX_RECORD_BYTES
                }
            }
        ) {
            return corruptResult()
        }

        val checkpoint = try {
            DataInputStream(BufferedInputStream(atomicFile.openRead())).use { input ->
                if (recordFile.length() != ENCODED_RECORD_BYTES) return@use null
                if (input.readInt() != RECORD_MAGIC) return@use null
                val version = input.readInt()
                val currentStep = OnboardingStep.fromWireValue(input.readUnsignedByte())
                    ?: return@use null
                val furthestStep = OnboardingStep.fromWireValue(input.readUnsignedByte())
                    ?: return@use null
                val privacyAcknowledged = input.readStrictBoolean() ?: return@use null
                val completed = input.readStrictBoolean() ?: return@use null
                if (input.read() != -1) return@use null
                runCatching {
                    OnboardingCheckpoint(
                        version = version,
                        currentStep = currentStep,
                        furthestStep = furthestStep,
                        privacyAcknowledged = privacyAcknowledged,
                        completed = completed,
                    )
                }.getOrNull()
            }
        } catch (_: Exception) {
            null
        }

        return checkpoint?.let(OnboardingCheckpointLoadResult::Loaded) ?: corruptResult()
    }

    private fun DataInputStream.readStrictBoolean(): Boolean? = when (readUnsignedByte()) {
        0 -> false
        1 -> true
        else -> null
    }

    private fun writeAndVerify(checkpoint: OnboardingCheckpoint): Boolean {
        var output: FileOutputStream? = null
        return try {
            if (recordFile.parentFile?.let { it.mkdirs() || it.isDirectory } != true) return false
            val started = atomicFile.startWrite()
            output = started
            val data = DataOutputStream(BufferedOutputStream(started))
            data.writeInt(RECORD_MAGIC)
            data.writeInt(checkpoint.version)
            data.writeByte(checkpoint.currentStep.wireValue)
            data.writeByte(checkpoint.furthestStep.wireValue)
            data.writeByte(if (checkpoint.privacyAcknowledged) 1 else 0)
            data.writeByte(if (checkpoint.completed) 1 else 0)
            data.flush()
            started.fd.sync()
            atomicFile.finishWrite(started)
            output = null
            readRecord() == OnboardingCheckpointLoadResult.Loaded(checkpoint)
        } catch (_: Exception) {
            runCatching { output?.let(atomicFile::failWrite) }
            false
        }
    }

    /**
     * Normal-sized corrupt artifacts are hashed completely. Oversized artifacts are represented
     * by their bounded prefix plus size and timestamp, keeping recovery identity work bounded even
     * if local storage was tampered with.
     */
    private fun corruptResult(): OnboardingCheckpointLoadResult.Corrupt {
        val digest = MessageDigest.getInstance("SHA-256")
        RECORD_ARTIFACT_SUFFIXES.forEach { suffix ->
            digest.update(suffix.toByteArray(Charsets.UTF_8))
            val artifact = File(recordFile.path + suffix)
            digest.update((if (artifact.exists()) 1 else 0).toByte())
            if (artifact.exists()) {
                digest.update(longBytes(artifact.length()))
                digest.update(longBytes(artifact.lastModified()))
                runCatching {
                    FileInputStream(artifact).use { input ->
                        val buffer = ByteArray(FINGERPRINT_BUFFER_BYTES)
                        var remaining = MAX_FINGERPRINT_BYTES
                        while (remaining > 0) {
                            val read = input.read(buffer, 0, minOf(buffer.size, remaining))
                            if (read < 0) break
                            digest.update(buffer, 0, read)
                            remaining -= read
                        }
                    }
                }.onFailure {
                    digest.update(FINGERPRINT_READ_FAILURE_MARKER)
                }
            }
        }
        val fingerprint = digest.digest().joinToString(separator = "") { byte ->
            "%02x".format(byte.toInt() and 0xff)
        }
        return OnboardingCheckpointLoadResult.Corrupt(
            OnboardingCorruptArtifactIdentity(fingerprint),
        )
    }

    private fun hasRecordArtifact(): Boolean = RECORD_ARTIFACT_SUFFIXES.any { suffix ->
        File(recordFile.path + suffix).exists()
    }

    private fun longBytes(value: Long): ByteArray =
        ByteBuffer.allocate(Long.SIZE_BYTES).putLong(value).array()

    internal companion object {
        const val RECORD_FILE_NAME = "dayweave_onboarding_checkpoint.bin"
        const val RECORD_MAGIC = 0x44574F4E
        const val MAX_RECORD_BYTES = 2_048L
        const val ENCODED_RECORD_BYTES = 12L
        val RECORD_ARTIFACT_SUFFIXES = listOf("", ".bak", ".new")

        private const val MAX_FINGERPRINT_BYTES = MAX_RECORD_BYTES.toInt() + 1
        private const val FINGERPRINT_BUFFER_BYTES = 512
        private val FINGERPRINT_READ_FAILURE_MARKER = byteArrayOf(0x7f)
        private val STORE_LOCK = Any()
    }
}
