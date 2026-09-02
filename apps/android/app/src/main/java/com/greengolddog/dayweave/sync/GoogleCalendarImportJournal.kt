package com.greengolddog.dayweave.sync

import android.content.Context
import android.util.AtomicFile
import com.greengolddog.dayweave.network.normalizedHttpsApiBaseUrl
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.File
import java.io.FileOutputStream
import java.util.UUID

/**
 * Persist-before-send identity for one Google import.
 *
 * The record deliberately contains no bearer token or provider payload. [configurationId] and
 * [apiBaseUrl] bind the request to the exact DayWeave credential generation that created it.
 */
data class GoogleCalendarImportJournal(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val configurationId: String,
    val apiBaseUrl: String,
    val accountId: String,
    val requestId: String,
    val createdAtEpochMillis: Long,
    val acceptedRefreshGeneration: Long? = null,
    val acceptedRecordedAtEpochMillis: Long? = null,
) {
    init {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        require(configurationId.isSafeOpaqueIdentifier(MAX_CONFIGURATION_ID_LENGTH))
        require(apiBaseUrl.isCanonicalHttpsApiBaseUrl())
        require(accountId.isCanonicalUuid())
        require(requestId.isCanonicalUuid())
        require(createdAtEpochMillis >= 0)
        require((acceptedRefreshGeneration == null) == (acceptedRecordedAtEpochMillis == null))
        acceptedRefreshGeneration?.let { require(it in 1 until Long.MAX_VALUE) }
        acceptedRecordedAtEpochMillis?.let {
            require(it >= createdAtEpochMillis)
        }
    }

    val isAccepted: Boolean get() = acceptedRefreshGeneration != null

    fun recordingAcceptance(
        refreshGeneration: Long,
        recordedAtEpochMillis: Long,
    ): GoogleCalendarImportJournal {
        require(refreshGeneration in 1 until Long.MAX_VALUE)
        require(recordedAtEpochMillis >= createdAtEpochMillis)
        if (isAccepted) {
            require(acceptedRefreshGeneration == refreshGeneration)
            require(acceptedRecordedAtEpochMillis == recordedAtEpochMillis)
            return this
        }
        return copy(
            acceptedRefreshGeneration = refreshGeneration,
            acceptedRecordedAtEpochMillis = recordedAtEpochMillis,
        )
    }

    fun isValidAt(nowEpochMillis: Long): Boolean =
        nowEpochMillis >= 0 &&
            createdAtEpochMillis <= nowEpochMillis.saturatingAdd(MAX_FUTURE_CLOCK_SKEW_MILLIS) &&
            (acceptedRecordedAtEpochMillis?.let {
                it <= nowEpochMillis.saturatingAdd(MAX_FUTURE_CLOCK_SKEW_MILLIS)
            } ?: true)

    /** Request identities are intentionally absent from diagnostics and crash reports. */
    override fun toString(): String =
        "GoogleCalendarImportJournal(binding=<redacted>, account=<redacted>, " +
            "request=<redacted>, accepted=$isAccepted)"

    internal companion object {
        const val CURRENT_SCHEMA_VERSION = 1
        const val MAX_CONFIGURATION_ID_LENGTH = 256
        const val MAX_API_BASE_URL_LENGTH = 2_048
        const val MAX_FUTURE_CLOCK_SKEW_MILLIS = 5 * 60 * 1_000L
    }
}

sealed interface GoogleCalendarImportJournalLoadResult {
    data class Loaded(val journals: List<GoogleCalendarImportJournal>) :
        GoogleCalendarImportJournalLoadResult

    data object Corrupt : GoogleCalendarImportJournalLoadResult
}

interface GoogleCalendarImportJournalStore {
    fun load(nowEpochMillis: Long): GoogleCalendarImportJournalLoadResult

    /** Inserts a prepared record or advances that exact request to accepted. */
    fun save(journal: GoogleCalendarImportJournal, nowEpochMillis: Long): Boolean

    /** Removes only [expected]; a stale completion can never clear a replacement request. */
    fun removeExact(
        expected: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean

    /**
     * Atomically retires an exact prepared identity after an explicit server rejection.
     * The caller must prove it durably created the identity in this invocation and that this was
     * its first dispatch; a prepared record loaded from disk may already have been accepted.
     * An accepted identity or a different replacement identity can never be removed by a stale
     * rejection callback.
     */
    fun retireRejectedPreparedExact(
        expected: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean

    /** Atomically replaces one accepted terminal run with a new persist-before-send identity. */
    fun restartAcceptedExact(
        expected: GoogleCalendarImportJournal,
        replacement: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean

    /** Explicit destructive path used only after the user confirms local credential removal. */
    fun abandonAllForConfirmedLocalDestruction(nowEpochMillis: Long): Boolean
}

/**
 * Strict crash-safe ledger stored outside Android backup and device transfer.
 *
 * AtomicFile provides rollback after a torn write. Every successful write is flushed, fsynced,
 * finished, and read back before returning. Malformed, truncated, oversized, duplicate, or
 * forward-version records fail closed.
 */
class AtomicGoogleCalendarImportJournalStore(context: Context) :
    GoogleCalendarImportJournalStore {
    internal val recordFile = File(context.noBackupFilesDir, RECORD_FILE_NAME)
    private val atomicFile = AtomicFile(recordFile)

    override fun load(nowEpochMillis: Long): GoogleCalendarImportJournalLoadResult =
        synchronized(STORE_LOCK) { readLedger(nowEpochMillis) }

    override fun save(
        journal: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        if (!journal.isValidAt(nowEpochMillis)) return@synchronized false
        val current = readLedger(nowEpochMillis) as? GoogleCalendarImportJournalLoadResult.Loaded
            ?: return@synchronized false
        val keyMatches: (GoogleCalendarImportJournal) -> Boolean = {
            it.configurationId == journal.configurationId && it.accountId == journal.accountId
        }
        val existingIndex = current.journals.indexOfFirst(keyMatches)
        val next = current.journals.toMutableList()
        if (existingIndex >= 0) {
            val existing = next[existingIndex]
            if (!isPermittedTransition(existing, journal)) return@synchronized false
            next[existingIndex] = journal
        } else {
            if (next.size >= MAX_ENTRIES) return@synchronized false
            next += journal
        }
        writeAndVerify(next.sortedWith(JOURNAL_ORDER), nowEpochMillis)
    }

    override fun removeExact(
        expected: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        if (!expected.isValidAt(nowEpochMillis)) return@synchronized false
        val current = readLedger(nowEpochMillis) as? GoogleCalendarImportJournalLoadResult.Loaded
            ?: return@synchronized false
        if (current.journals.none { it == expected }) return@synchronized false
        writeAndVerify(current.journals.filterNot { it == expected }, nowEpochMillis)
    }

    override fun retireRejectedPreparedExact(
        expected: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        if (expected.isAccepted || !expected.isValidAt(nowEpochMillis)) {
            return@synchronized false
        }
        val current = readLedger(nowEpochMillis) as? GoogleCalendarImportJournalLoadResult.Loaded
            ?: return@synchronized false
        if (current.journals.none { it == expected }) return@synchronized false
        writeAndVerify(current.journals.filterNot { it == expected }, nowEpochMillis)
    }

    override fun restartAcceptedExact(
        expected: GoogleCalendarImportJournal,
        replacement: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        if (
            !expected.isAccepted || replacement.isAccepted ||
            !expected.isValidAt(nowEpochMillis) || !replacement.isValidAt(nowEpochMillis) ||
            replacement.configurationId != expected.configurationId ||
            replacement.apiBaseUrl != expected.apiBaseUrl ||
            replacement.accountId != expected.accountId ||
            replacement.requestId == expected.requestId ||
            replacement.createdAtEpochMillis <
                requireNotNull(expected.acceptedRecordedAtEpochMillis)
        ) {
            return@synchronized false
        }
        val current = readLedger(nowEpochMillis) as? GoogleCalendarImportJournalLoadResult.Loaded
            ?: return@synchronized false
        val exactIndex = current.journals.indexOf(expected)
        if (exactIndex < 0) return@synchronized false
        val next = current.journals.toMutableList().also { it[exactIndex] = replacement }
        writeAndVerify(next.sortedWith(JOURNAL_ORDER), nowEpochMillis)
    }

    override fun abandonAllForConfirmedLocalDestruction(nowEpochMillis: Long): Boolean =
        synchronized(STORE_LOCK) {
            nowEpochMillis >= 0 && writeAndVerify(emptyList(), nowEpochMillis)
        }

    private fun readLedger(nowEpochMillis: Long): GoogleCalendarImportJournalLoadResult {
        if (nowEpochMillis < 0 || !hasRecordArtifact()) {
            return if (nowEpochMillis >= 0) {
                GoogleCalendarImportJournalLoadResult.Loaded(emptyList())
            } else {
                GoogleCalendarImportJournalLoadResult.Corrupt
            }
        }
        if (RECORD_ARTIFACT_SUFFIXES.any { suffix ->
                File(recordFile.path + suffix).let { it.exists() && it.length() > MAX_FILE_BYTES }
            }
        ) {
            return GoogleCalendarImportJournalLoadResult.Corrupt
        }
        return try {
            val journals = DataInputStream(BufferedInputStream(atomicFile.openRead())).use { input ->
                if (input.readInt() != RECORD_MAGIC) return@use null
                if (input.readInt() != LEDGER_SCHEMA_VERSION) return@use null
                val count = input.readInt()
                if (count !in 0..MAX_ENTRIES) return@use null
                val loaded = ArrayList<GoogleCalendarImportJournal>(count)
                repeat(count) { loaded += input.readJournal() }
                if (input.read() != -1) return@use null
                loaded
            } ?: return GoogleCalendarImportJournalLoadResult.Corrupt
            if (
                journals.any { !it.isValidAt(nowEpochMillis) } ||
                journals.toSet().size != journals.size ||
                journals.map { it.configurationId to it.accountId }.toSet().size != journals.size ||
                journals.sortedWith(JOURNAL_ORDER) != journals
            ) {
                GoogleCalendarImportJournalLoadResult.Corrupt
            } else {
                GoogleCalendarImportJournalLoadResult.Loaded(journals)
            }
        } catch (_: Exception) {
            GoogleCalendarImportJournalLoadResult.Corrupt
        }
    }

    private fun DataInputStream.readJournal(): GoogleCalendarImportJournal {
        val schemaVersion = readInt()
        val configurationId = readBoundedUtf(GoogleCalendarImportJournal.MAX_CONFIGURATION_ID_LENGTH)
        val apiBaseUrl = readBoundedUtf(GoogleCalendarImportJournal.MAX_API_BASE_URL_LENGTH)
        val accountId = UUID(readLong(), readLong()).toString()
        val requestId = UUID(readLong(), readLong()).toString()
        val createdAt = readLong()
        return when (val acceptance = readUnsignedByte()) {
            0 -> GoogleCalendarImportJournal(
                schemaVersion = schemaVersion,
                configurationId = configurationId,
                apiBaseUrl = apiBaseUrl,
                accountId = accountId,
                requestId = requestId,
                createdAtEpochMillis = createdAt,
            )

            1 -> GoogleCalendarImportJournal(
                schemaVersion = schemaVersion,
                configurationId = configurationId,
                apiBaseUrl = apiBaseUrl,
                accountId = accountId,
                requestId = requestId,
                createdAtEpochMillis = createdAt,
                acceptedRefreshGeneration = readLong(),
                acceptedRecordedAtEpochMillis = readLong(),
            )

            else -> throw IllegalArgumentException("Invalid Google import acceptance marker: $acceptance")
        }
    }

    private fun DataInputStream.readBoundedUtf(maxLength: Int): String =
        readUTF().also { require(it.length in 1..maxLength) }

    private fun writeAndVerify(
        journals: List<GoogleCalendarImportJournal>,
        nowEpochMillis: Long,
    ): Boolean {
        var output: FileOutputStream? = null
        return try {
            recordFile.parentFile?.mkdirs()
            val started = atomicFile.startWrite()
            output = started
            val data = DataOutputStream(BufferedOutputStream(started))
            data.writeInt(RECORD_MAGIC)
            data.writeInt(LEDGER_SCHEMA_VERSION)
            data.writeInt(journals.size)
            journals.forEach { data.writeJournal(it) }
            data.flush()
            started.fd.sync()
            // finishWrite owns and closes the still-open stream. Closing the DataOutputStream
            // first can make AtomicFile's final sync/close operate on an already-closed fd.
            atomicFile.finishWrite(started)
            output = null
            val readback = readLedger(nowEpochMillis)
            readback == GoogleCalendarImportJournalLoadResult.Loaded(journals)
        } catch (_: Exception) {
            runCatching { output?.let(atomicFile::failWrite) }
            false
        }
    }

    private fun DataOutputStream.writeJournal(journal: GoogleCalendarImportJournal) {
        writeInt(journal.schemaVersion)
        writeUTF(journal.configurationId)
        writeUTF(journal.apiBaseUrl)
        UUID.fromString(journal.accountId).also {
            writeLong(it.mostSignificantBits)
            writeLong(it.leastSignificantBits)
        }
        UUID.fromString(journal.requestId).also {
            writeLong(it.mostSignificantBits)
            writeLong(it.leastSignificantBits)
        }
        writeLong(journal.createdAtEpochMillis)
        val generation = journal.acceptedRefreshGeneration
        val acceptedAt = journal.acceptedRecordedAtEpochMillis
        if (generation == null || acceptedAt == null) {
            writeByte(0)
        } else {
            writeByte(1)
            writeLong(generation)
            writeLong(acceptedAt)
        }
    }

    private fun hasRecordArtifact(): Boolean = RECORD_ARTIFACT_SUFFIXES.any { suffix ->
        File(recordFile.path + suffix).exists()
    }

    private fun isPermittedTransition(
        existing: GoogleCalendarImportJournal,
        replacement: GoogleCalendarImportJournal,
    ): Boolean {
        if (
            existing.configurationId != replacement.configurationId ||
            existing.apiBaseUrl != replacement.apiBaseUrl ||
            existing.accountId != replacement.accountId ||
            existing.requestId != replacement.requestId ||
            existing.createdAtEpochMillis != replacement.createdAtEpochMillis
        ) {
            return false
        }
        return when {
            existing == replacement -> true
            existing.isAccepted -> false
            else -> replacement.isAccepted
        }
    }

    internal companion object {
        const val RECORD_FILE_NAME = "dayweave_google_import_journal.bin"
        const val RECORD_MAGIC = 0x44574749
        const val LEDGER_SCHEMA_VERSION = 1
        const val MAX_ENTRIES = 128
        const val MAX_FILE_BYTES = 1_048_576L
        val RECORD_ARTIFACT_SUFFIXES = listOf("", ".bak", ".new")
        private val STORE_LOCK = Any()
        private val JOURNAL_ORDER = compareBy<GoogleCalendarImportJournal>(
            GoogleCalendarImportJournal::createdAtEpochMillis,
            GoogleCalendarImportJournal::configurationId,
            GoogleCalendarImportJournal::accountId,
        )
    }
}

private fun String.isCanonicalUuid(): Boolean = runCatching {
    val parsed = UUID.fromString(this)
    parsed.toString() == this && parsed != UUID(0, 0)
}.getOrDefault(false)

private fun String.isSafeOpaqueIdentifier(maxLength: Int): Boolean =
    length in 1..maxLength && all { character -> character.code in 33..126 }

private fun String.isCanonicalHttpsApiBaseUrl(): Boolean =
    length in 1..GoogleCalendarImportJournal.MAX_API_BASE_URL_LENGTH &&
        runCatching { normalizedHttpsApiBaseUrl(this) == this }.getOrDefault(false)

private fun Long.saturatingAdd(increment: Long): Long =
    if (this > Long.MAX_VALUE - increment) Long.MAX_VALUE else this + increment
