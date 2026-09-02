package com.greengolddog.dayweave.state

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.health.EnergySignalState
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.MoveLaterApprovalEnvelope
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.isTimedBreakNotificationDigest
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.network.DeviceAuthUiState
import com.greengolddog.dayweave.network.DeviceAuthActionResult
import com.greengolddog.dayweave.notifications.PlannerTimedBreakNotificationRouteAccess
import com.greengolddog.dayweave.notifications.TimedBreakNotificationRouteConsumption
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.ExecutionSyncState
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.ProposalApplicationApproval
import com.greengolddog.dayweave.sync.ProposalApplicationState
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

class DayWeaveViewModel(application: Application) : AndroidViewModel(application) {
    private val dayWeaveApplication = application as DayWeaveApplication
    private val plannerStore = dayWeaveApplication.plannerStore
    private val suggestionSyncManager = dayWeaveApplication.suggestionSyncManager
    private val proposalApplicationManager = dayWeaveApplication.proposalApplicationManager
    private val canonicalSyncManager = dayWeaveApplication.canonicalSyncManager
    private val executionSyncManager = dayWeaveApplication.executionSyncManager
    private val googleAccountManager = dayWeaveApplication.googleAccountManager
    private val energySignalManager = dayWeaveApplication.energySignalManager
    private val deviceAuthCoordinator = dayWeaveApplication.deviceAuthCoordinator
    private val canonicalAuthoringController = CanonicalAuthoringController(plannerStore)
    private val timedBreakNotificationRouteAccess =
        PlannerTimedBreakNotificationRouteAccess(plannerStore)
    private val timedBreakNotificationPermissionRequestState =
        TimedBreakNotificationPermissionRequestState()
    private val scheduleCompositionProfileUpdateCoordinator =
        dayWeaveApplication.scheduleCompositionProfileUpdateCoordinator

    val state: StateFlow<com.greengolddog.dayweave.model.DayWeaveUiState> = plannerStore.state
    val durableState: StateFlow<com.greengolddog.dayweave.model.DayWeaveUiState?> =
        plannerStore.durableState
    val loadState: StateFlow<PlannerLoadState> = plannerStore.loadState
    val suggestionSyncState: StateFlow<SuggestionSyncState> = suggestionSyncManager.state
    val proposalApplicationState: StateFlow<ProposalApplicationState> =
        proposalApplicationManager.state
    val canonicalSyncState: StateFlow<CanonicalSyncState> = canonicalSyncManager.state
    val executionSyncState: StateFlow<ExecutionSyncState> = executionSyncManager.state
    val googleAccountState: StateFlow<GoogleAccountState> = googleAccountManager.state
    val energySignalState: StateFlow<EnergySignalState> = energySignalManager.state
    val deviceAuthState: StateFlow<DeviceAuthUiState> = deviceAuthCoordinator.uiState
    val healthConnectPermissions: Set<String> = energySignalManager.requiredPermissions
    val timedBreakNotificationPermissionRequestDigest: StateFlow<String?> =
        timedBreakNotificationPermissionRequestState.requestDigest
    internal val scheduleCompositionProfileUpdateState:
        StateFlow<ScheduleCompositionProfileUpdateState> =
        scheduleCompositionProfileUpdateCoordinator.state

    init {
        viewModelScope.launch {
            while (isActive) {
                delay(1_000)
                plannerStore.tickActiveSession()
            }
        }
        viewModelScope.launch {
            val restored = loadState.first { it != PlannerLoadState.LOADING }
            if (restored == PlannerLoadState.READY) energySignalManager.refresh()
        }
    }

    fun navigate(destination: AppDestination) = plannerStore.navigate(destination)
    fun startItem(id: String) {
        if (isCanonicalBlock(id)) {
            if (!plannerStore.state.value.isCanonicalPlanCurrent()) {
                recompose()
                return
            }
            if (isCanonicalBusy()) return
            dayWeaveApplication.launchCanonicalAction { executionSyncManager.start(id) }
        } else {
            val current = plannerStore.state.value
            if (
                current.canonicalExecutionSession == null &&
                current.pendingExecutionCommand == null &&
                current.pendingExecutionDeferIntent == null
            ) {
                plannerStore.startItem(id)
            }
        }
    }

    fun pauseActive(minutes: Int? = null) {
        withActiveBlock(
            canonicalAction = { id ->
                val before = plannerStore.durableState.value
                val outcome = executionSyncManager.pause(id, minutes?.let { it * 60 })
                requestNotificationPermissionAfterDurableTimedPause(
                    before = before,
                    outcome = outcome,
                    timedPauseRequested = minutes != null,
                )
                outcome
            },
            localAction = { plannerStore.pauseActive(minutes) },
        )
    }

    fun pauseActiveUntil(until: Instant) {
        withActiveBlock(
            canonicalAction = { id ->
                val before = plannerStore.durableState.value
                val outcome = executionSyncManager.pause(id, pauseUntil = until)
                requestNotificationPermissionAfterDurableTimedPause(
                    before = before,
                    outcome = outcome,
                    timedPauseRequested = true,
                )
                outcome
            },
            localAction = {
                val minutes = java.time.Duration.between(Instant.now(), until).toMinutes()
                    .coerceIn(1L, 24L * 60L)
                    .toInt()
                plannerStore.pauseActive(minutes)
            },
        )
    }

    fun resumeActive() {
        resumeActiveIfAvailable()
    }

    internal suspend fun consumeTimedBreakNotificationRoute(
        digest: String,
    ): TimedBreakNotificationRouteConsumption = timedBreakNotificationRouteAccess.consume(digest)

    suspend fun keepTimedBreakPaused(digest: String): Boolean {
        if (!dayWeaveApplication.cancelTimedBreakNotificationForAuthoritativeTransition()) {
            return false
        }
        return try {
            val receipt = plannerStore.acknowledgeTimedBreakEnded(digest)
            if (receipt != null && !receipt.awaitDurable()) return false
            val durable = plannerStore.durableState.value ?: return false
            durable.authoritativeTimedBreakNotificationIdentity()?.digest == digest &&
                durable.acknowledgedBreakEndDigest == digest
        } finally {
            dayWeaveApplication.reconcileTimedBreakNotificationAfterAuthoritativeTransition()
        }
    }

    fun takeTimedBreakNotificationPermissionRequest(digest: String): Boolean =
        timedBreakNotificationPermissionRequestState.takeIfCurrent(
            expectedDigest = digest,
            durableState = plannerStore.durableState.value,
        )

    private fun resumeActiveIfAvailable(): Boolean =
        withActiveBlock(
            canonicalAction = executionSyncManager::resume,
            localAction = plannerStore::resumeActive,
        )

    fun completeActive() {
        withActiveBlock(
            canonicalAction = { id ->
                finishCanonicalExecution(
                    command = { executionSyncManager.complete(id) },
                    refreshCanonicalState = dayWeaveApplication::refreshCanonicalState,
                )
            },
            localAction = plannerStore::completeActive,
        )
    }

    fun skipActive() {
        withActiveBlock(
            canonicalAction = { id ->
                finishCanonicalExecution(
                    command = { executionSyncManager.skip(id) },
                    refreshCanonicalState = dayWeaveApplication::refreshCanonicalState,
                )
            },
            localAction = plannerStore::skipActive,
        )
    }

    fun doActiveLater(
        moveStart: Instant? = null,
    ) {
        val activeId = plannerStore.state.value.activeSession?.itemId ?: return
        if (isCanonicalBlock(activeId) && moveStart == null) return
        withActiveBlock(
            canonicalAction = { id ->
                deferCanonicalExecutionAndRefresh(
                    command = {
                        executionSyncManager.doLater(
                            id,
                            requireNotNull(moveStart),
                        )
                    },
                    refreshCanonicalState = dayWeaveApplication::refreshCanonicalState,
                )
            },
            localAction = plannerStore::doActiveLater,
        )
    }

    fun approveActiveLater(assessmentDigest: String) {
        if (executionSyncManager.state.value.isBusy) return
        dayWeaveApplication.launchCanonicalAction {
            deferCanonicalExecutionAndRefresh(
                command = { executionSyncManager.approveDefer(assessmentDigest) },
                refreshCanonicalState = dayWeaveApplication::refreshCanonicalState,
            )
        }
    }

    fun cancelActiveLater() {
        if (executionSyncManager.state.value.isBusy) return
        dayWeaveApplication.launchCanonicalAction { executionSyncManager.cancelDefer() }
    }

    fun doScheduledLater(
        id: String,
        moveStart: Instant,
        approval: MoveLaterApprovalEnvelope? = null,
    ) {
        if (!isCanonicalBlock(id) || isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction {
            canonicalSyncManager.doLater(id, moveStart, approval)
        }
    }

    fun skipScheduled(id: String) {
        if (!isCanonicalBlock(id) || isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction {
            canonicalSyncManager.skipScheduled(id)
        }
    }
    suspend fun quickCapture(title: String, kind: ItemKind, isSensitive: Boolean): Boolean =
        canonicalAuthoringAction {
            canonicalAuthoringController.quickCapture(title, kind, isSensitive)
        }

    suspend fun createCanonicalItem(itemId: String, draft: CanonicalItemDraft): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.create(draft, itemId) }

    suspend fun convertInboxDraft(
        inboxId: String,
        itemId: String,
        draft: CanonicalItemDraft,
    ): Boolean = canonicalAuthoringAction {
        canonicalAuthoringController.convertInboxDraft(inboxId, itemId, draft)
    }

    suspend fun replaceCanonicalItem(itemId: String, draft: CanonicalItemDraft): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.replace(itemId, draft) }

    suspend fun updatePendingCanonicalItem(mutationId: String, draft: CanonicalItemDraft): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.updatePending(mutationId, draft) }

    suspend fun trashCanonicalItem(itemId: String, confirmed: Boolean): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.trash(itemId, confirmed) }

    suspend fun restoreCanonicalItem(itemId: String): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.restore(itemId) }

    suspend fun discardCanonicalAuthoringMutation(mutationId: String): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.discard(mutationId) }

    suspend fun copyConflictedCanonicalDraft(mutationId: String): Boolean =
        canonicalAuthoringAction { canonicalAuthoringController.copyConflict(mutationId) }

    /** Reconciles an interrupted submitted journal; conflicted journals remain review-only. */
    fun retryCanonicalAuthoring() = recompose()

    fun hasCredentialReplacementBlocker(): Boolean =
        plannerStore.hasCredentialReplacementBlocker()

    fun setCanonicalItemSensitive(itemId: String, expectedRevision: Long, isSensitive: Boolean) {
        if (isCanonicalBusy() || plannerStore.state.value.pendingCanonicalMutation != null) return
        dayWeaveApplication.launchCanonicalAction {
            canonicalSyncManager.setItemSensitivity(itemId, expectedRevision, isSensitive)
        }
    }
    fun approveSuggestion(id: String) {
        viewModelScope.launch { suggestionSyncManager.accept(id) }
    }

    fun reviewProposal(id: String) {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction {
            proposalApplicationManager.prepareReview(id)
        }
    }

    fun applyReviewedProposal(approval: ProposalApplicationApproval) {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction {
            if (proposalApplicationManager.applyReviewed(approval)) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    fun undoProposalApplication(id: String) {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction {
            if (proposalApplicationManager.undo(id)) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    fun discardProposalReview(id: String? = null) {
        proposalApplicationManager.discardReview(id)
    }

    fun recoverProposalApplication() {
        if (proposalApplicationManager.state.value.isBusy) return
        dayWeaveApplication.launchCanonicalAction {
            proposalApplicationManager.recoverPending()
            if (plannerStore.state.value.pendingProposalApplicationMutation == null) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    fun rejectSuggestion(id: String) {
        viewModelScope.launch { suggestionSyncManager.reject(id) }
    }

    fun updateSuggestion(id: String, title: String, summary: String) {
        viewModelScope.launch { suggestionSyncManager.edit(id, title, summary) }
    }

    fun refreshSuggestions() {
        viewModelScope.launch { suggestionSyncManager.refresh() }
    }

    fun upgradeDeviceAuthentication(baseUrl: String, bootstrapToken: String) {
        dayWeaveApplication.launchCanonicalAction {
            val result = deviceAuthCoordinator.upgradeWithBootstrap(baseUrl, bootstrapToken)
            dayWeaveApplication.suggestionSyncSchedulingCoordinator.onConfigurationSaved()
            if (result == DeviceAuthActionResult.SUCCESS) {
                dayWeaveApplication.refreshCanonicalState()
                googleAccountManager.refresh()
            }
        }
    }

    fun consumeDeviceEnrollmentCode(baseUrl: String, enrollmentCode: String) {
        dayWeaveApplication.launchCanonicalAction {
            val result = deviceAuthCoordinator.consumeOneTimeEnrollmentCode(baseUrl, enrollmentCode)
            dayWeaveApplication.suggestionSyncSchedulingCoordinator.onConfigurationSaved()
            if (result == DeviceAuthActionResult.SUCCESS) {
                dayWeaveApplication.refreshCanonicalState()
                googleAccountManager.refresh()
            }
        }
    }

    fun retryDeviceAuthentication() {
        dayWeaveApplication.launchCanonicalAction {
            val result = deviceAuthCoordinator.recoverPendingOrUpgradeLegacy()
            dayWeaveApplication.suggestionSyncSchedulingCoordinator.onConfigurationSaved()
            if (result == DeviceAuthActionResult.SUCCESS) {
                dayWeaveApplication.refreshCanonicalState()
                googleAccountManager.refresh()
            }
        }
    }

    fun signOutDeviceSession() {
        dayWeaveApplication.launchCanonicalAction {
            deviceAuthCoordinator.signOutRevokeFirst()
            dayWeaveApplication.suggestionSyncSchedulingCoordinator.onConfigurationSaved()
            googleAccountManager.refresh()
        }
    }

    fun destroyLocalDeviceAuthentication(confirmed: Boolean) {
        dayWeaveApplication.launchCanonicalAction {
            deviceAuthCoordinator.destroyLocalOnly(confirmed)
            dayWeaveApplication.suggestionSyncSchedulingCoordinator.onConfigurationSaved()
            googleAccountManager.refresh()
        }
    }

    fun refreshGoogleAccounts() {
        viewModelScope.launch { googleAccountManager.refresh() }
    }

    fun connectGoogleAccount() {
        viewModelScope.launch { googleAccountManager.connectNew() }
    }

    fun reauthorizeGoogleAccount(accountId: String) {
        viewModelScope.launch { googleAccountManager.reauthorize(accountId) }
    }

    fun restartGoogleAuthorization() {
        viewModelScope.launch { googleAccountManager.restartAuthorization() }
    }

    fun setGoogleAccountPaused(accountId: String, paused: Boolean) {
        viewModelScope.launch { googleAccountManager.setPaused(accountId, paused) }
    }

    fun disconnectGoogleAccount(accountId: String) {
        viewModelScope.launch { googleAccountManager.disconnect(accountId) }
    }

    fun openGoogleAuthorization(candidate: String, opener: (String) -> Unit) {
        viewModelScope.launch {
            try {
                if (!googleAccountManager.useAuthorizationUrlIfCurrent(candidate, opener)) {
                    googleAccountManager.refresh()
                }
            } catch (_: RuntimeException) {
                googleAccountManager.browserOpenFailed()
            }
        }
    }

    fun sendAssistantMessage(text: String): Boolean = plannerStore.sendAssistantMessage(text)
    fun toggleCompleted() = plannerStore.toggleCompleted()
    fun toggleQuietSuggestions() = plannerStore.toggleQuietSuggestions()
    fun toggleDynamicColor() = plannerStore.toggleDynamicColor()
    fun updateScheduleCompositionProfile(profile: ScheduleCompositionProfileSnapshot): Boolean =
        scheduleCompositionProfileUpdateCoordinator.update(profile)

    fun acknowledgeScheduleCompositionProfileUpdate() =
        scheduleCompositionProfileUpdateCoordinator.acknowledge()

    fun recordManualEnergyCheckIn(level: EnergyLevel) {
        plannerStore.recordManualEnergyCheckIn(level)
    }

    fun clearManualEnergyCheckIn() {
        plannerStore.clearManualEnergyCheckIn()
    }

    fun enableHealthConnect() {
        viewModelScope.launch { energySignalManager.enable() }
    }

    fun disableHealthConnect() {
        viewModelScope.launch { energySignalManager.disable() }
    }

    fun onHealthConnectPermissionResult(granted: Set<String>) {
        viewModelScope.launch { energySignalManager.onPermissionResult(granted) }
    }

    fun refreshEnergySignal() {
        viewModelScope.launch { energySignalManager.refresh() }
    }

    fun recompose() {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction { dayWeaveApplication.refreshCanonicalState() }
    }

    fun composeOnDevice() {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchLocalScheduleComposition()
    }

    fun cancelLocalScheduleComposition() {
        dayWeaveApplication.cancelLocalScheduleComposition()
    }

    fun setLocalScheduleCompositionForegroundActive(active: Boolean) {
        dayWeaveApplication.setLocalScheduleCompositionForegroundActive(active)
    }

    /** Called only while the application UI is STARTED; the process action gate coalesces races. */
    fun refreshExecution() {
        if (
            canonicalSyncManager.state.value.isBusy || executionSyncManager.state.value.isBusy ||
            proposalApplicationManager.state.value.isBusy
        ) return
        dayWeaveApplication.launchCanonicalAction {
            dayWeaveApplication.refreshForegroundExecution()
        }
    }

    /** The caller owns the unlocked STARTED lifecycle; cancellation drains the active stream. */
    suspend fun collectForegroundExecutionInvalidations() {
        dayWeaveApplication.runForegroundExecutionInvalidations()
    }

    /** The caller owns the unlocked STARTED lifecycle and therefore every item/probe socket. */
    suspend fun collectForegroundCanonicalItemInvalidations() {
        dayWeaveApplication.runForegroundCanonicalItemInvalidations()
    }

    /** The caller owns the unlocked STARTED lifecycle and therefore the schedule GET/SSE sockets. */
    suspend fun collectForegroundScheduleInvalidations() {
        dayWeaveApplication.runForegroundScheduleInvalidations()
    }

    fun keepLatestItemAfterTerminalConflict(sessionId: String) {
        if (isCanonicalBusy() || plannerStore.state.value.pendingCanonicalMutation != null) return
        dayWeaveApplication.launchCanonicalAction {
            val current = plannerStore.state.value
            val outcome = current.terminalExecutionOutcomes[sessionId]
            if (
                current.pendingCanonicalMutation != null ||
                outcome?.session?.status !in setOf("completed", "skipped") ||
                outcome?.let { current.isNewestExecutionForProjection(it.session) } != true ||
                outcome?.canonicalProjectionConflict == null
            ) {
                return@launchCanonicalAction
            }
            plannerStore.keepLatestItemAfterTerminalConflict(sessionId)?.awaitDurable()
        }
    }

    fun retryTerminalProjection(sessionId: String) {
        if (isCanonicalBusy() || plannerStore.state.value.pendingCanonicalMutation != null) return
        dayWeaveApplication.launchCanonicalAction {
            val current = plannerStore.state.value
            val outcome = current.terminalExecutionOutcomes[sessionId]
            if (
                current.pendingCanonicalMutation != null ||
                outcome?.session?.status !in setOf("completed", "skipped") ||
                outcome?.let { current.isNewestExecutionForProjection(it.session) } != true ||
                outcome?.canonicalProjectionConflict == null ||
                outcome.canonicalProjectionRetryAuthorizedAt != null
            ) {
                return@launchCanonicalAction
            }
            val authorization = plannerStore.authorizeTerminalProjectionRetry(sessionId)
                ?: return@launchCanonicalAction
            if (authorization.awaitDurable()) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    private fun isCanonicalBlock(id: String): Boolean =
        executionActionTarget(plannerStore.state.value, id) == ExecutionActionTarget.SERVER

    private fun withActiveBlock(
        canonicalAction: suspend (String) -> Any?,
        localAction: () -> Unit,
    ): Boolean {
        val activeId = plannerStore.state.value.activeSession?.itemId ?: return false
        if (isCanonicalBlock(activeId)) {
            if (isCanonicalBusy()) return false
            return dayWeaveApplication.launchCanonicalAction { canonicalAction(activeId) }
        } else {
            localAction()
            return true
        }
    }

    private fun isCanonicalBusy(): Boolean =
        canonicalSyncManager.state.value.isBusy || executionSyncManager.state.value.isBusy ||
            proposalApplicationManager.state.value.isBusy ||
            plannerStore.state.value.pendingExecutionDeferIntent != null ||
            plannerStore.state.value.pendingProposalApplicationMutation != null

    private suspend fun requestNotificationPermissionAfterDurableTimedPause(
        before: DayWeaveUiState?,
        outcome: ExecutionSyncOutcome,
        timedPauseRequested: Boolean,
    ) {
        if (
            shouldRequestPermissionAfterDurableTimedPause(
                before = before,
                after = plannerStore.durableState.value,
                outcome = outcome,
                timedPauseRequested = timedPauseRequested,
            )
        ) {
            plannerStore.durableState.value?.authoritativeTimedBreakNotificationIdentity()
                ?.digest
                ?.let(timedBreakNotificationPermissionRequestState::offer)
        }
    }

    private suspend inline fun canonicalAuthoringAction(
        crossinline action: suspend () -> Boolean,
    ): Boolean {
        if (isCanonicalBusy()) return false
        return try {
            persistCanonicalAuthoringThenScheduleSync(
                persist = { action() },
                scheduleSync = {
                    dayWeaveApplication.launchCanonicalAction {
                        dayWeaveApplication.refreshCanonicalState()
                    }
                },
            )
        } catch (error: RuntimeException) {
            if (error is CancellationException) throw error
            false
        }
    }
}

/** Activity-safe contextual one-shot state retained by the ViewModel across stop/lock/recreation. */
internal class TimedBreakNotificationPermissionRequestState {
    private val mutableRequestDigest = MutableStateFlow<String?>(null)
    val requestDigest: StateFlow<String?> = mutableRequestDigest.asStateFlow()

    fun offer(digest: String) {
        require(isTimedBreakNotificationDigest(digest))
        mutableRequestDigest.value = digest
    }

    fun takeIfCurrent(expectedDigest: String, durableState: DayWeaveUiState?): Boolean {
        require(isTimedBreakNotificationDigest(expectedDigest))
        if (!mutableRequestDigest.compareAndSet(expectedDigest, null)) return false
        return durableState?.authoritativeTimedBreakNotificationIdentity()?.digest == expectedDigest
    }
}

internal fun shouldRequestPermissionAfterDurableTimedPause(
    before: DayWeaveUiState?,
    after: DayWeaveUiState?,
    outcome: ExecutionSyncOutcome,
    timedPauseRequested: Boolean,
): Boolean {
    if (
        !timedPauseRequested ||
        outcome !in setOf(ExecutionSyncOutcome.SUCCESS, ExecutionSyncOutcome.RECOVERED_COMMAND)
    ) {
        return false
    }
    val afterIdentity = after?.authoritativeTimedBreakNotificationIdentity() ?: return false
    return before?.authoritativeTimedBreakNotificationIdentity()?.digest != afterIdentity.digest
}

/** Local durability is success; synchronization is best-effort and remains manually retryable. */
internal suspend fun persistCanonicalAuthoringThenScheduleSync(
    persist: suspend () -> Boolean,
    scheduleSync: () -> Unit,
): Boolean {
    val persisted = persist()
    if (persisted) scheduleSync()
    return persisted
}

/** Terminal execution success immediately enters the application projection/recompose sequence. */
internal suspend fun finishCanonicalExecution(
    command: suspend () -> ExecutionSyncOutcome,
    refreshCanonicalState: suspend () -> Unit,
): ExecutionSyncOutcome {
    val outcome = command()
    if (outcome == ExecutionSyncOutcome.SUCCESS) refreshCanonicalState()
    return outcome
}

/** A confirmed Defer must be followed by compose+publish before its replacement can start. */
internal suspend fun deferCanonicalExecutionAndRefresh(
    command: suspend () -> ExecutionSyncOutcome,
    refreshCanonicalState: suspend () -> Unit,
): ExecutionSyncOutcome {
    val outcome = command()
    if (
        outcome == ExecutionSyncOutcome.SUCCESS ||
        outcome == ExecutionSyncOutcome.RECOVERED_COMMAND
    ) {
        refreshCanonicalState()
    }
    return outcome
}

internal enum class ExecutionActionTarget { LOCAL, SERVER }

/** Keeps canonical work fail-closed even if a refreshed plan temporarily cannot locate its block. */
internal fun executionActionTarget(
    state: DayWeaveUiState,
    blockId: String,
): ExecutionActionTarget = if (
    state.schedule.firstOrNull { it.id == blockId }?.canonicalItemId != null ||
    (
        state.activeSession?.itemId == blockId &&
            state.activeSession.canonicalExecutionSessionId != null
        )
) {
    ExecutionActionTarget.SERVER
} else {
    ExecutionActionTarget.LOCAL
}
