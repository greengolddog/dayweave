package com.greengolddog.dayweave.ui.onboarding

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.OnboardingFirstItemCheck
import com.greengolddog.dayweave.model.hasExactOnboardingFirstPlanProof
import com.greengolddog.dayweave.model.validatedOnboardingFirstItemCheck
import com.greengolddog.dayweave.network.DeviceAuthPhase
import com.greengolddog.dayweave.network.DeviceAuthUiState
import com.greengolddog.dayweave.network.RemoteGoogleCalendarProjectionState
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportState

/**
 * Projects live authoritative integration state into content-free setup checks.
 *
 * A stored credential or an account row alone is deliberately insufficient. Setup only reports
 * ready after the current binding has completed an authenticated planner request and after one
 * selected Calendar plus one selected Tasks list have exact, current import evidence.
 */
internal fun apiOnboardingCheck(
    deviceAuth: DeviceAuthUiState,
    canonicalSync: CanonicalSyncState,
): OnboardingCheckState = when {
    deviceAuth.isBusy || canonicalSync.isBusy -> OnboardingCheckState.WORKING
    deviceAuth.phase == DeviceAuthPhase.ACTIVE &&
        canonicalSync.phase == CanonicalSyncPhase.CONNECTED -> OnboardingCheckState.READY
    deviceAuth.phase in setOf(
        DeviceAuthPhase.REAUTH,
        DeviceAuthPhase.INCOMPATIBLE,
    ) || canonicalSync.phase in setOf(
        CanonicalSyncPhase.AUTH_REQUIRED,
        CanonicalSyncPhase.OFFLINE,
        CanonicalSyncPhase.ERROR,
    ) -> OnboardingCheckState.NEEDS_ATTENTION
    else -> OnboardingCheckState.PENDING
}

internal fun googleOnboardingCheck(
    accountState: GoogleAccountState,
    importState: GoogleCalendarImportState,
): OnboardingCheckState {
    if (accountState.isBusy || importState.isBusy) return OnboardingCheckState.WORKING
    if (
        accountState.phase in setOf(
            GoogleAccountPhase.AUTHORIZATION_RECOVERY,
            GoogleAccountPhase.AUTH_REQUIRED,
            GoogleAccountPhase.RECOVERY_REQUIRED,
            GoogleAccountPhase.OFFLINE,
            GoogleAccountPhase.ERROR,
        ) ||
        accountState.authorizationRecoveryResetRequired ||
        accountState.authorizationRecoveryDiscardRequired ||
        importState.phase in setOf(
            GoogleCalendarImportPhase.AUTH_REQUIRED,
            GoogleCalendarImportPhase.OFFLINE,
            GoogleCalendarImportPhase.RECOVERY_REQUIRED,
            GoogleCalendarImportPhase.ERROR,
        ) ||
        importState.pendingRecoveryCount != 0 ||
        importState.pendingRecoveryAccountIds.isNotEmpty()
    ) {
        return OnboardingCheckState.NEEDS_ATTENTION
    }
    if (
        accountState.phase != GoogleAccountPhase.CONNECTED ||
        accountState.configurationId == null ||
        accountState.configurationId != importState.configurationId ||
        importState.phase !in setOf(
            GoogleCalendarImportPhase.READY,
            GoogleCalendarImportPhase.COMPLETED,
        )
    ) {
        return OnboardingCheckState.PENDING
    }

    var exactCalendarReady = false
    var exactTaskListReady = false
    accountState.accounts.filter { it.syncEnabled }.forEach { account ->
        val imports = importState.accounts[account.id] ?: return@forEach
        val run = imports.run ?: return@forEach
        val exactRun = run.state == RemoteGoogleSyncRunState.IDLE &&
            run.refreshGeneration > 0 &&
            run.claimedRefreshGeneration == run.refreshGeneration &&
            run.completedRefreshGeneration == run.refreshGeneration
        if (!exactRun) return@forEach

        val selected = imports.collections.filter {
            it.selected && !it.providerDeleted && it.configuredAt != null && it.lastImportAt != null
        }
        exactCalendarReady = exactCalendarReady || account.hasCalendar && selected.any { collection ->
            collection.accountId == account.id &&
                collection.kind == RemoteGoogleCollectionKind.CALENDAR &&
                collection.planningProjectionState ==
                RemoteGoogleCalendarProjectionState.COMPLETE &&
                collection.planningGeneration > 0 &&
                collection.planningCollectionRevision == collection.revision &&
                collection.planningWindowStart != null &&
                collection.planningWindowEnd != null &&
                collection.planningWindowRefreshedAt != null
        }
        exactTaskListReady = exactTaskListReady || account.hasTasks && selected.any {
            it.accountId == account.id && it.kind == RemoteGoogleCollectionKind.TASK_LIST
        }
    }
    return if (exactCalendarReady && exactTaskListReady) {
        OnboardingCheckState.READY
    } else {
        OnboardingCheckState.PENDING
    }
}

internal fun profileOnboardingCheck(
    current: DayWeaveUiState,
    durable: DayWeaveUiState?,
    update: ScheduleCompositionProfileUpdateState,
    profileReviewed: Boolean,
): OnboardingCheckState = when (update.phase) {
    ScheduleCompositionProfileUpdatePhase.SAVING -> OnboardingCheckState.WORKING
    ScheduleCompositionProfileUpdatePhase.BLOCKED,
    ScheduleCompositionProfileUpdatePhase.ERROR,
    -> OnboardingCheckState.NEEDS_ATTENTION
    ScheduleCompositionProfileUpdatePhase.IDLE,
    ScheduleCompositionProfileUpdatePhase.SAVED,
    -> if (!profileReviewed) {
        OnboardingCheckState.PENDING
    } else if (
        current.scheduleCompositionProfile.hasValidShape() &&
        durable?.scheduleCompositionProfile == current.scheduleCompositionProfile
    ) {
        OnboardingCheckState.READY
    } else {
        OnboardingCheckState.PENDING
    }
}

/** First-item completion is derived only from the encrypted durable planner generation. */
internal fun firstItemOnboardingCheck(
    durable: DayWeaveUiState?,
): OnboardingCheckState {
    durable ?: return OnboardingCheckState.PENDING
    return when (durable.validatedOnboardingFirstItemCheck()) {
        OnboardingFirstItemCheck.PENDING_CREATE,
        OnboardingFirstItemCheck.CANONICAL_ITEM,
        -> OnboardingCheckState.READY
        null -> if (durable.onboardingFirstItemAnchor == null) {
            OnboardingCheckState.PENDING
        } else {
            OnboardingCheckState.NEEDS_ATTENTION
        }
    }
}

/** First-plan completion requires an exact whole-plan publication proof for the reviewed item. */
internal fun firstPlanOnboardingCheck(
    durable: DayWeaveUiState?,
): OnboardingCheckState {
    durable ?: return OnboardingCheckState.PENDING
    val anchor = durable.onboardingFirstItemAnchor ?: return OnboardingCheckState.PENDING
    val anchorMutations = durable.pendingCanonicalAuthoringMutations.filter {
        it.itemId == anchor.itemId
    }
    if (anchorMutations.any {
            it.disposition == CanonicalAuthoringDisposition.CONFLICTED
        }
    ) {
        return OnboardingCheckState.NEEDS_ATTENTION
    }
    if (
        durable.pendingSchedulePublication != null ||
        anchorMutations.any { it.disposition == CanonicalAuthoringDisposition.PENDING }
    ) {
        return OnboardingCheckState.WORKING
    }
    return when {
        durable.validatedOnboardingFirstItemCheck() !=
            OnboardingFirstItemCheck.CANONICAL_ITEM -> OnboardingCheckState.NEEDS_ATTENTION
        durable.hasExactOnboardingFirstPlanProof() -> OnboardingCheckState.READY
        else -> OnboardingCheckState.PENDING
    }
}
