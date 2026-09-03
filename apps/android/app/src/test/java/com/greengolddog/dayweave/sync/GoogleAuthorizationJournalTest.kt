package com.greengolddog.dayweave.sync

import android.content.Context
import com.greengolddog.dayweave.network.GoogleService
import com.greengolddog.dayweave.network.StartGoogleAuthorizationRequest
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class GoogleAuthorizationJournalTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val store = AtomicGoogleAuthorizationJournalStore(context)

    @After
    fun tearDown() {
        AtomicGoogleAuthorizationJournalStore.RECORD_ARTIFACT_SUFFIXES.forEach { suffix ->
            File(store.recordFile.path + suffix).delete()
        }
    }

    @Test
    fun exactRequestExpiryAndBrowserStageRoundTripAcrossStoreInstances() {
        val prepared = journal(GoogleService.CALENDAR)
        assertTrue(store.saveIfAbsent(prepared, NOW))
        assertEquals(
            GoogleAuthorizationJournalLoadResult.Loaded(prepared),
            AtomicGoogleAuthorizationJournalStore(context).load(NOW),
        )

        val responded = prepared.recordingServerExpiry(NOW + 10 * 60_000)
        assertTrue(store.updateExact(prepared, responded, NOW))
        val opened = responded.recordingBrowserOpened(NOW + 1)
        assertTrue(store.updateExact(responded, opened, NOW + 1))
        assertEquals(
            GoogleAuthorizationJournalLoadResult.Loaded(opened),
            AtomicGoogleAuthorizationJournalStore(context).load(NOW + 1),
        )
        assertEquals(GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING, opened.action)
        assertTrue(opened.browserOpened)

        val differentService = journal(GoogleService.TASKS)
        assertFalse(store.updateExact(opened, differentService, NOW + 1))
        assertFalse(store.removeExact(responded, NOW + 1))
        assertTrue(store.removeExact(opened, NOW + 1))
        assertEquals(
            GoogleAuthorizationJournalLoadResult.Empty,
            AtomicGoogleAuthorizationJournalStore(context).load(NOW + 1),
        )
    }

    @Test
    fun staleWriterCannotReplaceOrClearExactRequest() {
        val calendar = journal(GoogleService.CALENDAR)
        val tasks = journal(GoogleService.TASKS)
        assertTrue(store.saveIfAbsent(calendar, NOW))
        assertFalse(store.saveIfAbsent(tasks, NOW))
        assertFalse(store.updateExact(tasks, tasks.recordingServerExpiry(NOW + 1_000), NOW))
        assertFalse(store.removeExact(tasks, NOW))
        assertEquals(GoogleAuthorizationJournalLoadResult.Loaded(calendar), store.load(NOW))
    }

    @Test
    fun emptyServiceSentinelRoundTripsAsReadOnlyConnectionPurpose() {
        val readOnly = GoogleAuthorizationJournal(
            configurationId = CONFIGURATION_ID,
            apiBaseUrl = "https://api.example.test/",
            request = StartGoogleAuthorizationRequest(makeDefault = true),
            idempotencyKey = IDEMPOTENCY_KEY,
            createdAtEpochMillis = NOW,
            expiresAtEpochMillis = NOW + 30 * 60_000,
        )
        assertTrue(store.saveIfAbsent(readOnly, NOW))
        val loaded = (store.load(NOW) as GoogleAuthorizationJournalLoadResult.Loaded).journal
        assertTrue(loaded.request.services.isEmpty())
        assertEquals(GoogleAuthorizationAction.CONNECT_READ_ONLY, loaded.action)
    }

    @Test
    fun expiredRecordMustBeRemovedExactlyBeforeAReplacementCanBeSaved() {
        val expired = journal(GoogleService.CALENDAR).copy(
            createdAtEpochMillis = NOW - 20 * 60_000,
            expiresAtEpochMillis = NOW - 1,
        )
        assertTrue(store.saveIfAbsent(expired, NOW - 10 * 60_000))
        assertEquals(GoogleAuthorizationJournalLoadResult.Expired(expired), store.load(NOW))
        assertFalse(store.saveIfAbsent(journal(GoogleService.TASKS), NOW))
        assertTrue(store.removeExact(expired, NOW))
        assertTrue(store.saveIfAbsent(journal(GoogleService.TASKS), NOW))
    }

    @Test
    fun browserExpiryRetainsRecoveryThroughClockSkewAndExchangeSettlement() {
        val expiry = NOW + 60_000
        val pending = journal(GoogleService.CALENDAR).copy(expiresAtEpochMillis = expiry)
        assertTrue(store.saveIfAbsent(pending, NOW))

        assertEquals(
            GoogleAuthorizationJournalLoadResult.Expired(pending),
            store.load(expiry),
        )
        assertEquals(
            GoogleAuthorizationJournalLoadResult.Expired(pending),
            store.load(
                expiry + GoogleAuthorizationJournal.SAFE_RETIREMENT_DELAY_MILLIS - 1,
            ),
        )
        assertEquals(
            GoogleAuthorizationJournalLoadResult.Retirable(pending),
            store.load(expiry + GoogleAuthorizationJournal.SAFE_RETIREMENT_DELAY_MILLIS),
        )
    }

    @Test
    fun tornWriteRollsBackAndMalformedRecordFailsClosedUntilConfirmedReset() {
        val durable = journal(GoogleService.TASKS)
        assertTrue(store.saveIfAbsent(durable, NOW))
        val backup = File(store.recordFile.path + ".bak")
        Files.copy(
            store.recordFile.toPath(),
            backup.toPath(),
            StandardCopyOption.REPLACE_EXISTING,
        )
        store.recordFile.writeBytes(byteArrayOf(0x44, 0x57))
        assertEquals(GoogleAuthorizationJournalLoadResult.Loaded(durable), store.load(NOW))
        assertFalse(backup.exists())

        store.recordFile.writeBytes(byteArrayOf(0x44, 0x57, 0x47, 0x41, 0x00))
        val corrupt = store.load(NOW) as GoogleAuthorizationJournalLoadResult.Corrupt
        assertFalse(store.saveIfAbsent(journal(GoogleService.CALENDAR), NOW))
        assertTrue(store.clearCorruptExact(corrupt.artifactIdentity, NOW))
        assertEquals(GoogleAuthorizationJournalLoadResult.Empty, store.load(NOW))
    }

    @Test
    fun corruptConfirmationCannotClearAReplacementArtifact() {
        store.recordFile.parentFile?.mkdirs()
        store.recordFile.writeBytes(byteArrayOf(0x01, 0x02, 0x03))
        val first = store.load(NOW) as GoogleAuthorizationJournalLoadResult.Corrupt

        store.recordFile.writeBytes(byteArrayOf(0x04, 0x05, 0x06))

        assertFalse(store.clearCorruptExact(first.artifactIdentity, NOW))
        val replacement = store.load(NOW) as GoogleAuthorizationJournalLoadResult.Corrupt
        assertTrue(store.clearCorruptExact(replacement.artifactIdentity, NOW))
        assertEquals(GoogleAuthorizationJournalLoadResult.Empty, store.load(NOW))
    }

    @Test
    fun journalDiagnosticsRedactBindingAccountAndRetryIdentity() {
        val journal = journal(GoogleService.CALENDAR)
        val rendered = journal.toString()
        assertFalse(rendered.contains(CONFIGURATION_ID))
        assertFalse(rendered.contains(ACCOUNT_ID))
        assertFalse(rendered.contains(IDEMPOTENCY_KEY))
        assertTrue(rendered.contains("ENABLE_CALENDAR_PUBLISHING"))
    }

    private fun journal(service: GoogleService) = GoogleAuthorizationJournal(
        configurationId = CONFIGURATION_ID,
        apiBaseUrl = "https://api.example.test/",
        request = StartGoogleAuthorizationRequest(
            services = listOf(service),
            forceConsent = true,
            accountId = ACCOUNT_ID,
            makeDefault = true,
        ),
        idempotencyKey = IDEMPOTENCY_KEY,
        createdAtEpochMillis = NOW,
        expiresAtEpochMillis = NOW + 30 * 60_000,
    )

    private companion object {
        const val NOW = 1_788_246_000_000L
        const val CONFIGURATION_ID = "configuration-a"
        const val ACCOUNT_ID = "11111111-1111-4111-8111-111111111111"
        const val IDEMPOTENCY_KEY = "22222222-2222-4222-8222-222222222222"
    }
}
