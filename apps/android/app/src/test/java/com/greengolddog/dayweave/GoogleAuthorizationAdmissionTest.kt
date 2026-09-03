package com.greengolddog.dayweave

import com.greengolddog.dayweave.sync.GoogleAuthorizationAction
import com.greengolddog.dayweave.sync.GoogleCalendarImportJournal
import com.greengolddog.dayweave.sync.GoogleCalendarImportJournalLoadResult
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleAuthorizationAdmissionTest {
    @Test
    fun onlyExactCurrentBindingImportAccountCanUseReauthorizationException() {
        val exact = journal(accountId = ACCOUNT_A)
        val loaded = GoogleCalendarImportJournalLoadResult.Loaded(listOf(exact))

        assertFalse(
            blocksGoogleAuthorizationForImportRecovery(
                action = GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY,
                targetAccountId = ACCOUNT_A,
                bindingConfigurationId = CONFIGURATION_ID,
                bindingBaseUrl = API_BASE_URL,
                recovery = loaded,
            ),
        )
        assertTrue(
            blocksGoogleAuthorizationForImportRecovery(
                action = GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY,
                targetAccountId = ACCOUNT_B,
                bindingConfigurationId = CONFIGURATION_ID,
                bindingBaseUrl = API_BASE_URL,
                recovery = loaded,
            ),
        )
        assertTrue(
            blocksGoogleAuthorizationForImportRecovery(
                action = GoogleAuthorizationAction.CONNECT_READ_ONLY,
                targetAccountId = null,
                bindingConfigurationId = CONFIGURATION_ID,
                bindingBaseUrl = API_BASE_URL,
                recovery = loaded,
            ),
        )
    }

    @Test
    fun foreignOrMixedBindingAndCorruptRecoveryAlwaysBlock() {
        val mixed = GoogleCalendarImportJournalLoadResult.Loaded(
            listOf(
                journal(accountId = ACCOUNT_A),
                journal(accountId = ACCOUNT_B, configurationId = FOREIGN_CONFIGURATION_ID),
            ),
        )

        assertTrue(
            blocksGoogleAuthorizationForImportRecovery(
                action = GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY,
                targetAccountId = ACCOUNT_A,
                bindingConfigurationId = CONFIGURATION_ID,
                bindingBaseUrl = API_BASE_URL,
                recovery = mixed,
            ),
        )
        assertTrue(
            blocksGoogleAuthorizationForImportRecovery(
                action = GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY,
                targetAccountId = ACCOUNT_A,
                bindingConfigurationId = CONFIGURATION_ID,
                bindingBaseUrl = API_BASE_URL,
                recovery = GoogleCalendarImportJournalLoadResult.Corrupt,
            ),
        )
    }

    @Test
    fun sameBindingRecoveryForAnotherAccountBlocksTheNarrowReauthorizationException() {
        val twoAccounts = GoogleCalendarImportJournalLoadResult.Loaded(
            listOf(
                journal(accountId = ACCOUNT_A),
                journal(accountId = ACCOUNT_B),
            ),
        )

        assertTrue(
            blocksGoogleAuthorizationForImportRecovery(
                action = GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY,
                targetAccountId = ACCOUNT_A,
                bindingConfigurationId = CONFIGURATION_ID,
                bindingBaseUrl = API_BASE_URL,
                recovery = twoAccounts,
            ),
        )
    }

    @Test
    fun emptyImportLedgerDoesNotBlockAnyGoogleAuthorizationAction() {
        val empty = GoogleCalendarImportJournalLoadResult.Loaded(emptyList())

        GoogleAuthorizationAction.entries.forEach { action ->
            assertFalse(
                blocksGoogleAuthorizationForImportRecovery(
                    action = action,
                    targetAccountId = if (
                        action == GoogleAuthorizationAction.CONNECT_READ_ONLY
                    ) {
                        null
                    } else {
                        ACCOUNT_A
                    },
                    bindingConfigurationId = CONFIGURATION_ID,
                    bindingBaseUrl = API_BASE_URL,
                    recovery = empty,
                ),
            )
        }
    }

    private fun journal(
        accountId: String,
        configurationId: String = CONFIGURATION_ID,
    ) = GoogleCalendarImportJournal(
        configurationId = configurationId,
        apiBaseUrl = API_BASE_URL,
        accountId = accountId,
        requestId = REQUEST_ID,
        createdAtEpochMillis = 1_000,
    )

    private companion object {
        const val API_BASE_URL = "https://planner.example.test/"
        const val CONFIGURATION_ID = "configuration-current"
        const val FOREIGN_CONFIGURATION_ID = "configuration-foreign"
        const val ACCOUNT_A = "11111111-1111-4111-8111-111111111111"
        const val ACCOUNT_B = "22222222-2222-4222-8222-222222222222"
        const val REQUEST_ID = "33333333-3333-4333-8333-333333333333"
    }
}
