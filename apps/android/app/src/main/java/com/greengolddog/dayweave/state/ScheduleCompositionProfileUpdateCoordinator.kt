package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.util.UUID

internal enum class ScheduleCompositionProfileEditBlocker(val message: String) {
    SCHEDULE_PUBLICATION("Wait for the current schedule update to reconcile."),
    PROPOSAL_APPLICATION("Wait for the current assistant proposal to reconcile."),
    CANONICAL_ITEM_WRITE("Wait for the current item change to reconcile."),
    EXECUTION_CHANGE("Finish or reconcile the current focus action first."),
    EXECUTION_PROJECTION("Wait for the completed focus action to sync."),
}

/**
 * One fail-closed policy shared by presentation and the store mutation boundary.
 *
 * Keep this in lockstep with every state that can still change canonical schedule authority.
 */
internal fun DayWeaveUiState.scheduleCompositionProfileEditBlocker():
    ScheduleCompositionProfileEditBlocker? = when {
    pendingSchedulePublication != null ->
        ScheduleCompositionProfileEditBlocker.SCHEDULE_PUBLICATION
    pendingProposalApplicationMutation != null ->
        ScheduleCompositionProfileEditBlocker.PROPOSAL_APPLICATION
    pendingCanonicalMutation != null || pendingCanonicalAuthoringMutations.isNotEmpty() ->
        ScheduleCompositionProfileEditBlocker.CANONICAL_ITEM_WRITE
    pendingExecutionCommand != null || pendingExecutionDeferIntent != null ||
        canonicalExecutionSession != null || activeSession != null ||
        schedule.any { block ->
            block.canonicalItemId != null &&
                block.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED)
        } -> ScheduleCompositionProfileEditBlocker.EXECUTION_CHANGE
    terminalExecutionOutcomes.values.any { outcome ->
        outcome.requiresCanonicalItemProjection &&
            outcome.canonicalProjectionRevision == null &&
            outcome.canonicalProjectionResolution == null &&
            isNewestExecutionForProjection(outcome.session)
    } -> ScheduleCompositionProfileEditBlocker.EXECUTION_PROJECTION
    else -> null
}

enum class ScheduleCompositionProfileUpdatePhase {
    IDLE,
    SAVING,
    SAVED,
    BLOCKED,
    ERROR,
}

data class ScheduleCompositionProfileUpdateState(
    val phase: ScheduleCompositionProfileUpdatePhase = ScheduleCompositionProfileUpdatePhase.IDLE,
    val requestedProfile: ScheduleCompositionProfileSnapshot? = null,
    val message: String? = null,
) {
    val isSaving: Boolean
        get() = phase == ScheduleCompositionProfileUpdatePhase.SAVING
}

/**
 * ViewModel-facing coordinator. The injected launcher is the process canonical-action gate, so
 * the exact encrypted save survives a transient Activity/ViewModel lifecycle boundary.
 */
internal class ScheduleCompositionProfileUpdateCoordinator(
    private val plannerStore: PlannerStore,
    persistProfile: (suspend (ScheduleCompositionProfileSnapshot) -> Boolean)? = null,
    private val launchCanonicalAction: (suspend () -> Unit) -> Boolean,
) {
    private val persistProfile = persistProfile ?: ::persistProfileDurably
    private val mutableState = MutableStateFlow(ScheduleCompositionProfileUpdateState())
    val state: StateFlow<ScheduleCompositionProfileUpdateState> = mutableState.asStateFlow()

    fun update(profile: ScheduleCompositionProfileSnapshot): Boolean {
        if (mutableState.value.isSaving) return false
        if (!profile.hasValidShape()) {
            mutableState.value = ScheduleCompositionProfileUpdateState(
                phase = ScheduleCompositionProfileUpdatePhase.BLOCKED,
                requestedProfile = profile,
                message = "Check the planning profile values and try again.",
            )
            return false
        }
        plannerStore.state.value.scheduleCompositionProfileEditBlocker()?.let { blocker ->
            mutableState.value = ScheduleCompositionProfileUpdateState(
                phase = ScheduleCompositionProfileUpdatePhase.BLOCKED,
                requestedProfile = profile,
                message = blocker.message,
            )
            return false
        }

        mutableState.value = ScheduleCompositionProfileUpdateState(
            phase = ScheduleCompositionProfileUpdatePhase.SAVING,
            requestedProfile = profile,
            message = "Saving the encrypted planning profile…",
        )
        val launched = launchCanonicalAction {
            val saved = try {
                persistProfile(profile)
            } catch (cancelled: CancellationException) {
                mutableState.value = ScheduleCompositionProfileUpdateState(
                    phase = ScheduleCompositionProfileUpdatePhase.ERROR,
                    requestedProfile = profile,
                    message = "Planning profile save was interrupted before confirmation.",
                )
                throw cancelled
            } catch (_: Exception) {
                plannerStore.durableState.value?.scheduleCompositionProfile == profile
            }
            mutableState.value = ScheduleCompositionProfileUpdateState(
                phase = if (saved) {
                    ScheduleCompositionProfileUpdatePhase.SAVED
                } else {
                    ScheduleCompositionProfileUpdatePhase.ERROR
                },
                requestedProfile = profile,
                message = if (saved) {
                    "Planning profile saved. Recompose to refresh your day."
                } else {
                    "The planning profile could not be saved securely. Try again after storage " +
                        "is available."
                },
            )
        }
        if (!launched) {
            mutableState.value = ScheduleCompositionProfileUpdateState(
                phase = ScheduleCompositionProfileUpdatePhase.BLOCKED,
                requestedProfile = profile,
                message = "Another planner action is finishing. Try again in a moment.",
            )
        }
        return launched
    }

    private suspend fun persistProfileDurably(
        profile: ScheduleCompositionProfileSnapshot,
    ): Boolean {
        if (plannerStore.durableState.value?.scheduleCompositionProfile == profile) return true
        return try {
            val acknowledged = plannerStore.updateScheduleCompositionProfileDurably(profile)
                ?.awaitDurable() == true
            acknowledged ||
                plannerStore.durableState.value?.scheduleCompositionProfile == profile
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            plannerStore.durableState.value?.scheduleCompositionProfile == profile
        }
    }

    fun acknowledge() {
        if (!mutableState.value.isSaving) {
            ScheduleCompositionProfileDraftMemory.clear()
            mutableState.value = ScheduleCompositionProfileUpdateState()
        }
    }
}

/**
 * Bounded process-only handoff for configuration recreation. SavedState receives only the opaque
 * token; unsaved availability hours and weights never enter a Bundle or disk-backed saved state.
 */
internal object ScheduleCompositionProfileDraftMemory {
    private const val MAX_RETAINED_DRAFTS = 4
    private val entriesByToken = linkedMapOf<String, DraftEntry>()

    @Synchronized
    fun retain(
        baseline: ScheduleCompositionProfileSnapshot,
        nextValues: List<String>,
    ): String {
        val nextToken = UUID.randomUUID().toString()
        entriesByToken[nextToken] = DraftEntry(baseline, nextValues.toList())
        while (entriesByToken.size > MAX_RETAINED_DRAFTS) {
            entriesByToken.remove(entriesByToken.keys.first())
        }
        return nextToken
    }

    @Synchronized
    fun restore(
        expectedToken: String,
        expectedBaseline: ScheduleCompositionProfileSnapshot,
    ): List<String>? = entriesByToken[expectedToken]
        ?.takeIf { it.baseline == expectedBaseline }
        ?.values
        ?.toList()

    @Synchronized
    fun clear() {
        entriesByToken.clear()
    }

    private data class DraftEntry(
        val baseline: ScheduleCompositionProfileSnapshot,
        val values: List<String>,
    )
}
