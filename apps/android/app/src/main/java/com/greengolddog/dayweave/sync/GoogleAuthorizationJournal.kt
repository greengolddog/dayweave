package com.greengolddog.dayweave.sync

import android.content.Context
import android.util.AtomicFile
import com.greengolddog.dayweave.network.GoogleService
import com.greengolddog.dayweave.network.StartGoogleAuthorizationRequest
import com.greengolddog.dayweave.network.normalizedHttpsApiBaseUrl
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.UUID

/** The exact user-visible purpose of a durable Google authorization request. */
enum class GoogleAuthorizationAction {
    CONNECT_READ_ONLY,
    REAUTHORIZE_READ_ONLY,
    ENABLE_CALENDAR_PUBLISHING,
    ENABLE_TASKS_PUBLISHING,
}

/**
 * Persist-before-send identity for one Google OAuth start request.
 *
 * The journal deliberately excludes the bearer token and one-use provider URL. It retains the
 * exact server request and retry identity, however, so an ambiguous response or process death can
 * only replay the same service-specific request. No account inventory or provider identity beyond
 * the request's required target is retained locally.
 */
data class GoogleAuthorizationJournal(
    val schemaVersion: Int = CURRENT_SCHEMA_VERSION,
    val configurationId: String,
    val apiBaseUrl: String,
    val request: StartGoogleAuthorizationRequest,
    val idempotencyKey: String,
    val createdAtEpochMillis: Long,
    val expiresAtEpochMillis: Long,
    val browserOpenedAtEpochMillis: Long? = null,
) {
    init {
        require(schemaVersion == CURRENT_SCHEMA_VERSION)
        require(configurationId.isSafeGoogleAuthorizationIdentifier(MAX_CONFIGURATION_ID_CHARS))
        require(
            apiBaseUrl.length in 1..MAX_API_BASE_URL_CHARS &&
                normalizedHttpsApiBaseUrl(apiBaseUrl) == apiBaseUrl,
        )
        require(request.isSupportedAndroidGoogleAuthorizationRequest())
        // Login hints are not needed by the Android UI and would add account data to this record.
        require(request.loginHint == null)
        require(idempotencyKey.isSafeGoogleAuthorizationIdempotencyKey())
        require(createdAtEpochMillis >= 0)
        require(expiresAtEpochMillis > createdAtEpochMillis)
        require(expiresAtEpochMillis - createdAtEpochMillis <= MAXIMUM_LIFETIME_MILLIS)
        browserOpenedAtEpochMillis?.let {
            require(it in createdAtEpochMillis until expiresAtEpochMillis)
        }
    }

    val action: GoogleAuthorizationAction
        get() = when (request.services.singleOrNull()) {
            GoogleService.CALENDAR -> GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING
            GoogleService.TASKS -> GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING
            else -> if (request.accountId == null) {
                GoogleAuthorizationAction.CONNECT_READ_ONLY
            } else {
                GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY
            }
        }

    val browserOpened: Boolean get() = browserOpenedAtEpochMillis != null

    fun isValidAt(nowEpochMillis: Long): Boolean =
        nowEpochMillis >= 0 &&
            createdAtEpochMillis <= nowEpochMillis.saturatingGoogleAuthorizationAdd(
                MAX_FUTURE_CLOCK_SKEW_MILLIS,
            ) &&
            expiresAtEpochMillis > nowEpochMillis

    /**
     * The browser deadline alone is not safe retirement: the device may be ahead of the server,
     * and a callback claimed just before expiry may hold the server's bounded exchange lease.
     */
    fun isSafeToRetireAt(nowEpochMillis: Long): Boolean =
        nowEpochMillis >= 0 && expiresAtEpochMillis.saturatingGoogleAuthorizationAdd(
            SAFE_RETIREMENT_DELAY_MILLIS,
        ) <= nowEpochMillis

    fun recordingServerExpiry(expiresAtEpochMillis: Long): GoogleAuthorizationJournal = copy(
        expiresAtEpochMillis = expiresAtEpochMillis,
    )

    fun recordingBrowserOpened(openedAtEpochMillis: Long): GoogleAuthorizationJournal {
        require(browserOpenedAtEpochMillis == null || browserOpenedAtEpochMillis == openedAtEpochMillis)
        return copy(browserOpenedAtEpochMillis = openedAtEpochMillis)
    }

    /** Request identities and account/binding metadata stay out of diagnostics. */
    override fun toString(): String =
        "GoogleAuthorizationJournal(binding=<redacted>, request=<redacted>, " +
            "action=$action, browserOpened=$browserOpened)"

    internal companion object {
        const val CURRENT_SCHEMA_VERSION = 1
        const val MAX_CONFIGURATION_ID_CHARS = 256
        const val MAX_API_BASE_URL_CHARS = 2_048
        const val MAX_FUTURE_CLOCK_SKEW_MILLIS = 5 * 60 * 1_000L
        const val MAX_SERVER_EXCHANGE_SETTLEMENT_MILLIS = 2 * 60 * 1_000L
        const val SAFE_RETIREMENT_DELAY_MILLIS =
            MAX_FUTURE_CLOCK_SKEW_MILLIS + MAX_SERVER_EXCHANGE_SETTLEMENT_MILLIS
        const val MAXIMUM_LIFETIME_MILLIS = 30 * 60 * 1_000L
    }
}

sealed interface GoogleAuthorizationJournalLoadResult {
    data object Empty : GoogleAuthorizationJournalLoadResult
    data class Loaded(val journal: GoogleAuthorizationJournal) :
        GoogleAuthorizationJournalLoadResult

    data class Expired(val journal: GoogleAuthorizationJournal) :
        GoogleAuthorizationJournalLoadResult

    data class Retirable(val journal: GoogleAuthorizationJournal) :
        GoogleAuthorizationJournalLoadResult

    data class Corrupt(
        internal val artifactIdentity: GoogleAuthorizationCorruptArtifactIdentity,
    ) : GoogleAuthorizationJournalLoadResult {
        override fun toString(): String = "Corrupt(<redacted>)"
    }
}

/** Content-free identity used to bind destructive confirmation to one unreadable artifact. */
class GoogleAuthorizationCorruptArtifactIdentity internal constructor(
    private val fingerprint: String,
) {
    override fun equals(other: Any?): Boolean =
        other is GoogleAuthorizationCorruptArtifactIdentity && fingerprint == other.fingerprint

    override fun hashCode(): Int = fingerprint.hashCode()

    override fun toString(): String = "GoogleAuthorizationCorruptArtifactIdentity(<redacted>)"
}

interface GoogleAuthorizationJournalStore {
    fun load(nowEpochMillis: Long): GoogleAuthorizationJournalLoadResult

    /** Creates [journal] only when there is no unresolved request. */
    fun saveIfAbsent(journal: GoogleAuthorizationJournal, nowEpochMillis: Long): Boolean

    /** Advances only [expected]; stale callbacks cannot replace a newer authorization. */
    fun updateExact(
        expected: GoogleAuthorizationJournal,
        replacement: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean

    /** Removes only the exact completed or expired request. */
    fun removeExact(expected: GoogleAuthorizationJournal, nowEpochMillis: Long): Boolean

    /** Explicit destructive recovery for an unreadable local record. */
    fun clearForConfirmedReset(nowEpochMillis: Long): Boolean

    /** Removes only the exact unreadable artifact for which the owner confirmed destruction. */
    fun clearCorruptExact(
        expected: GoogleAuthorizationCorruptArtifactIdentity,
        nowEpochMillis: Long,
    ): Boolean
}

/**
 * Fail-safe default used until the application supplies its no-backup store. Reads remain usable,
 * but authorization cannot leave the device because persist-before-send always fails.
 */
object UnavailableGoogleAuthorizationJournalStore : GoogleAuthorizationJournalStore {
    override fun load(nowEpochMillis: Long) = GoogleAuthorizationJournalLoadResult.Empty

    override fun saveIfAbsent(
        journal: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean = false

    override fun updateExact(
        expected: GoogleAuthorizationJournal,
        replacement: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean = false

    override fun removeExact(
        expected: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean = false

    override fun clearForConfirmedReset(nowEpochMillis: Long): Boolean = false

    override fun clearCorruptExact(
        expected: GoogleAuthorizationCorruptArtifactIdentity,
        nowEpochMillis: Long,
    ): Boolean = false
}

/**
 * Strict one-record ledger kept outside Android backup and device transfer.
 *
 * [AtomicFile] rolls a torn write back to the last complete request. Successful writes are flushed,
 * fsynced, finished, and read back before the manager is allowed to send or claim completion.
 */
class AtomicGoogleAuthorizationJournalStore(context: Context) :
    GoogleAuthorizationJournalStore {
    internal val recordFile = File(context.noBackupFilesDir, RECORD_FILE_NAME)
    private val atomicFile = AtomicFile(recordFile)

    override fun load(nowEpochMillis: Long): GoogleAuthorizationJournalLoadResult =
        synchronized(STORE_LOCK) { readRecord(nowEpochMillis) }

    override fun saveIfAbsent(
        journal: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        if (!journal.isValidAt(nowEpochMillis)) return@synchronized false
        when (val current = readRecord(nowEpochMillis)) {
            GoogleAuthorizationJournalLoadResult.Empty -> writeAndVerify(journal, nowEpochMillis)
            is GoogleAuthorizationJournalLoadResult.Loaded -> current.journal == journal
            is GoogleAuthorizationJournalLoadResult.Corrupt,
            is GoogleAuthorizationJournalLoadResult.Expired,
            is GoogleAuthorizationJournalLoadResult.Retirable,
            -> false
        }
    }

    override fun updateExact(
        expected: GoogleAuthorizationJournal,
        replacement: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        if (!replacement.isValidAt(nowEpochMillis) || !isPermittedTransition(expected, replacement)) {
            return@synchronized false
        }
        val current = readRecord(nowEpochMillis) as? GoogleAuthorizationJournalLoadResult.Loaded
            ?: return@synchronized false
        if (current.journal != expected) return@synchronized false
        writeAndVerify(replacement, nowEpochMillis)
    }

    override fun removeExact(
        expected: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        val current = when (val loaded = readRecord(nowEpochMillis)) {
            is GoogleAuthorizationJournalLoadResult.Loaded -> loaded.journal
            is GoogleAuthorizationJournalLoadResult.Expired -> loaded.journal
            is GoogleAuthorizationJournalLoadResult.Retirable -> loaded.journal
            GoogleAuthorizationJournalLoadResult.Empty,
            is GoogleAuthorizationJournalLoadResult.Corrupt,
            -> return@synchronized false
        }
        current == expected && writeAndVerify(null, nowEpochMillis)
    }

    override fun clearForConfirmedReset(nowEpochMillis: Long): Boolean =
        synchronized(STORE_LOCK) {
            nowEpochMillis >= 0 && writeAndVerify(null, nowEpochMillis)
        }

    override fun clearCorruptExact(
        expected: GoogleAuthorizationCorruptArtifactIdentity,
        nowEpochMillis: Long,
    ): Boolean = synchronized(STORE_LOCK) {
        val current = readRecord(nowEpochMillis) as? GoogleAuthorizationJournalLoadResult.Corrupt
            ?: return@synchronized false
        current.artifactIdentity == expected && writeAndVerify(null, nowEpochMillis)
    }

    private fun readRecord(nowEpochMillis: Long): GoogleAuthorizationJournalLoadResult {
        if (nowEpochMillis < 0) return corruptResult()
        if (!hasRecordArtifact()) return GoogleAuthorizationJournalLoadResult.Empty
        if (RECORD_ARTIFACT_SUFFIXES.any { suffix ->
                File(recordFile.path + suffix).let { it.exists() && it.length() > MAX_FILE_BYTES }
            }
        ) {
            return corruptResult()
        }
        return try {
            val journal = DataInputStream(BufferedInputStream(atomicFile.openRead())).use { input ->
                if (input.readInt() != RECORD_MAGIC) return@use null
                if (input.readInt() != LEDGER_SCHEMA_VERSION) return@use null
                when (input.readUnsignedByte()) {
                    RECORD_EMPTY -> {
                        if (input.read() != -1) return@use null
                        return GoogleAuthorizationJournalLoadResult.Empty
                    }
                    RECORD_PRESENT -> Unit
                    else -> return@use null
                }
                val decoded = input.readJournal()
                if (input.read() != -1) return@use null
                decoded
            } ?: return corruptResult()
            if (journal.isValidAt(nowEpochMillis)) {
                GoogleAuthorizationJournalLoadResult.Loaded(journal)
            } else if (journal.isSafeToRetireAt(nowEpochMillis)) {
                GoogleAuthorizationJournalLoadResult.Retirable(journal)
            } else if (journal.expiresAtEpochMillis <= nowEpochMillis) {
                GoogleAuthorizationJournalLoadResult.Expired(journal)
            } else {
                corruptResult()
            }
        } catch (_: Exception) {
            corruptResult()
        }
    }

    /**
     * Fingerprints the complete normal-sized AtomicFile artifact set. Oversized corruption is
     * still bound to its size, timestamps, and bounded prefix so confirmation never acts on a
     * newly written normal record and cannot be replayed after an ordinary artifact change.
     */
    private fun corruptResult(): GoogleAuthorizationJournalLoadResult.Corrupt {
        val digest = MessageDigest.getInstance("SHA-256")
        RECORD_ARTIFACT_SUFFIXES.forEach { suffix ->
            digest.update(suffix.toByteArray(Charsets.UTF_8))
            val artifact = File(recordFile.path + suffix)
            digest.update((if (artifact.exists()) 1 else 0).toByte())
            if (artifact.exists()) {
                digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(artifact.length()).array())
                digest.update(
                    ByteBuffer.allocate(Long.SIZE_BYTES).putLong(artifact.lastModified()).array(),
                )
                FileInputStream(artifact).use { input ->
                    val buffer = ByteArray(CORRUPT_FINGERPRINT_BUFFER_BYTES)
                    var remaining = MAX_CORRUPT_FINGERPRINT_BYTES
                    while (remaining > 0) {
                        val read = input.read(buffer, 0, minOf(buffer.size.toLong(), remaining).toInt())
                        if (read < 0) break
                        digest.update(buffer, 0, read)
                        remaining -= read
                    }
                }
            }
        }
        val fingerprint = digest.digest().joinToString(separator = "") { byte ->
            "%02x".format(byte.toInt() and 0xff)
        }
        return GoogleAuthorizationJournalLoadResult.Corrupt(
            GoogleAuthorizationCorruptArtifactIdentity(fingerprint),
        )
    }

    private fun DataInputStream.readJournal(): GoogleAuthorizationJournal {
        val schemaVersion = readInt()
        val configurationId = readBoundedUtf(GoogleAuthorizationJournal.MAX_CONFIGURATION_ID_CHARS)
        val apiBaseUrl = readBoundedUtf(GoogleAuthorizationJournal.MAX_API_BASE_URL_CHARS)
        val serviceCount = readInt()
        require(serviceCount in 0..1)
        val services = buildList(serviceCount) {
            repeat(serviceCount) {
                add(
                    when (readUnsignedByte()) {
                        SERVICE_CALENDAR -> GoogleService.CALENDAR
                        SERVICE_TASKS -> GoogleService.TASKS
                        else -> throw IllegalArgumentException("Invalid Google service")
                    },
                )
            }
        }
        val forceConsent = readBoolean()
        val accountId = if (readBoolean()) UUID(readLong(), readLong()).toString() else null
        val connectNew = readBoolean()
        val makeDefault = readBoolean()
        val request = StartGoogleAuthorizationRequest(
            services = services,
            forceConsent = forceConsent,
            loginHint = null,
            accountId = accountId,
            connectNew = connectNew,
            makeDefault = makeDefault,
        )
        val idempotencyKey = readBoundedUtf(MAX_IDEMPOTENCY_KEY_CHARS)
        val createdAt = readLong()
        val expiresAt = readLong()
        val browserOpenedAt = when (readUnsignedByte()) {
            0 -> null
            1 -> readLong()
            else -> throw IllegalArgumentException("Invalid browser-open marker")
        }
        return GoogleAuthorizationJournal(
            schemaVersion = schemaVersion,
            configurationId = configurationId,
            apiBaseUrl = apiBaseUrl,
            request = request,
            idempotencyKey = idempotencyKey,
            createdAtEpochMillis = createdAt,
            expiresAtEpochMillis = expiresAt,
            browserOpenedAtEpochMillis = browserOpenedAt,
        )
    }

    private fun DataInputStream.readBoundedUtf(maxChars: Int): String =
        readUTF().also { require(it.length in 1..maxChars) }

    private fun writeAndVerify(
        journal: GoogleAuthorizationJournal?,
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
            data.writeByte(if (journal == null) RECORD_EMPTY else RECORD_PRESENT)
            journal?.let { data.writeJournal(it) }
            data.flush()
            started.fd.sync()
            atomicFile.finishWrite(started)
            output = null
            readRecord(nowEpochMillis) == if (journal == null) {
                GoogleAuthorizationJournalLoadResult.Empty
            } else {
                GoogleAuthorizationJournalLoadResult.Loaded(journal)
            }
        } catch (_: Exception) {
            runCatching { output?.let(atomicFile::failWrite) }
            false
        }
    }

    private fun DataOutputStream.writeJournal(journal: GoogleAuthorizationJournal) {
        writeInt(journal.schemaVersion)
        writeUTF(journal.configurationId)
        writeUTF(journal.apiBaseUrl)
        writeInt(journal.request.services.size)
        journal.request.services.forEach { service ->
            writeByte(
                when (service) {
                    GoogleService.CALENDAR -> SERVICE_CALENDAR
                    GoogleService.TASKS -> SERVICE_TASKS
                    GoogleService.CALENDAR_READ_ONLY,
                    GoogleService.TASKS_READ_ONLY,
                    -> error("Unsupported journal service")
                },
            )
        }
        writeBoolean(journal.request.forceConsent)
        val accountId = journal.request.accountId
        writeBoolean(accountId != null)
        accountId?.let(UUID::fromString)?.also {
            writeLong(it.mostSignificantBits)
            writeLong(it.leastSignificantBits)
        }
        writeBoolean(journal.request.connectNew)
        writeBoolean(journal.request.makeDefault)
        writeUTF(journal.idempotencyKey)
        writeLong(journal.createdAtEpochMillis)
        writeLong(journal.expiresAtEpochMillis)
        val openedAt = journal.browserOpenedAtEpochMillis
        writeByte(if (openedAt == null) 0 else 1)
        openedAt?.let(::writeLong)
    }

    private fun isPermittedTransition(
        expected: GoogleAuthorizationJournal,
        replacement: GoogleAuthorizationJournal,
    ): Boolean {
        if (
            expected.schemaVersion != replacement.schemaVersion ||
            expected.configurationId != replacement.configurationId ||
            expected.apiBaseUrl != replacement.apiBaseUrl ||
            expected.request != replacement.request ||
            expected.idempotencyKey != replacement.idempotencyKey ||
            expected.createdAtEpochMillis != replacement.createdAtEpochMillis
        ) {
            return false
        }
        return when {
            expected == replacement -> true
            expected.browserOpened -> replacement.browserOpenedAtEpochMillis ==
                expected.browserOpenedAtEpochMillis
            else -> true
        }
    }

    private fun hasRecordArtifact(): Boolean = RECORD_ARTIFACT_SUFFIXES.any { suffix ->
        File(recordFile.path + suffix).exists()
    }

    internal companion object {
        const val RECORD_FILE_NAME = "dayweave_google_authorization_journal.bin"
        const val RECORD_MAGIC = 0x44574741
        const val LEDGER_SCHEMA_VERSION = 1
        const val MAX_FILE_BYTES = 1_048_576L
        private const val MAX_CORRUPT_FINGERPRINT_BYTES = MAX_FILE_BYTES + 1
        private const val CORRUPT_FINGERPRINT_BUFFER_BYTES = 8 * 1_024
        val RECORD_ARTIFACT_SUFFIXES = listOf("", ".bak", ".new")
        private const val RECORD_EMPTY = 0
        private const val RECORD_PRESENT = 1
        private const val SERVICE_CALENDAR = 1
        private const val SERVICE_TASKS = 2
        private const val MAX_IDEMPOTENCY_KEY_CHARS = 128
        private val STORE_LOCK = Any()
    }
}

private fun StartGoogleAuthorizationRequest.isSupportedAndroidGoogleAuthorizationRequest(): Boolean {
    val servicesAreValid = services.isEmpty() ||
        services == listOf(GoogleService.CALENDAR) || services == listOf(GoogleService.TASKS)
    val publishingUpgradeIsValid = services.isEmpty() ||
        accountId != null && forceConsent && !connectNew
    return servicesAreValid && publishingUpgradeIsValid &&
        !(connectNew && accountId != null) &&
        (accountId?.isCanonicalNonzeroGoogleAuthorizationUuid() ?: true) &&
        (loginHint?.let { hint ->
            hint.isNotEmpty() && hint.toByteArray(Charsets.UTF_8).size <= 320 &&
                !hint.any(Char::isISOControl)
        } ?: true)
}

private fun String.isCanonicalNonzeroGoogleAuthorizationUuid(): Boolean = runCatching {
    val parsed = UUID.fromString(this)
    parsed.toString() == this && parsed != UUID(0, 0)
}.getOrDefault(false)

private fun String.isSafeGoogleAuthorizationIdentifier(maxChars: Int): Boolean =
    length in 1..maxChars && all { it.code in 33..126 }

private fun String.isSafeGoogleAuthorizationIdempotencyKey(): Boolean =
    length in 8..128 && all { character ->
        character in '0'..'9' || character in 'A'..'Z' || character in 'a'..'z' ||
            character == '.' || character == '_' || character == '-'
    }

private fun Long.saturatingGoogleAuthorizationAdd(increment: Long): Long =
    if (this > Long.MAX_VALUE - increment) Long.MAX_VALUE else this + increment
