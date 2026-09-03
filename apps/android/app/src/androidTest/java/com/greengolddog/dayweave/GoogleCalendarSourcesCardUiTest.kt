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
import com.greengolddog.dayweave.network.ConfigureGoogleCollectionRequest
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleEventDisposition
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.sync.GoogleImportAccountState
import com.greengolddog.dayweave.sync.GoogleImportCollectionState
import com.greengolddog.dayweave.ui.screens.GoogleSourcesCard
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class GoogleCalendarSourcesCardUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun showsFullCalendarAndTaskModesWhileGatingPublishByExactGrant() {
        showCard(
            googleAccounts = listOf(
                account(
                    id = ACTIVE_ACCOUNT,
                    label = "Personal",
                    hasCalendarWriteScope = true,
                    hasTasks = false,
                ),
                account(
                    id = PAUSED_ACCOUNT,
                    label = "Paused",
                    status = "paused",
                    syncEnabled = false,
                ),
                account(
                    id = TASKS_ONLY_ACCOUNT,
                    label = "Tasks only",
                    hasCalendar = false,
                    hasTasks = true,
                    hasTasksWriteScope = true,
                ),
            ),
            collections = listOf(
                collection(
                    id = WRITABLE_CALENDAR,
                    name = "Shared family",
                    syncRole = RemoteGoogleSyncRole.WRITABLE,
                    providerAccessRole = "owner",
                ),
                collection(
                    id = TASK_LIST,
                    name = "Google Tasks",
                    accountId = TASKS_ONLY_ACCOUNT,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                ),
            ),
        )

        composeRule.onNodeWithTag("google_calendar_account_0").assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_account_1").assertIsDisplayed()
        composeRule.onNodeWithText("Paused").assertDoesNotExist()
        composeRule.onNodeWithText("Tasks only").assertIsDisplayed()
        composeRule.onNodeWithTag("google_calendar_collection_0_0").assertIsDisplayed()
        composeRule.onNodeWithTag("google_task_collection_1_0").assertExists()
        composeRule.onNodeWithText("Google Tasks").assertExists()
        composeRule.onNodeWithText("Publish · writable Calendar destination").assertExists()
        composeRule.onNodeWithTag("google_calendar_role_0_0_off").assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only")
            .assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking")
            .assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_writable")
            .assertIsEnabled()
            .assertIsSelected()
        composeRule.onNodeWithTag("google_calendar_visible_0_0").assertIsEnabled()
        composeRule.onNodeWithTag("google_calendar_policy_0_0_confirmed_busy_blocking")
            .assertExists()
        composeRule.onNodeWithTag("google_calendar_publish_0_0_all_day").assertExists()
        composeRule.onNodeWithTag("google_task_role_1_0_off").assertIsEnabled()
        composeRule.onNodeWithTag("google_task_role_1_0_read_only").assertIsEnabled()
        composeRule.onNodeWithTag("google_task_role_1_0_writable").assertIsEnabled()
        composeRule.onNodeWithTag("google_task_role_1_0_blocking").assertDoesNotExist()
        composeRule.onNodeWithTag("google_task_policy_1_0_confirmed_busy_blocking")
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
        composeRule.onNodeWithTag("google_calendar_role_0_0_writable").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_visible_0_0").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_policy_0_0_all_day_reference")
            .assertIsNotEnabled()
    }

    @Test
    fun offModeDoesNotForceVisibilityOff() {
        val configured = AtomicReference<ConfigurationAction?>()
        showCard(
            collections = listOf(
                collection(
                    id = WRITABLE_CALENDAR,
                    name = "Optional calendar",
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    selected = true,
                    visible = true,
                ),
            ),
            onConfigure = { accountId, collectionId, request ->
                configured.set(ConfigurationAction(accountId, collectionId, request))
            },
        )

        composeRule.onNodeWithTag("google_calendar_role_0_0_off").performClick()

        composeRule.runOnIdle {
            val request = requireNotNull(configured.get()).request
            assertFalse(request.selected)
            assertTrue(request.visible)
            assertEquals(RemoteGoogleSyncRole.READ_ONLY, request.syncRole)
        }
    }

    @Test
    fun routesExactRequestAndKeepsVisibilityIndependentFromMode() {
        val configured = mutableListOf<ConfigurationAction>()
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
            onConfigure = { accountId, collectionId, request ->
                configured += ConfigurationAction(accountId, collectionId, request)
            },
        )

        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only")
            .assertIsSelected()
            .performClick()
        composeRule.runOnIdle { assertTrue(configured.isEmpty()) }

        composeRule.onNodeWithTag("google_calendar_role_0_0_blocking")
            .performClick()
        composeRule.onNodeWithTag("google_calendar_visible_0_0").performClick()
        composeRule.onNodeWithTag("google_calendar_discover_0").performClick()
        composeRule.onNodeWithTag("google_calendar_refresh_0").performClick()

        composeRule.runOnIdle {
            assertEquals(
                2,
                configured.size,
            )
            val blocking = configured[0]
            assertEquals(ACTIVE_ACCOUNT, blocking.accountId)
            assertEquals(READ_ONLY_CALENDAR, blocking.collectionId)
            assertEquals(37, blocking.request.expectedRevision)
            assertEquals(RemoteGoogleCollectionKind.CALENDAR, blocking.request.kind)
            assertTrue(blocking.request.selected)
            assertTrue(blocking.request.visible)
            assertEquals(RemoteGoogleSyncRole.BLOCKING, blocking.request.syncRole)
            val hidden = configured[1].request
            assertTrue(hidden.selected)
            assertFalse(hidden.visible)
            assertEquals(RemoteGoogleSyncRole.READ_ONLY, hidden.syncRole)
            assertEquals(ACTIVE_ACCOUNT, discoveredAccount.get())
            assertEquals(ACTIVE_ACCOUNT, refreshedAccount.get())
        }
        composeRule.onNodeWithText("Refresh import").assertIsDisplayed()
    }

    @Test
    fun calendarPolicyEditorRoutesEveryDispositionAndWritablePublicationOption() {
        val configured = mutableListOf<ConfigurationAction>()
        showCard(
            googleAccounts = listOf(
                account(
                    id = ACTIVE_ACCOUNT,
                    label = "Personal",
                    hasCalendarWriteScope = true,
                ),
            ),
            collections = listOf(
                collection(
                    id = WRITABLE_CALENDAR,
                    name = "Publish calendar",
                    syncRole = RemoteGoogleSyncRole.WRITABLE,
                    providerAccessRole = "writer",
                    calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
                    revision = 41,
                ),
            ),
            onConfigure = { accountId, collectionId, request ->
                configured += ConfigurationAction(accountId, collectionId, request)
            },
        )

        composeRule.onNodeWithTag("google_calendar_policy_0_0_confirmed_busy_ignore")
            .performClick()
        composeRule.onNodeWithTag("google_calendar_policy_0_0_tentative_blocking")
            .performClick()
        composeRule.onNodeWithTag("google_calendar_policy_0_0_free_ignore").performClick()
        composeRule.onNodeWithTag("google_calendar_policy_0_0_all_day_blocking")
            .performClick()
        composeRule.onNodeWithTag("google_calendar_publish_0_0_all_day").performClick()
        composeRule.onNodeWithTag("google_calendar_publish_0_0_tentative").performClick()
        composeRule.onNodeWithTag("google_calendar_publish_0_0_free").performClick()

        composeRule.runOnIdle {
            assertEquals(7, configured.size)
            configured.forEach { action ->
                assertEquals(ACTIVE_ACCOUNT, action.accountId)
                assertEquals(WRITABLE_CALENDAR, action.collectionId)
                assertEquals(41, action.request.expectedRevision)
                assertTrue(action.request.selected)
                assertEquals(RemoteGoogleSyncRole.WRITABLE, action.request.syncRole)
            }
            assertEquals(
                RemoteGoogleEventDisposition.IGNORE,
                configured[0].request.calendarPolicy.confirmedBusy,
            )
            assertEquals(
                RemoteGoogleEventDisposition.BLOCKING,
                configured[1].request.calendarPolicy.tentative,
            )
            assertEquals(
                RemoteGoogleEventDisposition.IGNORE,
                configured[2].request.calendarPolicy.free,
            )
            assertEquals(
                RemoteGoogleEventDisposition.BLOCKING,
                configured[3].request.calendarPolicy.allDay,
            )
            assertTrue(configured[4].request.calendarPolicy.publishAllDay)
            assertTrue(configured[5].request.calendarPolicy.publishTentative)
            assertTrue(configured[6].request.calendarPolicy.publishFree)
        }
    }

    @Test
    fun nonWritableCalendarPolicyEditStripsEveryPublicationFlag() {
        val configured = AtomicReference<ConfigurationAction?>()
        showCard(
            collections = listOf(
                collection(
                    id = READ_ONLY_CALENDAR,
                    name = "Legacy policy",
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault().copy(
                        publishAllDay = true,
                        publishTentative = true,
                        publishFree = true,
                    ),
                ),
            ),
            onConfigure = { accountId, collectionId, request ->
                configured.set(ConfigurationAction(accountId, collectionId, request))
            },
        )

        composeRule.onNodeWithTag("google_calendar_policy_0_0_tentative_ignore")
            .performClick()

        composeRule.runOnIdle {
            val policy = requireNotNull(configured.get()).request.calendarPolicy
            assertEquals(RemoteGoogleEventDisposition.IGNORE, policy.tentative)
            assertFalse(policy.publishAllDay)
            assertFalse(policy.publishTentative)
            assertFalse(policy.publishFree)
        }
        composeRule.onNodeWithTag("google_calendar_publish_0_0_all_day").assertDoesNotExist()
    }

    @Test
    fun calendarPublishRequiresFullScopeAndOwnerOrWriterProviderRole() {
        showCard(
            googleAccounts = listOf(
                account(
                    id = ACTIVE_ACCOUNT,
                    label = "Personal",
                    hasCalendarWriteScope = true,
                ),
            ),
            collections = listOf(
                collection(
                    id = READ_ONLY_CALENDAR,
                    name = "Reader",
                    providerAccessRole = "reader",
                ),
                collection(
                    id = BLOCKING_CALENDAR,
                    name = "Writer",
                    providerAccessRole = "writer",
                ),
            ),
        )

        composeRule.onNodeWithTag("google_calendar_role_0_0_writable").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_1_writable").assertIsEnabled()
    }

    @Test
    fun taskListRoutesImportAndOffWithoutOfferingBlocking() {
        val configured = mutableListOf<ConfigurationAction>()
        showCard(
            googleAccounts = listOf(
                account(
                    id = TASKS_ONLY_ACCOUNT,
                    label = "Tasks only",
                    hasCalendar = false,
                    hasTasks = true,
                ),
            ),
            collections = listOf(
                collection(
                    id = TASK_LIST,
                    name = "Inbox",
                    accountId = TASKS_ONLY_ACCOUNT,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    selected = false,
                    revision = 12,
                ),
                collection(
                    id = SECOND_TASK_LIST,
                    name = "Errands",
                    accountId = TASKS_ONLY_ACCOUNT,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                    revision = 13,
                ),
            ),
            onConfigure = { accountId, collectionId, request ->
                configured += ConfigurationAction(accountId, collectionId, request)
            },
        )

        composeRule.onNodeWithTag("google_task_role_0_0_read_only").performClick()
        composeRule.onNodeWithTag("google_task_role_0_1_off").performClick()

        composeRule.runOnIdle {
            assertEquals(
                listOf(
                    ConfigurationAction(
                        TASKS_ONLY_ACCOUNT,
                        TASK_LIST,
                        ConfigureGoogleCollectionRequest(
                            expectedRevision = 12,
                            kind = RemoteGoogleCollectionKind.TASK_LIST,
                            selected = true,
                            visible = true,
                            syncRole = RemoteGoogleSyncRole.READ_ONLY,
                        ),
                    ),
                    ConfigurationAction(
                        TASKS_ONLY_ACCOUNT,
                        SECOND_TASK_LIST,
                        ConfigureGoogleCollectionRequest(
                            expectedRevision = 13,
                            kind = RemoteGoogleCollectionKind.TASK_LIST,
                            selected = false,
                            visible = true,
                            syncRole = RemoteGoogleSyncRole.READ_ONLY,
                        ),
                    ),
                ),
                configured,
            )
        }
        composeRule.onNodeWithTag("google_task_role_0_0_blocking").assertDoesNotExist()
        composeRule.onNodeWithTag("google_task_role_0_1_blocking").assertDoesNotExist()
    }

    @Test
    fun taskPublishNeedsTasksScopeButDowngradeRemainsAvailable() {
        showCard(
            googleAccounts = listOf(
                account(
                    id = TASKS_ONLY_ACCOUNT,
                    label = "Tasks only",
                    hasCalendar = false,
                    hasTasks = true,
                ),
            ),
            collections = listOf(
                collection(
                    id = TASK_LIST,
                    name = "Publishing list",
                    accountId = TASKS_ONLY_ACCOUNT,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    syncRole = RemoteGoogleSyncRole.WRITABLE,
                ),
            ),
        )

        composeRule.onNodeWithText("Publish · writable Tasks destination").assertIsDisplayed()
        composeRule.onNodeWithTag("google_task_role_0_0_off").assertIsEnabled()
        composeRule.onNodeWithTag("google_task_role_0_0_read_only").assertIsEnabled()
        composeRule.onNodeWithTag("google_task_role_0_0_writable").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_task_role_0_0_blocking").assertDoesNotExist()
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

    @Test
    fun googleOperatorRecoveryDisablesCachedSourceControls() {
        showCard(
            collections = listOf(collection(id = BLOCKING_CALENDAR, name = "Focus")),
            googlePhase = GoogleAccountPhase.RECOVERY_REQUIRED,
        )

        composeRule.onNodeWithTag("google_calendar_discover_0").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_refresh_0").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only").assertIsNotEnabled()
    }

    @Test
    fun orphanedAuthorizationFlagDisablesCachedSourceControlsIndependently() {
        showCard(
            collections = listOf(collection(id = BLOCKING_CALENDAR, name = "Focus")),
            authorizationRecoveryDiscardRequired = true,
        )

        composeRule.onNodeWithTag("google_calendar_discover_0").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_refresh_0").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_calendar_role_0_0_read_only").assertIsNotEnabled()
    }

    @Test
    fun schedulePublicationRequiresCurrentScheduleUnlessRecoveryExists() {
        showCard(schedulePublicationHasCurrentSchedule = false)

        composeRule.onNodeWithTag("google_publish_generated_schedule").assertIsNotEnabled()
        composeRule.onNodeWithTag("google_schedule_publication_unavailable").assertIsDisplayed()
    }

    @Test
    fun savedSchedulePublicationRemainsAccessibleWithoutCurrentSchedule() {
        showCard(
            schedulePublicationHasCurrentSchedule = false,
            schedulePublicationHasRecovery = true,
        )

        composeRule.onNodeWithTag("google_publish_generated_schedule").assertIsEnabled()
        composeRule.onNodeWithText("Review saved publication").assertIsDisplayed()
        composeRule.onNodeWithTag("google_schedule_publication_unavailable").assertDoesNotExist()
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
        schedulePublicationHasCurrentSchedule: Boolean = true,
        schedulePublicationHasRecovery: Boolean = false,
        googlePhase: GoogleAccountPhase = GoogleAccountPhase.CONNECTED,
        authorizationRecoveryDiscardRequired: Boolean = false,
        onDiscover: (String) -> Unit = {},
        onRefreshOrCheck: (String) -> Unit = {},
        onConfigure: (
            String,
            String,
            ConfigureGoogleCollectionRequest,
        ) -> Unit = { _, _, _ -> },
    ) {
        composeRule.setContent {
            MaterialTheme {
                GoogleSourcesCard(
                    googleAccountState = GoogleAccountState(
                        phase = googlePhase,
                        accounts = googleAccounts,
                        message = "Google connected",
                        authorizationRecoveryDiscardRequired =
                            authorizationRecoveryDiscardRequired,
                        configurationId = CONFIGURATION_ID,
                    ),
                    importState = GoogleCalendarImportState(
                        phase = importPhase,
                        message = "Saved import status is safe to show",
                        isBusy = importBusy,
                        accounts = googleAccounts.associate { account ->
                            account.id to GoogleImportAccountState(
                                collections = collections.filter { it.accountId == account.id },
                            )
                        },
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
                    schedulePublicationHasCurrentSchedule =
                        schedulePublicationHasCurrentSchedule,
                    schedulePublicationHasRecovery = schedulePublicationHasRecovery,
                )
            }
        }
    }

    private data class ConfigurationAction(
        val accountId: String,
        val collectionId: String,
        val request: ConfigureGoogleCollectionRequest,
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
        const val SECOND_TASK_LIST = "99999999-9999-4999-8999-999999999999"

        fun account(
            id: String,
            label: String,
            status: String = "active",
            syncEnabled: Boolean = true,
            hasCalendar: Boolean = true,
            hasCalendarWriteScope: Boolean = false,
            hasTasks: Boolean = true,
            hasTasksWriteScope: Boolean = false,
        ) = GoogleAccountSummary(
            id = id,
            label = label,
            status = status,
            syncEnabled = syncEnabled,
            isDefault = id == ACTIVE_ACCOUNT,
            hasCalendar = hasCalendar,
            hasCalendarWriteScope = hasCalendarWriteScope,
            hasTasks = hasTasks,
            hasTasksWriteScope = hasTasksWriteScope,
            revision = 1,
        )

        fun collection(
            id: String,
            name: String,
            accountId: String = ACTIVE_ACCOUNT,
            kind: RemoteGoogleCollectionKind = RemoteGoogleCollectionKind.CALENDAR,
            syncRole: RemoteGoogleSyncRole = RemoteGoogleSyncRole.BLOCKING,
            revision: Long = 1,
            selected: Boolean = true,
            visible: Boolean = true,
            providerDeleted: Boolean = false,
            providerAccessRole: String? = null,
            calendarPolicy: RemoteGoogleCalendarPolicy =
                RemoteGoogleCalendarPolicy.inboundDefault(),
        ) = GoogleImportCollectionState(
            id = id,
            accountId = accountId,
            displayName = name,
            kind = kind,
            selected = selected,
            visible = visible,
            syncRole = syncRole,
            calendarPolicy = calendarPolicy,
            revision = revision,
            lastImportAt = null,
            providerDeleted = providerDeleted,
            providerAccessRole = providerAccessRole,
        )
    }
}
