package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.network.GoogleCalendarInboundRole
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.sync.GoogleImportAccountState
import com.greengolddog.dayweave.sync.GoogleImportCollectionState
import com.greengolddog.dayweave.ui.screens.GoogleCalendarSourcesCard
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class GoogleCalendarSourcesCardUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun showsOnlyActiveCalendarAccountsAndCalendarCollections() {
        showCard(
            googleAccounts = listOf(
                account(id = ACTIVE_ACCOUNT, label = "Personal"),
                account(
                    id = PAUSED_ACCOUNT,
                    label = "Paused",
                    status = "paused",
                    syncEnabled = false,
                ),
                account(id = TASKS_ONLY_ACCOUNT, label = "Tasks only", hasCalendar = false),
            ),
            collections = listOf(
                collection(
                    id = WRITABLE_CALENDAR,
                    name = "Shared family",
                    syncRole = RemoteGoogleSyncRole.WRITABLE,
                ),
                collection(
                    id = TASK_LIST,
                    name = "Google Tasks",
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                ),
            ),
        )

        composeRule.onNodeWithTag("google_calendar_account_0").assertIsDisplayed()
        composeRule.onNodeWithText("Paused").assertDoesNotExist()
        composeRule.onNodeWithText("Tasks only").assertDoesNotExist()
        composeRule.onNodeWithTag("google_calendar_collection_0_0").assertIsDisplayed()
        composeRule.onNodeWithText("Google Tasks").assertDoesNotExist()
        composeRule.onNodeWithText("Writable · managed on another device").assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_role_0_0_off").assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only")
            .assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking")
            .assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_writable")
            .assertDoesNotExist()
    }

    @Test
    fun providerDeletedCalendarIsUnavailableAndCannotBeReconfigured() {
        showCard(
            collections = listOf(
                collection(
                    id = BLOCKING_CALENDAR,
                    name = "Former team calendar",
                    providerDeleted = true,
                ),
            ),
        )

        composeRule.onNodeWithText("Unavailable · removed from Google Calendar")
            .assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_role_0_0_off").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking").assertIsNotEnabled()
    }

    @Test
    fun unselectedWritableCalendarIsLabeledAndSelectedAsOff() {
        showCard(
            collections = listOf(
                collection(
                    id = WRITABLE_CALENDAR,
                    name = "Former writable calendar",
                    syncRole = RemoteGoogleSyncRole.WRITABLE,
                    selected = false,
                ),
            ),
        )

        composeRule.onNodeWithText("Off · not imported").assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_role_0_0_off").assertIsSelected()
        composeRule.onNodeWithText("Writable · managed on another device").assertDoesNotExist()
    }

    @Test
    fun routesExactAccountCollectionRevisionAndInboundRole() {
        val configured = AtomicReference<ConfigurationAction?>()
        val discoveredAccount = AtomicReference<String?>()
        val refreshedAccount = AtomicReference<String?>()
        showCard(
            collections = listOf(
                collection(
                    id = READ_ONLY_CALENDAR,
                    name = "Work",
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    revision = 37,
                ),
            ),
            onDiscover = discoveredAccount::set,
            onRefreshOrCheck = refreshedAccount::set,
            onConfigure = { accountId, collectionId, revision, role ->
                configured.set(ConfigurationAction(accountId, collectionId, revision, role))
            },
        )

        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only")
            .assertIsSelected()
            .performClick()
        composeRule.runOnIdle { assertNull(configured.get()) }

        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking")
            .performClick()
        composeRule.onNodeWithTag("google_calendar_discover_0").performClick()
        composeRule.onNodeWithTag("google_calendar_refresh_0").performClick()

        composeRule.runOnIdle {
            assertEquals(
                ConfigurationAction(
                    accountId = ACTIVE_ACCOUNT,
                    collectionId = READ_ONLY_CALENDAR,
                    revision = 37,
                    role = GoogleCalendarInboundRole.BLOCKING,
                ),
                configured.get(),
            )
            assertEquals(ACTIVE_ACCOUNT, discoveredAccount.get())
            assertEquals(ACTIVE_ACCOUNT, refreshedAccount.get())
        }
        composeRule.onNodeWithText("Refresh import").assertIsDisplayed()
    }

    @Test
    fun callerGateDisablesEveryMutationControl() {
        showCard(
            collections = listOf(collection(id = BLOCKING_CALENDAR, name = "Focus")),
            actionsEnabled = false,
        )

        composeRule.onNodeWithTag("google_calendar_discover_0")
            .assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_refresh_0")
            .assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_off")
            .assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only")
            .assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking")
            .assertIsNotEnabled()
    }

    @Test
    fun importBusyGateShowsProgressAndDisablesControls() {
        showCard(
            collections = listOf(collection(id = BLOCKING_CALENDAR, name = "Focus")),
            importBusy = true,
        )
        composeRule.onNodeWithTag("google_calendar_import_progress").assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_discover_0")
            .assertIsNotEnabled()
    }

    @Test
    fun savedRecoveryKeepsCheckEnabledButFencesSourceChanges() {
        showCard(
            collections = listOf(collection(id = BLOCKING_CALENDAR, name = "Focus")),
            importPhase = GoogleCalendarImportPhase.RECOVERY_REQUIRED,
            pendingRecoveryCount = 1,
        )

        composeRule.onNodeWithText("Check import").assertIsDisplayed()
        composeRule.onNodeWithText("1 saved import needs checking").assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_refresh_0").assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_off").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking").assertIsNotEnabled()
    }

    private fun showCard(
        googleAccounts: List<GoogleAccountSummary> = listOf(
            account(id = ACTIVE_ACCOUNT, label = "Personal"),
        ),
        collections: List<GoogleImportCollectionState> = emptyList(),
        importPhase: GoogleCalendarImportPhase = GoogleCalendarImportPhase.READY,
        pendingRecoveryCount: Int = 0,
        importBusy: Boolean = false,
        actionsEnabled: Boolean = true,
        onDiscover: (String) -> Unit = {},
        onRefreshOrCheck: (String) -> Unit = {},
        onConfigure: (String, String, Long, GoogleCalendarInboundRole) -> Unit =
            { _, _, _, _ -> },
    ) {
        composeRule.setContent {
            MaterialTheme {
                GoogleCalendarSourcesCard(
                    googleAccountState = GoogleAccountState(
                        phase = GoogleAccountPhase.CONNECTED,
                        accounts = googleAccounts,
                        message = "Google connected",
                        configurationId = CONFIGURATION_ID,
                    ),
                    importState = GoogleCalendarImportState(
                        phase = importPhase,
                        message = "Saved import status is safe to show",
                        isBusy = importBusy,
                        accounts = mapOf(
                            ACTIVE_ACCOUNT to GoogleImportAccountState(collections = collections),
                        ),
                        activeAccountId = ACTIVE_ACCOUNT,
                        pendingRecoveryCount = pendingRecoveryCount,
                        pendingRecoveryAccountIds = if (pendingRecoveryCount > 0) {
                            setOf(ACTIVE_ACCOUNT)
                        } else {
                            emptySet()
                        },
                        configurationId = CONFIGURATION_ID,
                    ),
                    onDiscover = onDiscover,
                    onRefreshOrCheck = onRefreshOrCheck,
                    onConfigure = onConfigure,
                    actionsEnabled = actionsEnabled,
                )
            }
        }
    }

    private data class ConfigurationAction(
        val accountId: String,
        val collectionId: String,
        val revision: Long,
        val role: GoogleCalendarInboundRole,
    )

    private companion object {
        const val CONFIGURATION_ID = "11111111-1111-4111-8111-111111111111"
        const val ACTIVE_ACCOUNT = "22222222-2222-4222-8222-222222222222"
        const val PAUSED_ACCOUNT = "33333333-3333-4333-8333-333333333333"
        const val TASKS_ONLY_ACCOUNT = "44444444-4444-4444-8444-444444444444"
        const val WRITABLE_CALENDAR = "55555555-5555-4555-8555-555555555555"
        const val TASK_LIST = "66666666-6666-4666-8666-666666666666"
        const val READ_ONLY_CALENDAR = "77777777-7777-4777-8777-777777777777"
        const val BLOCKING_CALENDAR = "88888888-8888-4888-8888-888888888888"

        fun account(
            id: String,
            label: String,
            status: String = "active",
            syncEnabled: Boolean = true,
            hasCalendar: Boolean = true,
        ) = GoogleAccountSummary(
            id = id,
            label = label,
            status = status,
            syncEnabled = syncEnabled,
            isDefault = id == ACTIVE_ACCOUNT,
            hasCalendar = hasCalendar,
            hasCalendarWriteScope = false,
            hasTasks = true,
            hasTasksWriteScope = false,
            revision = 1,
        )

        fun collection(
            id: String,
            name: String,
            kind: RemoteGoogleCollectionKind = RemoteGoogleCollectionKind.CALENDAR,
            syncRole: RemoteGoogleSyncRole = RemoteGoogleSyncRole.BLOCKING,
            revision: Long = 1,
            selected: Boolean = true,
            providerDeleted: Boolean = false,
        ) = GoogleImportCollectionState(
            id = id,
            accountId = ACTIVE_ACCOUNT,
            displayName = name,
            kind = kind,
            selected = selected,
            visible = true,
            syncRole = syncRole,
            calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
            revision = revision,
            lastImportAt = null,
            providerDeleted = providerDeleted,
        )
    }
}
