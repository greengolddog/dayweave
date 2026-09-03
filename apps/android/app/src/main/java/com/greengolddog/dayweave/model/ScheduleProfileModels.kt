package com.greengolddog.dayweave.model

import java.time.DayOfWeek
import java.time.DateTimeException
import java.time.ZoneId
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** ISO-ordered weekday used by the encrypted scheduling profile. */
@Serializable
enum class ScheduleWeekday(val isoDayNumber: Int) {
    @SerialName("monday")
    MONDAY(1),

    @SerialName("tuesday")
    TUESDAY(2),

    @SerialName("wednesday")
    WEDNESDAY(3),

    @SerialName("thursday")
    THURSDAY(4),

    @SerialName("friday")
    FRIDAY(5),

    @SerialName("saturday")
    SATURDAY(6),

    @SerialName("sunday")
    SUNDAY(7),
    ;

    companion object {
        fun from(dayOfWeek: DayOfWeek): ScheduleWeekday = entries[dayOfWeek.value - 1]
    }
}

/** Non-overnight local wall-clock window. Rich profiles intentionally do not use 24:00. */
@Serializable
data class ScheduleLocalTimeWindow(
    val startMinute: Int,
    val endMinute: Int,
) {
    val durationMinutes: Int get() = endMinute - startMinute

    fun hasValidShape(): Boolean =
        startMinute in 0 until MINUTES_PER_DAY &&
            endMinute in 1 until MINUTES_PER_DAY &&
            startMinute < endMinute

    companion object {
        const val MINUTES_PER_DAY = 24 * 60
    }
}

/** One overnight sleep interval, anchored at its evening start. */
@Serializable
data class ScheduleSleepInterval(
    val startMinute: Int,
    val endMinute: Int,
) {
    val durationMinutes: Int
        get() = ScheduleLocalTimeWindow.MINUTES_PER_DAY - startMinute + endMinute

    fun hasValidShape(): Boolean =
        startMinute in 0 until ScheduleLocalTimeWindow.MINUTES_PER_DAY &&
            endMinute in 0 until ScheduleLocalTimeWindow.MINUTES_PER_DAY &&
            startMinute > endMinute
}

@Serializable
data class ScheduleAvailabilityDay(
    val weekday: ScheduleWeekday,
    val isEnabled: Boolean,
    val windows: List<ScheduleLocalTimeWindow>,
) {
    fun hasValidShape(): Boolean =
        windows.size <= MAX_WINDOWS &&
            (if (isEnabled) windows.isNotEmpty() else windows.isEmpty()) &&
            windows.hasCanonicalNonOverlappingShape()

    companion object {
        const val MAX_WINDOWS = 8
    }
}

@Serializable
data class ScheduleProtectedDay(
    val weekday: ScheduleWeekday,
    val isEnabled: Boolean,
    val windows: List<ScheduleLocalTimeWindow>,
) {
    fun hasValidShape(): Boolean =
        windows.size <= MAX_WINDOWS &&
            (if (isEnabled) windows.isNotEmpty() else windows.isEmpty()) &&
            windows.sumOf(ScheduleLocalTimeWindow::durationMinutes) <=
            ScheduleCompositionProfileSnapshot.MAX_PROTECTED_MINUTES_PER_DAY &&
            windows.hasCanonicalNonOverlappingShape()

    companion object {
        const val MAX_WINDOWS = 8
    }
}

internal fun List<ScheduleLocalTimeWindow>.hasCanonicalNonOverlappingShape(): Boolean {
    if (any { !it.hasValidShape() }) return false
    for (index in indices) {
        if (index > 0) {
            val previous = this[index - 1]
            val current = this[index]
            if (
                previous.startMinute > current.startMinute ||
                previous.startMinute == current.startMinute &&
                previous.endMinute > current.endMinute ||
                previous.endMinute > current.startMinute
            ) {
                return false
            }
        }
    }
    return true
}

internal fun String.isKnownScheduleTimeZone(): Boolean {
    if (isBlank() || length > ScheduleCompositionProfileSnapshot.MAX_TIMEZONE_NAME_BYTES) {
        return false
    }
    if (toByteArray(Charsets.UTF_8).size > ScheduleCompositionProfileSnapshot.MAX_TIMEZONE_NAME_BYTES) {
        return false
    }
    if (any(Char::isISOControl)) return false
    val normalized = normalizedScheduleTimeZoneName()
    if (normalized != "UTC" && normalized !in SCHEDULE_TIME_ZONE_IDS) return false
    return try {
        ZoneId.of(normalized)
        true
    } catch (_: DateTimeException) {
        false
    }
}

fun String.normalizedScheduleTimeZoneName(): String = if (this == "GMT") "UTC" else this

fun currentScheduleTimeZoneName(): String =
    ZoneId.systemDefault().id.normalizedScheduleTimeZoneName()

private val SCHEDULE_TIME_ZONE_IDS: Set<String> by lazy(ZoneId::getAvailableZoneIds)
