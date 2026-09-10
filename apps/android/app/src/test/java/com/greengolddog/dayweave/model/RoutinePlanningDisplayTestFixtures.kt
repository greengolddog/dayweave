package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.scheduler.RustScheduleComposer

internal const val PLANNING_DISPLAY_NOW = "2026-09-10T10:00:01.123456Z"
internal fun planningDisplayCapsule(state: DayWeaveUiState = planningTestReadyState()) = RoutinePlanningInputCapsule.create(
    state, " \n" + ROUTINE_PLANNING_JSON.encodeToString(planningTestRequest()) + "\n", planningTestWitness(), ROUTINE_NOW)
internal fun planningDisplayState(): DayWeaveUiState {
    val state = planningTestReadyState()
    return state.copy(routinePlanningInputCapsule = planningDisplayCapsule(state))
}
internal suspend fun planningDisplayComposition(capsule: RoutinePlanningInputCapsule) =
    RustScheduleComposer(bridge = { planningTestHelperResponse(capsule.witness) }).compose(capsule.canonicalItems, capsule.witness)
internal suspend fun planningDisplaySnapshot(capsule: RoutinePlanningInputCapsule) =
    RoutinePlanningDisplaySnapshot.create(capsule, planningDisplayComposition(capsule), PLANNING_DISPLAY_NOW)
