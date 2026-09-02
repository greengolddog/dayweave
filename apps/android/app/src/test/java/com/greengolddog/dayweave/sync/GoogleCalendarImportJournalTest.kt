package com.greengolddog.dayweave.sync

import android.content.Context
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
class GoogleCalendarImportJournalTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val store = AtomicGoogleCalendarImportJournalStore(context)

    @After
    fun tearDown() {
        AtomicGoogleCalendarImportJournalStore.RECORD_ARTIFACT_SUFFIXES.forEach { suffix ->
            File(store.recordFile.path + suffix).delete()
        }
    }

    @Test
    fun preparedIdentityAndAcceptedGenerationAdvanceAtomically() {
        val prepared = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
        assertTrue(store.save(prepared, NOW))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(prepared)),
            store.load(NOW),
        )

        val accepted = prepared.recordingAcceptance(17, NOW + 1)
        assertTrue(store.save(accepted, NOW + 1))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(accepted)),
            store.load(NOW + 1),
        )
        assertFalse(store.removeExact(prepared, NOW + 1))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(accepted)),
            store.load(NOW + 1),
        )
        assertTrue(store.removeExact(accepted, NOW + 1))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(emptyList()),
            store.load(NOW + 1),
        )
    }

    @Test
    fun existingRequestCannotBeReplacedOrRegressed() {
        val prepared = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
        assertTrue(store.save(prepared, NOW))
        assertFalse(store.save(prepared.copy(requestId = REQUEST_B), NOW))
        val accepted = prepared.recordingAcceptance(3, NOW + 1)
        assertTrue(store.save(accepted, NOW + 1))
        assertFalse(store.save(prepared, NOW + 1))
        assertFalse(
            store.save(
                accepted.copy(
                    acceptedRefreshGeneration = 4,
                    acceptedRecordedAtEpochMillis = NOW + 2,
                ),
                NOW + 2,
            ),
        )
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(accepted)),
            store.load(NOW + 2),
        )
    }

    @Test
    fun definitiveRejectionRetiresOnlyTheExactPreparedIdentity() {
        val prepared = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
        assertTrue(store.save(prepared, NOW))

        assertFalse(
            store.retireRejectedPreparedExact(
                prepared.copy(requestId = REQUEST_B),
                NOW,
            ),
        )
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(prepared)),
            store.load(NOW),
        )

        assertTrue(store.retireRejectedPreparedExact(prepared, NOW))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(emptyList()),
            store.load(NOW),
        )
        assertFalse(store.retireRejectedPreparedExact(prepared, NOW))
    }

    @Test
    fun staleRejectionRetirementCannotDeleteAcceptedOrReplacementIdentity() {
        val prepared = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
        val accepted = prepared.recordingAcceptance(7, NOW + 1)
        val replacement = journal(
            accountId = ACCOUNT_A,
            requestId = REQUEST_B,
            createdAt = NOW + 2,
        )
        assertTrue(store.save(prepared, NOW))
        assertTrue(store.save(accepted, NOW + 1))

        assertFalse(store.retireRejectedPreparedExact(prepared, NOW + 1))
        assertFalse(store.retireRejectedPreparedExact(accepted, NOW + 1))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(accepted)),
            store.load(NOW + 1),
        )

        assertTrue(store.restartAcceptedExact(accepted, replacement, NOW + 2))
        assertFalse(store.retireRejectedPreparedExact(prepared, NOW + 2))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(replacement)),
            store.load(NOW + 2),
        )
        assertTrue(store.retireRejectedPreparedExact(replacement, NOW + 2))
    }

    @Test
    fun acceptedTerminalRunRestartsOnlyThroughExactAtomicReplacement() {
        val accepted = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
            .recordingAcceptance(9, NOW + 1)
        val replacement = journal(
            accountId = ACCOUNT_A,
            requestId = REQUEST_B,
            createdAt = NOW + 2,
        )
        assertTrue(store.save(accepted, NOW + 1))
        assertFalse(
            store.restartAcceptedExact(
                accepted.copy(requestId = REQUEST_B),
                replacement,
                NOW + 2,
            ),
        )
        assertTrue(store.restartAcceptedExact(accepted, replacement, NOW + 2))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(replacement)),
            store.load(NOW + 2),
        )
        assertFalse(store.removeExact(accepted, NOW + 2))
    }

    @Test
    fun tornAtomicWriteRollsBackToLastCompleteLedger() {
        val durable = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
        assertTrue(store.save(durable, NOW))
        val backup = File(store.recordFile.path + ".bak")
        Files.copy(
            store.recordFile.toPath(),
            backup.toPath(),
            StandardCopyOption.REPLACE_EXISTING,
        )
        store.recordFile.writeBytes(byteArrayOf(0x44, 0x57))

        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(durable)),
            store.load(NOW),
        )
        assertFalse(backup.exists())
    }

    @Test
    fun corruptLedgerFailsClosedUntilConfirmedDestructionAbandonsIt() {
        store.recordFile.parentFile?.mkdirs()
        store.recordFile.writeBytes(byteArrayOf(0x44, 0x57, 0x47, 0x49, 0x00))
        assertEquals(GoogleCalendarImportJournalLoadResult.Corrupt, store.load(NOW))

        assertTrue(store.abandonAllForConfirmedLocalDestruction(NOW))
        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(emptyList()),
            store.load(NOW),
        )
    }

    @Test
    fun ledgerSupportsMultipleAccountsButRedactsEveryIdentity() {
        val later = journal(
            accountId = ACCOUNT_B,
            requestId = REQUEST_B,
            createdAt = NOW + 1,
        )
        val earlier = journal(accountId = ACCOUNT_A, requestId = REQUEST_A)
        assertTrue(store.save(later, NOW + 1))
        assertTrue(store.save(earlier, NOW + 1))

        assertEquals(
            GoogleCalendarImportJournalLoadResult.Loaded(listOf(earlier, later)),
            store.load(NOW + 1),
        )
        val diagnostic = earlier.toString()
        assertFalse(diagnostic.contains(CONFIGURATION_ID))
        assertFalse(diagnostic.contains(ACCOUNT_A))
        assertFalse(diagnostic.contains(REQUEST_A))
        assertTrue(diagnostic.contains("<redacted>"))
    }

    private fun journal(
        accountId: String,
        requestId: String,
        createdAt: Long = NOW,
    ): GoogleCalendarImportJournal = GoogleCalendarImportJournal(
        configurationId = CONFIGURATION_ID,
        apiBaseUrl = API_BASE_URL,
        accountId = accountId,
        requestId = requestId,
        createdAtEpochMillis = createdAt,
    )

    private companion object {
        const val NOW = 1_788_259_200_000L
        const val API_BASE_URL = "https://dayweave.example/gateway/"
        const val CONFIGURATION_ID = "11111111-1111-4111-8111-111111111111"
        const val ACCOUNT_A = "22222222-2222-4222-8222-222222222222"
        const val ACCOUNT_B = "33333333-3333-4333-8333-333333333333"
        const val REQUEST_A = "44444444-4444-4444-8444-444444444444"
        const val REQUEST_B = "55555555-5555-4555-8555-555555555555"
    }
}
