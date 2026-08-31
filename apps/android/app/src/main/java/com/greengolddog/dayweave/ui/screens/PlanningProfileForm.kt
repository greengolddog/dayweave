package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.state.scheduleCompositionProfileEditBlocker

internal const val MIN_SCHEDULER_WEIGHT = 0
internal const val MAX_SCHEDULER_WEIGHT = 1_000_000
internal const val MIN_SLOT_GRANULARITY_MINUTES = 1
internal const val MAX_SLOT_GRANULARITY_MINUTES = 60
internal const val PLANNING_PROFILE_ACTION_BUSY_MESSAGE =
    "Wait for the current planner action to finish."

internal fun planningProfileEditBlockedMessage(
    state: DayWeaveUiState,
    canonicalActionBusy: Boolean,
): String? = state.scheduleCompositionProfileEditBlocker()?.message
    ?: PLANNING_PROFILE_ACTION_BUSY_MESSAGE.takeIf { canonicalActionBusy }

internal data class PlanningProfileForm(
    val startHour: String,
    val startMinute: String,
    val endHour: String,
    val endMinute: String,
    val slotGranularityMinutes: Int,
    val stabilityWeight: String,
    val defaultSoftWeight: String,
) {
    fun validate(): PlanningProfileFormValidation {
        val start = parseTime(startHour, startMinute, allowEndOfDay = false)
        val end = parseTime(endHour, endMinute, allowEndOfDay = true)
        val stability = stabilityWeight.toIntOrNull()
        val soft = defaultSoftWeight.toIntOrNull()
        val startError = if (start == null) "Use a 24-hour time from 00:00 to 23:59." else null
        val endError = when {
            end == null -> "Use a 24-hour time from 00:01 to 24:00."
            start != null && end <= start -> "End must be later than start."
            else -> null
        }
        val granularityError = if (
            slotGranularityMinutes !in
            MIN_SLOT_GRANULARITY_MINUTES..MAX_SLOT_GRANULARITY_MINUTES
        ) {
            "Choose a slot size from 1 to 60 minutes."
        } else {
            null
        }
        val stabilityError = if (
            stability == null || stability !in MIN_SCHEDULER_WEIGHT..MAX_SCHEDULER_WEIGHT
        ) {
            "Use a whole number from 0 to 1,000,000."
        } else {
            null
        }
        val softError = if (
            soft == null || soft !in MIN_SCHEDULER_WEIGHT..MAX_SCHEDULER_WEIGHT
        ) {
            "Use a whole number from 0 to 1,000,000."
        } else {
            null
        }
        val profile = if (
            startError == null && endError == null && granularityError == null &&
            stabilityError == null && softError == null
        ) {
            ScheduleCompositionProfileSnapshot(
                dayStartMinute = requireNotNull(start),
                dayEndMinute = requireNotNull(end),
                slotGranularityMinutes = slotGranularityMinutes,
                stabilityWeight = requireNotNull(stability),
                defaultSoftWeight = requireNotNull(soft),
            ).takeIf(ScheduleCompositionProfileSnapshot::hasValidShape)
        } else {
            null
        }
        return PlanningProfileFormValidation(
            profile = profile,
            startError = startError,
            endError = endError,
            granularityError = granularityError,
            stabilityWeightError = stabilityError,
            defaultSoftWeightError = softError,
        )
    }

    companion object {
        fun from(profile: ScheduleCompositionProfileSnapshot): PlanningProfileForm {
            require(profile.hasValidShape())
            return PlanningProfileForm(
                startHour = (profile.dayStartMinute / 60).twoDigits(),
                startMinute = (profile.dayStartMinute % 60).twoDigits(),
                endHour = (profile.dayEndMinute / 60).twoDigits(),
                endMinute = (profile.dayEndMinute % 60).twoDigits(),
                slotGranularityMinutes = profile.slotGranularityMinutes,
                stabilityWeight = profile.stabilityWeight.toString(),
                defaultSoftWeight = profile.defaultSoftWeight.toString(),
            )
        }

        private fun parseTime(hour: String, minute: String, allowEndOfDay: Boolean): Int? {
            if (hour.isEmpty() || minute.isEmpty() || hour.length > 2 || minute.length > 2) {
                return null
            }
            val parsedHour = hour.toIntOrNull() ?: return null
            val parsedMinute = minute.toIntOrNull() ?: return null
            if (parsedMinute !in 0..59) return null
            if (allowEndOfDay && parsedHour == 24) {
                return if (parsedMinute == 0) 24 * 60 else null
            }
            if (parsedHour !in 0..23) return null
            val total = parsedHour * 60 + parsedMinute
            return if (allowEndOfDay && total == 0) null else total
        }

        private fun Int.twoDigits(): String = toString().padStart(2, '0')
    }
}

internal data class PlanningProfileFormValidation(
    val profile: ScheduleCompositionProfileSnapshot?,
    val startError: String?,
    val endError: String?,
    val granularityError: String?,
    val stabilityWeightError: String?,
    val defaultSoftWeightError: String?,
) {
    val isValid: Boolean
        get() = profile != null
}

internal fun formatPlanningProfileMinute(minuteOfDay: Int): String {
    require(minuteOfDay in 0..24 * 60)
    val hour = (minuteOfDay / 60).toString().padStart(2, '0')
    val minute = (minuteOfDay % 60).toString().padStart(2, '0')
    return "$hour:$minute"
}

internal fun sanitizePlanningTimePart(raw: String): String =
    raw.filter(Char::isDigit).take(2)

internal fun sanitizePlanningWeight(raw: String): String =
    raw.filter(Char::isDigit).take(MAX_SCHEDULER_WEIGHT.toString().length)

/** Primitive in-memory snapshot used solely during same-process configuration recreation. */
internal fun PlanningProfileForm.toDraftMemoryValues(): List<String> = listOf(
    startHour,
    startMinute,
    endHour,
    endMinute,
    slotGranularityMinutes.toString(),
    stabilityWeight,
    defaultSoftWeight,
)

internal fun planningProfileFormFromDraftMemoryValues(values: List<String>): PlanningProfileForm? {
    if (values.size != 7) return null
    return PlanningProfileForm(
        startHour = values[0],
        startMinute = values[1],
        endHour = values[2],
        endMinute = values[3],
        slotGranularityMinutes = values[4].toIntOrNull() ?: return null,
        stabilityWeight = values[5],
        defaultSoftWeight = values[6],
    )
}
