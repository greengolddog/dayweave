package com.greengolddog.dayweave.security

import android.content.Context
import java.io.DataOutputStream
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AppLockSettingsStoreTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val preferences = context.getSharedPreferences(
        AtomicFileAppLockSettingsStore.LEGACY_PREFERENCES_NAME,
        Context.MODE_PRIVATE,
    )
    private val store = AtomicFileAppLockSettingsStore(context)

    @Before
    fun setUp() = removeAllRecords()

    @After
    fun tearDown() = removeAllRecords()

    @Test
    fun absentAtomicAndLegacyRecordsAreAHealthyFirstInstallDefault() {
        assertEquals(
            AppLockSettingsLoadResult.Loaded(AppLockSettings()),
            store.load(),
        )
        assertEquals(context.noBackupFilesDir, store.recordFile.parentFile)
    }

    @Test
    fun completeAtomicSettingsRoundTrip() {
        val settings = AppLockSettings(
            enabled = true,
            timeout = AppLockTimeout.FIFTEEN_MINUTES,
        )

        assertTrue(store.save(settings))

        assertEquals(AppLockSettingsLoadResult.Loaded(settings), store.load())
    }

    @Test
    fun zeroLengthAtomicRecordFailsClosed() {
        store.recordFile.parentFile?.mkdirs()
        store.recordFile.createNewFile()

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
    }

    @Test
    fun truncatedAtomicRecordFailsClosed() {
        writeRawRecord { output ->
            output.writeInt(AtomicFileAppLockSettingsStore.RECORD_MAGIC)
            output.writeInt(AtomicFileAppLockSettingsStore.RECORD_SCHEMA_VERSION)
        }

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
    }

    @Test
    fun malformedMagicFailsClosed() {
        writeRecord(magic = 0x01020304)

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
    }

    @Test
    fun unknownSchemaFailsClosed() {
        writeRecord(schema = AtomicFileAppLockSettingsStore.RECORD_SCHEMA_VERSION + 1)

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
    }

    @Test
    fun trailingBytesFailClosed() {
        writeRecord(trailingByte = 1)

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
    }

    @Test
    fun legacyEnabledOnlyFailsClosed() {
        preferences.edit()
            .putBoolean(AtomicFileAppLockSettingsStore.LEGACY_KEY_ENABLED, true)
            .commit()

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
        assertFalse(store.recordFile.exists())
    }

    @Test
    fun legacyTimeoutOnlyFailsClosed() {
        preferences.edit()
            .putString(
                AtomicFileAppLockSettingsStore.LEGACY_KEY_TIMEOUT,
                AppLockTimeout.FIVE_MINUTES.persistedValue,
            )
            .commit()

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
        assertFalse(store.recordFile.exists())
    }

    @Test
    fun legacyEnabledAndTimeoutWithoutSchemaFailClosed() {
        preferences.edit()
            .putBoolean(AtomicFileAppLockSettingsStore.LEGACY_KEY_ENABLED, true)
            .putString(
                AtomicFileAppLockSettingsStore.LEGACY_KEY_TIMEOUT,
                AppLockTimeout.FIVE_MINUTES.persistedValue,
            )
            .commit()

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
        assertFalse(store.recordFile.exists())
    }

    @Test
    fun wrongTypedLegacyRecordFailsClosed() {
        preferences.edit()
            .putInt(
                AtomicFileAppLockSettingsStore.LEGACY_KEY_SCHEMA_VERSION,
                AtomicFileAppLockSettingsStore.LEGACY_SCHEMA_VERSION,
            )
            .putString(AtomicFileAppLockSettingsStore.LEGACY_KEY_ENABLED, "not-a-boolean")
            .putString(
                AtomicFileAppLockSettingsStore.LEGACY_KEY_TIMEOUT,
                AppLockTimeout.ONE_MINUTE.persistedValue,
            )
            .commit()

        assertEquals(AppLockSettingsLoadResult.Corrupt, store.load())
        assertFalse(store.recordFile.exists())
    }

    @Test
    fun completeValidLegacyRecordMigratesBeforeRemovingLegacySource() {
        val settings = AppLockSettings(enabled = true, timeout = AppLockTimeout.THIRTY_MINUTES)
        putCompleteLegacy(settings)

        assertEquals(AppLockSettingsLoadResult.Loaded(settings), store.load())
        assertTrue(store.recordFile.exists())
        assertTrue(preferences.all.isEmpty())
        assertEquals(
            AppLockSettingsLoadResult.Loaded(settings),
            AtomicFileAppLockSettingsStore(context).load(),
        )
    }

    @Test
    fun authenticatedControllerRecoveryDurablyRepairsMalformedRecord() {
        store.recordFile.parentFile?.mkdirs()
        store.recordFile.writeBytes(byteArrayOf(0x44, 0x57))
        val controller = AppLockController(store, MonotonicClock { 0L })
        controller.onForegrounded()
        controller.updateAvailability(AppUnlockAvailability.AVAILABLE)

        val request = requireNotNull(
            controller.beginAuthentication(AppLockAuthenticationPurpose.UNLOCK),
        )
        assertTrue(
            controller.completeAuthentication(request, AppLockAuthenticationOutcome.SUCCESS),
        )

        val repaired = AppLockSettings(enabled = true)
        assertEquals(
            AppLockSettingsLoadResult.Loaded(repaired),
            AtomicFileAppLockSettingsStore(context).load(),
        )
        assertTrue(controller.state.value.settingsHealthy)
        assertFalse(controller.state.value.isLocked)
    }

    private fun putCompleteLegacy(settings: AppLockSettings) {
        preferences.edit()
            .putInt(
                AtomicFileAppLockSettingsStore.LEGACY_KEY_SCHEMA_VERSION,
                AtomicFileAppLockSettingsStore.LEGACY_SCHEMA_VERSION,
            )
            .putBoolean(AtomicFileAppLockSettingsStore.LEGACY_KEY_ENABLED, settings.enabled)
            .putString(
                AtomicFileAppLockSettingsStore.LEGACY_KEY_TIMEOUT,
                settings.timeout.persistedValue,
            )
            .commit()
    }

    private fun writeRecord(
        magic: Int = AtomicFileAppLockSettingsStore.RECORD_MAGIC,
        schema: Int = AtomicFileAppLockSettingsStore.RECORD_SCHEMA_VERSION,
        trailingByte: Int? = null,
    ) = writeRawRecord { output ->
        output.writeInt(magic)
        output.writeInt(schema)
        output.writeByte(1)
        output.writeUTF(AppLockTimeout.ONE_MINUTE.persistedValue)
        trailingByte?.let(output::writeByte)
    }

    private fun writeRawRecord(write: (DataOutputStream) -> Unit) {
        store.recordFile.parentFile?.mkdirs()
        DataOutputStream(store.recordFile.outputStream()).use(write)
    }

    private fun removeAllRecords() {
        preferences.edit().clear().commit()
        listOf(
            File(
                context.applicationInfo.dataDir,
                "shared_prefs/${AtomicFileAppLockSettingsStore.LEGACY_PREFERENCES_NAME}.xml",
            ),
            File(
                context.applicationInfo.dataDir,
                "shared_prefs/${AtomicFileAppLockSettingsStore.LEGACY_PREFERENCES_NAME}.xml.bak",
            ),
        ).forEach(File::delete)
        AtomicFileAppLockSettingsStore.RECORD_ARTIFACT_SUFFIXES.forEach { suffix ->
            File(store.recordFile.path + suffix).delete()
        }
    }
}
