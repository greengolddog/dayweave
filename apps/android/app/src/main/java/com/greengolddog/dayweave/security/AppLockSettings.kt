package com.greengolddog.dayweave.security

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import android.util.AtomicFile
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.File
import java.io.FileOutputStream

/**
 * Non-secret presentation-lock settings.
 *
 * Planner content and credentials never enter this store. They remain in the SQLCipher database
 * and Android Keystore-backed credential stores respectively.
 */
data class AppLockSettings(
    val enabled: Boolean = false,
    val timeout: AppLockTimeout = AppLockTimeout.ONE_MINUTE,
)

enum class AppLockTimeout(
    val persistedValue: String,
    val durationMillis: Long,
    val label: String,
    val backgroundDescription: String,
) {
    IMMEDIATELY("immediately", 0L, "Immediately", "as soon as you leave the app"),
    ONE_MINUTE("one_minute", 60_000L, "After 1 minute", "after 1 minute away"),
    FIVE_MINUTES("five_minutes", 5 * 60_000L, "After 5 minutes", "after 5 minutes away"),
    FIFTEEN_MINUTES(
        "fifteen_minutes",
        15 * 60_000L,
        "After 15 minutes",
        "after 15 minutes away",
    ),
    THIRTY_MINUTES(
        "thirty_minutes",
        30 * 60_000L,
        "After 30 minutes",
        "after 30 minutes away",
    ),
    ;

    companion object {
        fun fromPersistedValue(value: String): AppLockTimeout? =
            entries.firstOrNull { it.persistedValue == value }
    }
}

sealed interface AppLockSettingsLoadResult {
    data class Loaded(val settings: AppLockSettings) : AppLockSettingsLoadResult
    data object Corrupt : AppLockSettingsLoadResult
}

interface AppLockSettingsStore {
    fun load(): AppLockSettingsLoadResult
    fun save(settings: AppLockSettings): Boolean
}

/**
 * Strict, atomic, non-backed-up app-lock record.
 *
 * A genuinely absent record is the only representation of a first-install opt-in default. Any
 * artifact left by an interrupted or malformed existing record fails closed. The fixed binary
 * envelope rejects truncation, trailing bytes, unknown schemas, and unknown timeout values.
 */
class AtomicFileAppLockSettingsStore(context: Context) : AppLockSettingsStore {
    internal val recordFile = File(context.noBackupFilesDir, RECORD_FILE_NAME)
    private val atomicFile = AtomicFile(recordFile)
    private val legacyRecordFiles = listOf(
        File(context.applicationInfo.dataDir, "shared_prefs/$LEGACY_PREFERENCES_NAME.xml"),
        File(context.applicationInfo.dataDir, "shared_prefs/$LEGACY_PREFERENCES_NAME.xml.bak"),
    )
    private val legacyPreferences: SharedPreferences = context.getSharedPreferences(
        LEGACY_PREFERENCES_NAME,
        Context.MODE_PRIVATE,
    )

    override fun load(): AppLockSettingsLoadResult = runCatching {
        if (hasAtomicRecordArtifact()) {
            return@runCatching readAtomicRecord()
        }
        migrateLegacyOrLoadFirstInstall()
    }.getOrDefault(AppLockSettingsLoadResult.Corrupt)

    override fun save(settings: AppLockSettings): Boolean = writeAtomicRecord(settings)

    private fun migrateLegacyOrLoadFirstInstall(): AppLockSettingsLoadResult {
        val legacyValues = legacyPreferences.all
        if (legacyValues.isEmpty()) {
            if (legacyRecordFiles.any(File::exists)) return AppLockSettingsLoadResult.Corrupt
            return AppLockSettingsLoadResult.Loaded(AppLockSettings())
        }

        val expectedKeys = setOf(LEGACY_KEY_SCHEMA_VERSION, LEGACY_KEY_ENABLED, LEGACY_KEY_TIMEOUT)
        if (legacyValues.keys != expectedKeys) return AppLockSettingsLoadResult.Corrupt
        val schema = legacyValues[LEGACY_KEY_SCHEMA_VERSION] as? Int
            ?: return AppLockSettingsLoadResult.Corrupt
        val enabled = legacyValues[LEGACY_KEY_ENABLED] as? Boolean
            ?: return AppLockSettingsLoadResult.Corrupt
        val timeoutValue = legacyValues[LEGACY_KEY_TIMEOUT] as? String
            ?: return AppLockSettingsLoadResult.Corrupt
        val timeout = AppLockTimeout.fromPersistedValue(timeoutValue)
            ?: return AppLockSettingsLoadResult.Corrupt
        if (schema != LEGACY_SCHEMA_VERSION) return AppLockSettingsLoadResult.Corrupt

        val settings = AppLockSettings(enabled = enabled, timeout = timeout)
        if (!writeAtomicRecord(settings)) return AppLockSettingsLoadResult.Corrupt
        // AtomicFile.finishWrite and fd.sync complete before the legacy values are cleared. If
        // cleanup fails, the authoritative atomic record remains intact and wins on every load.
        clearLegacyAfterMigration()
        return AppLockSettingsLoadResult.Loaded(settings)
    }

    private fun readAtomicRecord(): AppLockSettingsLoadResult = runCatching {
        DataInputStream(BufferedInputStream(atomicFile.openRead())).use { input ->
            if (input.readInt() != RECORD_MAGIC) return@runCatching AppLockSettingsLoadResult.Corrupt
            if (input.readInt() != RECORD_SCHEMA_VERSION) {
                return@runCatching AppLockSettingsLoadResult.Corrupt
            }
            val enabledValue = input.readUnsignedByte()
            if (enabledValue !in 0..1) return@runCatching AppLockSettingsLoadResult.Corrupt
            val timeout = AppLockTimeout.fromPersistedValue(input.readUTF())
                ?: return@runCatching AppLockSettingsLoadResult.Corrupt
            if (input.read() != -1) return@runCatching AppLockSettingsLoadResult.Corrupt
            AppLockSettingsLoadResult.Loaded(
                AppLockSettings(enabled = enabledValue == 1, timeout = timeout),
            )
        }
    }.getOrDefault(AppLockSettingsLoadResult.Corrupt)

    private fun writeAtomicRecord(settings: AppLockSettings): Boolean {
        var output: FileOutputStream? = null
        return try {
            val startedOutput = atomicFile.startWrite()
            output = startedOutput
            val data = DataOutputStream(BufferedOutputStream(startedOutput))
            data.writeInt(RECORD_MAGIC)
            data.writeInt(RECORD_SCHEMA_VERSION)
            data.writeByte(if (settings.enabled) 1 else 0)
            data.writeUTF(settings.timeout.persistedValue)
            data.flush()
            startedOutput.fd.sync()
            atomicFile.finishWrite(startedOutput)
            output = null
            true
        } catch (_: Exception) {
            runCatching { output?.let(atomicFile::failWrite) }
            false
        }
    }

    private fun hasAtomicRecordArtifact(): Boolean = RECORD_ARTIFACT_SUFFIXES.any { suffix ->
        File(recordFile.path + suffix).exists()
    }

    @SuppressLint("UseKtx")
    private fun clearLegacyAfterMigration() {
        runCatching { legacyPreferences.edit().clear().commit() }
    }

    internal companion object {
        const val RECORD_FILE_NAME = "dayweave_app_lock.bin"
        const val RECORD_MAGIC = 0x44574C4B
        const val RECORD_SCHEMA_VERSION = 1

        // AtomicFile implementations use the base name plus one or both of these recovery files.
        // A lone recovery artifact is still evidence that this is not a first install.
        val RECORD_ARTIFACT_SUFFIXES = listOf("", ".bak", ".new")

        const val LEGACY_PREFERENCES_NAME = "dayweave_app_lock"
        const val LEGACY_KEY_SCHEMA_VERSION = "schema_version"
        const val LEGACY_KEY_ENABLED = "enabled"
        const val LEGACY_KEY_TIMEOUT = "timeout"
        const val LEGACY_SCHEMA_VERSION = 1
    }
}
