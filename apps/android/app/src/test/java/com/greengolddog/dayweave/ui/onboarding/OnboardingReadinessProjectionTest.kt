package com.greengolddog.dayweave.ui.onboarding

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.OnboardingFirstItemAnchorSnapshot
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionHintSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.network.DeviceAuthPhase
import com.greengolddog.dayweave.network.DeviceAuthUiState
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCalendarProjectionState
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.sync.GoogleImportAccountState
import com.greengolddog.dayweave.sync.GoogleImportCollectionState
import com.greengolddog.dayweave.sync.GoogleImportRunState
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Test

class OnboardingReadinessProjectionTest {
    @Test
    fun apiRequiresFreshAuthenticatedPlannerSuccess() {
        val device = deviceAuth(DeviceAuthPhase.ACTIVE)

        assertEquals(
            OnboardingCheckState.PENDING,
            apiOnboardingCheck(device, canonical(CanonicalSyncPhase.READY)),
        )
        assertEquals(
            OnboardingCheckState.READY,
            apiOnboardingCheck(device, canonical(CanonicalSyncPhase.CONNECTED)),
        )
    }

    @Test
    fun apiSurfacesBusyAndRepairStatesWithoutClaimingReady() {
        assertEquals(
            OnboardingCheckState.WORKING,
            apiOnboardingCheck(
                deviceAuth(DeviceAuthPhase.ACTIVE, isBusy = true),
                canonical(CanonicalSyncPhase.CONNECTED),
            ),
        )
        assertEquals(
            OnboardingCheckState.NEEDS_ATTENTION,
            apiOnboardingCheck(
                deviceAuth(DeviceAuthPhase.REAUTH),
                canonical(CanonicalSyncPhase.AUTH_REQUIRED),
            ),
        )
    }

    @Test
    fun googleRequiresBothSelectedKindsAndExactCompletedProjection() {
        val (accounts, imports) = exactGoogleStates()
        assertEquals(OnboardingCheckState.READY, googleOnboardingCheck(accounts, imports))

        val withoutTasks = imports.withCollections { collections ->
            collections.filterNot { it.kind == RemoteGoogleCollectionKind.TASK_LIST }
        }
        assertEquals(
            OnboardingCheckState.PENDING,
            googleOnboardingCheck(accounts, withoutTasks),
        )

        val staleCalendar = imports.withCollections { collections ->
            collections.map { collection ->
                if (collection.kind == RemoteGoogleCollectionKind.CALENDAR) {
                    collection.copy(planningCollectionRevision = collection.revision - 1)
                } else {
                    collection
                }
            }
        }
        assertEquals(
            OnboardingCheckState.PENDING,
            googleOnboardingCheck(accounts, staleCalendar),
        )
    }

    @Test
    fun googleRejectsBindingMismatchAndRecovery() {
        val (accounts, imports) = exactGoogleStates()
        assertEquals(
            OnboardingCheckState.PENDING,
            googleOnboardingCheck(accounts, imports.copy(configurationId = OTHER_CONFIGURATION)),
        )
        assertEquals(
            OnboardingCheckState.NEEDS_ATTENTION,
            googleOnboardingCheck(
                accounts,
                imports.copy(
                    phase = GoogleCalendarImportPhase.RECOVERY_REQUIRED,
                    pendingRecoveryCount = 1,
                ),
            ),
        )
    }

    @Test
    fun googleAllowsExactCalendarAndTaskSelectionsAcrossDifferentAccounts() {
        val (singleAccountState, singleImportState) = exactGoogleStates()
        val originalAccount = singleAccountState.accounts.single()
        val originalImport = singleImportState.accounts.getValue(ACCOUNT_ID)
        val calendarOnlyAccount = originalAccount.copy(hasTasks = false)
        val tasksOnlyAccount = originalAccount.copy(
            id = OTHER_ACCOUNT_ID,
            isDefault = false,
            hasCalendar = false,
        )
        val calendar = originalImport.collections.single {
            it.kind == RemoteGoogleCollectionKind.CALENDAR
        }
        val tasks = originalImport.collections.single {
            it.kind == RemoteGoogleCollectionKind.TASK_LIST
        }.copy(accountId = OTHER_ACCOUNT_ID)

        assertEquals(
            OnboardingCheckState.READY,
            googleOnboardingCheck(
                singleAccountState.copy(accounts = listOf(calendarOnlyAccount, tasksOnlyAccount)),
                singleImportState.copy(
                    accounts = mapOf(
                        ACCOUNT_ID to originalImport.copy(collections = listOf(calendar)),
                        OTHER_ACCOUNT_ID to originalImport.copy(collections = listOf(tasks)),
                    ),
                ),
            ),
        )
    }

    @Test
    fun profileRequiresTheExactEncryptedDurableValue() {
        val current = DayWeaveUiState()
        assertEquals(
            OnboardingCheckState.PENDING,
            profileOnboardingCheck(
                current,
                null,
                ScheduleCompositionProfileUpdateState(),
                profileReviewed = true,
            ),
        )
        assertEquals(
            OnboardingCheckState.PENDING,
            profileOnboardingCheck(
                current,
                current,
                ScheduleCompositionProfileUpdateState(),
                profileReviewed = false,
            ),
        )
        assertEquals(
            OnboardingCheckState.READY,
            profileOnboardingCheck(
                current,
                current,
                ScheduleCompositionProfileUpdateState(),
                profileReviewed = true,
            ),
        )
        assertEquals(
            OnboardingCheckState.NEEDS_ATTENTION,
            profileOnboardingCheck(
                current,
                current,
                ScheduleCompositionProfileUpdateState(
                    phase = ScheduleCompositionProfileUpdatePhase.ERROR,
                ),
                profileReviewed = true,
            ),
        )
    }

    @Test
    fun firstItemRequiresExactDurableAnchorRelationship() {
        val pending = pendingFirstItemState()
        assertEquals(OnboardingCheckState.READY, firstItemOnboardingCheck(pending))
        assertEquals(OnboardingCheckState.PENDING, firstItemOnboardingCheck(null))
        assertEquals(
            OnboardingCheckState.NEEDS_ATTENTION,
            firstItemOnboardingCheck(
                DayWeaveUiState(
                    onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
                ),
            ),
        )
    }

    @Test
    fun firstPlanRequiresExactWholePlanPublicationEvidence() {
        assertEquals(
            OnboardingCheckState.WORKING,
            firstPlanOnboardingCheck(pendingFirstItemState()),
        )
        val published = publishedFirstPlanState()
        assertEquals(OnboardingCheckState.READY, firstPlanOnboardingCheck(published))
        assertEquals(
            OnboardingCheckState.PENDING,
            firstPlanOnboardingCheck(published.copy(publishedScheduleProof = null)),
        )
        assertEquals(
            OnboardingCheckState.NEEDS_ATTENTION,
            firstPlanOnboardingCheck(
                published.copy(
                    onboardingFirstItemAnchor =
                        OnboardingFirstItemAnchorSnapshot(ITEM_ID, 2),
                ),
            ),
        )
    }

    private fun deviceAuth(
        phase: DeviceAuthPhase,
        isBusy: Boolean = false,
    ) = DeviceAuthUiState(
        phase = phase,
        baseUrl = "https://dayweave.example/",
        clientInstanceId = null,
        sessionId = null,
        deviceLabel = null,
        accessExpiresAt = null,
        message = "",
        isBusy = isBusy,
    )

    private fun canonical(phase: CanonicalSyncPhase) = CanonicalSyncState(
        phase = phase,
        message = "",
    )

    private fun exactGoogleStates(): Pair<GoogleAccountState, GoogleCalendarImportState> {
        val account = GoogleAccountSummary(
            id = ACCOUNT_ID,
            label = "Private account",
            status = "active",
            syncEnabled = true,
            isDefault = true,
            hasCalendar = true,
            hasCalendarWriteScope = false,
            hasTasks = true,
            hasTasksWriteScope = false,
            revision = 1,
        )
        val accountState = GoogleAccountState(
            phase = GoogleAccountPhase.CONNECTED,
            accounts = listOf(account),
            message = "",
            configurationId = CONFIGURATION,
        )
        val calendar = collection(
            id = CALENDAR_ID,
            kind = RemoteGoogleCollectionKind.CALENDAR,
        ).copy(
            planningProjectionState = RemoteGoogleCalendarProjectionState.COMPLETE,
            planningGeneration = 3,
            planningCollectionRevision = 4,
            planningWindowStart = "2026-09-01T00:00:00Z",
            planningWindowEnd = "2026-09-08T00:00:00Z",
            planningWindowRefreshedAt = "2026-09-01T12:00:00Z",
        )
        val tasks = collection(
            id = TASKS_ID,
            kind = RemoteGoogleCollectionKind.TASK_LIST,
        )
        val importState = GoogleCalendarImportState(
            phase = GoogleCalendarImportPhase.COMPLETED,
            message = "",
            accounts = mapOf(
                ACCOUNT_ID to GoogleImportAccountState(
                    collections = listOf(calendar, tasks),
                    run = GoogleImportRunState(
                        state = RemoteGoogleSyncRunState.IDLE,
                        refreshGeneration = 3,
                        claimedRefreshGeneration = 3,
                        completedRefreshGeneration = 3,
                        nextAttemptAt = Instant.parse("2026-09-02T00:00:00Z"),
                        importedCount = 2,
                        updatedCount = 0,
                        deletedCount = 0,
                        conflictCount = 0,
                        rejectedCount = 0,
                    ),
                ),
            ),
            configurationId = CONFIGURATION,
        )
        return accountState to importState
    }

    private fun collection(
        id: String,
        kind: RemoteGoogleCollectionKind,
    ) = GoogleImportCollectionState(
        id = id,
        accountId = ACCOUNT_ID,
        displayName = "Private source",
        kind = kind,
        providerDeleted = false,
        selected = true,
        visible = true,
        syncRole = RemoteGoogleSyncRole.READ_ONLY,
        calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
        revision = 4,
        lastImportAt = "2026-09-01T12:00:00Z",
        configuredAt = "2026-09-01T11:00:00Z",
    )

    private fun GoogleCalendarImportState.withCollections(
        transform: (List<GoogleImportCollectionState>) -> List<GoogleImportCollectionState>,
    ): GoogleCalendarImportState = copy(
        accounts = accounts.mapValues { (_, account) ->
            account.copy(collections = transform(account.collections))
        },
    )

    private fun pendingFirstItemState(): DayWeaveUiState {
        val draft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.TASK,
            title = "First task",
            timezoneName = "UTC",
            durationSeconds = 1_800,
        )
        return DayWeaveUiState(
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
            pendingCanonicalAuthoringMutations = listOf(
                PendingCanonicalAuthoringMutation(
                    id = MUTATION_ID,
                    itemId = ITEM_ID,
                    operation = CanonicalAuthoringOperation.CREATE,
                    draft = draft,
                    createdAt = CREATED_AT,
                ),
            ),
        )
    }

    private fun publishedFirstPlanState(): DayWeaveUiState {
        val item = CanonicalItemSnapshot(
            id = ITEM_ID,
            kind = "task",
            status = "planned",
            title = "First task",
            timezoneName = "UTC",
            durationSeconds = 1_800,
            flexibleConstraintsJson = "{}",
            splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 50,
            urgency = 50,
            siblingOrder = 0,
            isExecutable = true,
            revision = 1,
            createdAt = CREATED_AT,
            updatedAt = "2026-09-03T07:30:00Z",
        )
        val block = ScheduleItem(
            id = BLOCK_ID,
            title = item.title,
            kind = ItemKind.TASK,
            startMinute = 9 * 60,
            durationMinutes = 30,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = ITEM_ID,
            canonicalRevision = 1,
            sessionIndex = 0,
            absoluteStartAt = "2026-09-03T09:00:00Z",
            absoluteEndAt = "2026-09-03T09:30:00Z",
            planningZoneId = "UTC",
            canonicalBlockKind = "planned",
        )
        val revision = PublishedScheduleRevisionSnapshot(
            id = PUBLICATION_ID,
            revision = "1:$PUBLICATION_ID",
            revisionNumber = 1uL,
            inputDigest = PLAN_DIGEST,
            horizonStart = "2026-09-03T00:00:00Z",
            horizonEnd = "2026-09-10T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = "2026-09-03T08:00:00Z",
        )
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = "https://example.test/",
            configurationId = CONFIGURATION,
            revision = revision,
            asOf = "2026-09-03T08:00:00Z",
            blocks = listOf(PublishedScheduleBlockProofSnapshot.from(block)),
        )
        return DayWeaveUiState(
            canonicalItems = listOf(item),
            schedule = listOf(block),
            canonicalSyncOrigin = proof.syncOrigin,
            canonicalConfigurationId = proof.configurationId,
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID, 1),
            publishedScheduleRevision = revision,
            publishedScheduleProof = proof,
            publishedScheduleRevisionHint = PublishedScheduleRevisionHintSnapshot(
                syncOrigin = proof.syncOrigin,
                configurationId = proof.configurationId,
                revisionNumber = revision.revisionNumber,
            ),
            scheduleInputDigest = PLAN_DIGEST,
            scheduleGeneratedAt = proof.asOf,
            schedulePlanningZoneId = "UTC",
        )
    }

    private companion object {
        const val CONFIGURATION = "11111111-1111-4111-8111-111111111111"
        const val OTHER_CONFIGURATION = "22222222-2222-4222-8222-222222222222"
        const val ACCOUNT_ID = "33333333-3333-4333-8333-333333333333"
        const val OTHER_ACCOUNT_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val CALENDAR_ID = "44444444-4444-4444-8444-444444444444"
        const val TASKS_ID = "55555555-5555-4555-8555-555555555555"
        const val ITEM_ID = "66666666-6666-4666-8666-666666666666"
        const val MUTATION_ID = "77777777-7777-4777-8777-777777777777"
        const val BLOCK_ID = "88888888-8888-4888-8888-888888888888"
        const val PUBLICATION_ID = "99999999-9999-4999-8999-999999999999"
        const val CREATED_AT = "2026-09-03T07:00:00Z"
        const val PLAN_DIGEST =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
}
