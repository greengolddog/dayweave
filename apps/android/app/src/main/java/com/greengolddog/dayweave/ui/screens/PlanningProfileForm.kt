package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ScheduleAvailabilityDay
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.ScheduleLocalTimeWindow
import com.greengolddog.dayweave.model.ScheduleProtectedDay
import com.greengolddog.dayweave.model.ScheduleSleepInterval
import com.greengolddog.dayweave.model.ScheduleWeekday
import com.greengolddog.dayweave.model.currentScheduleTimeZoneName
import com.greengolddog.dayweave.model.isKnownScheduleTimeZone
import com.greengolddog.dayweave.model.normalizedScheduleTimeZoneName
import com.greengolddog.dayweave.state.scheduleCompositionProfileEditBlocker
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

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
    val firmHorizonDays: Int,
    val slotGranularityMinutes: Int,
    val stabilityWeight: String,
    val defaultSoftWeight: String,
    val useWeeklySchedule: Boolean = false,
    val timezoneName: String = currentScheduleTimeZoneName(),
    val sleepStart: String = "23:00",
    val sleepEnd: String = "06:00",
    val availabilityDays: List<PlanningDayForm> = emptyList(),
    val protectedDays: List<PlanningDayForm> = emptyList(),
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
        val horizonError = if (
            firmHorizonDays !in ScheduleCompositionProfileSnapshot.MIN_FIRM_HORIZON_DAYS..
            ScheduleCompositionProfileSnapshot.MAX_FIRM_HORIZON_DAYS
        ) {
            "Choose a firm horizon from " +
                "${ScheduleCompositionProfileSnapshot.MIN_FIRM_HORIZON_DAYS} to " +
                "${ScheduleCompositionProfileSnapshot.MAX_FIRM_HORIZON_DAYS} days."
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
        val baseFieldsValid =
            startError == null && endError == null && horizonError == null &&
            granularityError == null &&
            stabilityError == null && softError == null
        val timezoneError = if (
            useWeeklySchedule && !timezoneName.trim().normalizedScheduleTimeZoneName()
                .isKnownScheduleTimeZone()
        ) {
            "Choose a recognized IANA timezone, such as Europe/Paris."
        } else {
            null
        }
        val parsedSleepStart = parseClockTime(sleepStart)
        val parsedSleepEnd = parseClockTime(sleepEnd)
        val sleepError = when {
            !useWeeklySchedule -> null
            parsedSleepStart == null || parsedSleepEnd == null ->
                "Use 24-hour sleep times from 00:00 to 23:59."
            parsedSleepStart <= parsedSleepEnd ->
                "Sleep must be one overnight interval; bedtime must be later than wake time."
            else -> null
        }
        val weeklyDraft = if (
            useWeeklySchedule && timezoneError == null && sleepError == null
        ) {
            validatedWeeklyDraft(
                sleep = ScheduleSleepInterval(
                    startMinute = requireNotNull(parsedSleepStart),
                    endMinute = requireNotNull(parsedSleepEnd),
                ),
            )
        } else {
            null
        }
        val profile = if (baseFieldsValid && !useWeeklySchedule) {
            ScheduleCompositionProfileSnapshot(
                dayStartMinute = requireNotNull(start),
                dayEndMinute = requireNotNull(end),
                firmHorizonDays = firmHorizonDays,
                slotGranularityMinutes = slotGranularityMinutes,
                stabilityWeight = requireNotNull(stability),
                defaultSoftWeight = requireNotNull(soft),
            ).takeIf(ScheduleCompositionProfileSnapshot::hasValidShape)
        } else if (
            baseFieldsValid && timezoneError == null && sleepError == null && weeklyDraft != null
        ) {
            val workWindows = weeklyDraft.availability.flatMap(ScheduleAvailabilityDay::windows)
            val compatibilityStart = workWindows.minOfOrNull(ScheduleLocalTimeWindow::startMinute)
                ?: weeklyDraft.sleep.endMinute
            val compatibilityEnd = workWindows.maxOfOrNull(ScheduleLocalTimeWindow::endMinute)
                ?: weeklyDraft.sleep.startMinute
            ScheduleCompositionProfileSnapshot(
                dayStartMinute = compatibilityStart,
                dayEndMinute = compatibilityEnd,
                firmHorizonDays = firmHorizonDays,
                slotGranularityMinutes = slotGranularityMinutes,
                stabilityWeight = requireNotNull(stability),
                defaultSoftWeight = requireNotNull(soft),
                timezoneName = timezoneName.trim().normalizedScheduleTimeZoneName(),
                availability = weeklyDraft.availability,
                sleep = weeklyDraft.sleep,
                protectedTime = weeklyDraft.protected,
            ).takeIf(ScheduleCompositionProfileSnapshot::hasValidShape)
        } else {
            null
        }
        val weeklyScheduleError = if (
            useWeeklySchedule && timezoneError == null && sleepError == null && profile == null
        ) {
            "Each enabled day needs 1–8 ordered work windows. Protected time must be " +
                "non-overlapping, stay within waking hours, and total at most 8 hours per day."
        } else {
            null
        }
        return PlanningProfileFormValidation(
            profile = profile,
            startError = startError,
            endError = endError,
            firmHorizonError = horizonError,
            granularityError = granularityError,
            stabilityWeightError = stabilityError,
            defaultSoftWeightError = softError,
            timezoneError = timezoneError,
            sleepError = sleepError,
            weeklyScheduleError = weeklyScheduleError,
        )
    }

    private fun validatedWeeklyDraft(sleep: ScheduleSleepInterval): ValidatedWeeklyDraft? {
        if (
            availabilityDays.map(PlanningDayForm::weekday) != ScheduleWeekday.entries ||
            protectedDays.map(PlanningDayForm::weekday) != ScheduleWeekday.entries
        ) {
            return null
        }
        val availability = availabilityDays.map { it.toAvailabilityDay() ?: return null }
        val protected = protectedDays.map { it.toProtectedDay() ?: return null }
        return ValidatedWeeklyDraft(availability, sleep, protected)
    }

    companion object {
        fun from(profile: ScheduleCompositionProfileSnapshot): PlanningProfileForm {
            require(profile.hasValidShape())
            val weekly = if (profile.usesWeeklySchedule) {
                profile
            } else {
                profile.upgradedToWeeklySchedule()
            }
            return PlanningProfileForm(
                startHour = (profile.dayStartMinute / 60).twoDigits(),
                startMinute = (profile.dayStartMinute % 60).twoDigits(),
                endHour = (profile.dayEndMinute / 60).twoDigits(),
                endMinute = (profile.dayEndMinute % 60).twoDigits(),
                firmHorizonDays = profile.firmHorizonDays,
                slotGranularityMinutes = profile.slotGranularityMinutes,
                stabilityWeight = profile.stabilityWeight.toString(),
                defaultSoftWeight = profile.defaultSoftWeight.toString(),
                useWeeklySchedule = profile.usesWeeklySchedule,
                timezoneName = weekly?.timezoneName ?: currentScheduleTimeZoneName(),
                sleepStart = weekly?.sleep?.startMinute?.let(::formatPlanningClockMinute)
                    ?: "23:00",
                sleepEnd = weekly?.sleep?.endMinute?.let(::formatPlanningClockMinute)
                    ?: "06:00",
                availabilityDays = weekly?.availability?.map(PlanningDayForm::fromAvailability)
                    ?: defaultPlanningDays(profile.dayStartMinute, minOf(profile.dayEndMinute, 1439)),
                protectedDays = weekly?.protectedTime?.map(PlanningDayForm::fromProtected)
                    ?: ScheduleWeekday.entries.map { PlanningDayForm(it, false, emptyList()) },
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

        private fun defaultPlanningDays(start: Int, end: Int): List<PlanningDayForm> =
            ScheduleWeekday.entries.map { weekday ->
                PlanningDayForm(
                    weekday = weekday,
                    isEnabled = start < end,
                    windows = if (start < end) {
                        listOf(PlanningWindowForm.from(ScheduleLocalTimeWindow(start, end)))
                    } else {
                        emptyList()
                    },
                )
            }

        private fun Int.twoDigits(): String = toString().padStart(2, '0')
    }
}

@Serializable
internal data class PlanningWindowForm(
    val start: String,
    val end: String,
) {
    fun parsed(): ScheduleLocalTimeWindow? {
        val parsedStart = parseClockTime(start) ?: return null
        val parsedEnd = parseClockTime(end) ?: return null
        return ScheduleLocalTimeWindow(parsedStart, parsedEnd)
            .takeIf(ScheduleLocalTimeWindow::hasValidShape)
    }

    companion object {
        fun from(window: ScheduleLocalTimeWindow): PlanningWindowForm = PlanningWindowForm(
            start = formatPlanningClockMinute(window.startMinute),
            end = formatPlanningClockMinute(window.endMinute),
        )
    }
}

@Serializable
internal data class PlanningDayForm(
    val weekday: ScheduleWeekday,
    val isEnabled: Boolean,
    val windows: List<PlanningWindowForm>,
) {
    fun toAvailabilityDay(): ScheduleAvailabilityDay? {
        val parsed = windows.map { it.parsed() ?: return null }
        return ScheduleAvailabilityDay(weekday, isEnabled, parsed)
            .takeIf(ScheduleAvailabilityDay::hasValidShape)
    }

    fun toProtectedDay(): ScheduleProtectedDay? {
        val parsed = windows.map { it.parsed() ?: return null }
        return ScheduleProtectedDay(weekday, isEnabled, parsed)
            .takeIf(ScheduleProtectedDay::hasValidShape)
    }

    companion object {
        fun fromAvailability(day: ScheduleAvailabilityDay): PlanningDayForm = PlanningDayForm(
            weekday = day.weekday,
            isEnabled = day.isEnabled,
            windows = day.windows.map(PlanningWindowForm::from),
        )

        fun fromProtected(day: ScheduleProtectedDay): PlanningDayForm = PlanningDayForm(
            weekday = day.weekday,
            isEnabled = day.isEnabled,
            windows = day.windows.map(PlanningWindowForm::from),
        )
    }
}

private data class ValidatedWeeklyDraft(
    val availability: List<ScheduleAvailabilityDay>,
    val sleep: ScheduleSleepInterval,
    val protected: List<ScheduleProtectedDay>,
)

internal data class PlanningProfileFormValidation(
    val profile: ScheduleCompositionProfileSnapshot?,
    val startError: String?,
    val endError: String?,
    val firmHorizonError: String?,
    val granularityError: String?,
    val stabilityWeightError: String?,
    val defaultSoftWeightError: String?,
    val timezoneError: String?,
    val sleepError: String?,
    val weeklyScheduleError: String?,
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

internal fun formatPlanningClockMinute(minuteOfDay: Int): String {
    require(minuteOfDay in 0 until 24 * 60)
    return "${(minuteOfDay / 60).toString().padStart(2, '0')}:" +
        (minuteOfDay % 60).toString().padStart(2, '0')
}

internal fun parseClockTime(raw: String): Int? {
    if (!raw.matches(Regex("(?:[01]\\d|2[0-3]):[0-5]\\d"))) return null
    return raw.substring(0, 2).toInt() * 60 + raw.substring(3, 5).toInt()
}

internal fun sanitizePlanningTimePart(raw: String): String =
    raw.filter(Char::isDigit).take(2)

internal fun sanitizePlanningWeight(raw: String): String =
    raw.filter(Char::isDigit).take(MAX_SCHEDULER_WEIGHT.toString().length)

internal fun sanitizePlanningClock(raw: String): String =
    raw.filter { it.isDigit() || it == ':' }.take(5)

/** Primitive in-memory snapshot used solely during same-process configuration recreation. */
internal fun PlanningProfileForm.toDraftMemoryValues(): List<String> = listOf(
    startHour,
    startMinute,
    endHour,
    endMinute,
    firmHorizonDays.toString(),
    slotGranularityMinutes.toString(),
    stabilityWeight,
    defaultSoftWeight,
    PLANNING_PROFILE_DRAFT_VERSION,
    PLANNING_PROFILE_DRAFT_JSON.encodeToString(
        RichPlanningProfileDraft.serializer(),
        RichPlanningProfileDraft(
            useWeeklySchedule = useWeeklySchedule,
            timezoneName = timezoneName,
            sleepStart = sleepStart,
            sleepEnd = sleepEnd,
            availabilityDays = availabilityDays,
            protectedDays = protectedDays,
        ),
    ),
)

internal fun planningProfileFormFromDraftMemoryValues(values: List<String>): PlanningProfileForm? {
    if (values.size !in setOf(8, 10)) return null
    val legacy = PlanningProfileForm(
        startHour = values[0],
        startMinute = values[1],
        endHour = values[2],
        endMinute = values[3],
        firmHorizonDays = values[4].toIntOrNull() ?: return null,
        slotGranularityMinutes = values[5].toIntOrNull() ?: return null,
        stabilityWeight = values[6],
        defaultSoftWeight = values[7],
    )
    if (values.size == 8) return legacy
    if (values[8] != PLANNING_PROFILE_DRAFT_VERSION) return null
    val rich = runCatching {
        PLANNING_PROFILE_DRAFT_JSON.decodeFromString(
            RichPlanningProfileDraft.serializer(),
            values[9],
        )
    }.getOrNull() ?: return null
    return legacy.copy(
        useWeeklySchedule = rich.useWeeklySchedule,
        timezoneName = rich.timezoneName,
        sleepStart = rich.sleepStart,
        sleepEnd = rich.sleepEnd,
        availabilityDays = rich.availabilityDays,
        protectedDays = rich.protectedDays,
    )
}

@Serializable
private data class RichPlanningProfileDraft(
    val useWeeklySchedule: Boolean,
    val timezoneName: String,
    val sleepStart: String,
    val sleepEnd: String,
    val availabilityDays: List<PlanningDayForm>,
    val protectedDays: List<PlanningDayForm>,
)

private const val PLANNING_PROFILE_DRAFT_VERSION = "weekly-v1"
private val PLANNING_PROFILE_DRAFT_JSON = Json {
    encodeDefaults = true
    ignoreUnknownKeys = false
}
