package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.ScheduleLocalTimeWindow
import com.greengolddog.dayweave.model.ScheduleWeekday
import com.greengolddog.dayweave.model.normalizedScheduleTimeZoneName
import com.greengolddog.dayweave.network.FixedScheduleBlockRequest
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.time.DateTimeException
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.time.temporal.ChronoUnit
import java.util.UUID

internal class ScheduleProfileExpansionException(cause: Throwable? = null) :
    IllegalArgumentException("Invalid scheduling profile expansion", cause)

internal data class ExpandedScheduleCompositionProfile(
    val planningZone: ZoneId,
    val availability: List<ScheduleAvailabilityRequest>,
    val fixedBlocks: List<FixedScheduleBlockRequest>,
)

internal fun ScheduleCompositionProfileSnapshot.compositionZone(fallbackZone: ZoneId): ZoneId {
    if (!hasValidShape()) throw ScheduleProfileExpansionException()
    return try {
        ZoneId.of(timezoneName?.normalizedScheduleTimeZoneName() ?: fallbackZone.id)
    } catch (error: DateTimeException) {
        throw ScheduleProfileExpansionException(error)
    }
}

/**
 * Expands encrypted local-wall-clock policy without consulting locale or ambient timezone state.
 * Repeated-time choices mirror macOS: availability is conservative, while protection is broad.
 */
internal fun ScheduleCompositionProfileSnapshot.expandForComposition(
    fallbackZone: ZoneId,
    horizonStart: Instant,
    horizonEnd: Instant,
): ExpandedScheduleCompositionProfile {
    if (!hasValidShape() || horizonStart >= horizonEnd) {
        throw ScheduleProfileExpansionException()
    }
    val planningZone = compositionZone(fallbackZone)
    val firstDate = horizonStart.atZone(planningZone).toLocalDate()
    val lastDate = horizonEnd.minusNanos(1).atZone(planningZone).toLocalDate()
    val dayCount = try {
        Math.addExact(ChronoUnit.DAYS.between(firstDate, lastDate).toInt(), 1)
    } catch (error: ArithmeticException) {
        throw ScheduleProfileExpansionException(error)
    }
    if (dayCount !in 1..MAX_EXPANDED_LOCAL_DAYS) {
        throw ScheduleProfileExpansionException()
    }

    if (!usesWeeklySchedule) {
        val windows = (0 until dayCount).mapNotNull { offset ->
            expandAvailabilityWindow(
                date = firstDate.plusDays(offset.toLong()),
                window = ScheduleLocalTimeWindow(dayStartMinute, dayEndMinute),
                zone = planningZone,
                horizonStart = horizonStart,
                horizonEnd = horizonEnd,
                legacyEndOfDay = dayEndMinute == MINUTES_PER_DAY,
            )
        }
        return ExpandedScheduleCompositionProfile(planningZone, windows, emptyList())
    }

    val availabilityByDay = requireNotNull(availability).associateBy { it.weekday }
    val protectedByDay = requireNotNull(protectedTime).associateBy { it.weekday }
    val exactSleep = requireNotNull(sleep)
    val expandedAvailability = buildList {
        repeat(dayCount) { offset ->
            val date = firstDate.plusDays(offset.toLong())
            val day = availabilityByDay.getValue(ScheduleWeekday.from(date.dayOfWeek))
            if (day.isEnabled) {
                day.windows.forEach { window ->
                    expandAvailabilityWindow(
                        date = date,
                        window = window,
                        zone = planningZone,
                        horizonStart = horizonStart,
                        horizonEnd = horizonEnd,
                    )?.let(::add)
                }
            }
        }
    }

    val expandedFixedBlocks = buildList {
        for (offset in -1 until dayCount) {
            val startDate = firstDate.plusDays(offset.toLong())
            val endDate = startDate.plusDays(1)
            val start = wallInstant(
                startDate,
                exactSleep.startMinute,
                planningZone,
                RepeatedTimePolicy.FIRST,
            )
            val end = wallInstant(
                endDate,
                exactSleep.endMinute,
                planningZone,
                RepeatedTimePolicy.LAST,
            )
            if (start >= end) throw ScheduleProfileExpansionException()
            if (end > horizonStart && start < horizonEnd) {
                add(
                    FixedScheduleBlockRequest(
                        id = scheduleProfileFixedBlockId(
                            kind = "sleep",
                            timezoneName = planningZone.id,
                            anchorDate = startDate,
                            startMinute = exactSleep.startMinute,
                            endMinute = exactSleep.endMinute,
                        ),
                        isSensitive = true,
                        title = "Sleep",
                        start = start.toString(),
                        end = end.toString(),
                        source = "sleep",
                    ),
                )
            }
        }
        repeat(dayCount) { offset ->
            val date = firstDate.plusDays(offset.toLong())
            val day = protectedByDay.getValue(ScheduleWeekday.from(date.dayOfWeek))
            if (day.isEnabled) {
                day.windows.forEach { window ->
                    val start = wallInstant(
                        date,
                        window.startMinute,
                        planningZone,
                        RepeatedTimePolicy.FIRST,
                    )
                    val end = wallInstant(
                        date,
                        window.endMinute,
                        planningZone,
                        RepeatedTimePolicy.LAST,
                    )
                    if (start >= end) throw ScheduleProfileExpansionException()
                    if (end > horizonStart && start < horizonEnd) {
                        add(
                            FixedScheduleBlockRequest(
                                id = scheduleProfileFixedBlockId(
                                    kind = "protected_time",
                                    timezoneName = planningZone.id,
                                    anchorDate = date,
                                    startMinute = window.startMinute,
                                    endMinute = window.endMinute,
                                ),
                                isSensitive = true,
                                title = "Protected time",
                                start = start.toString(),
                                end = end.toString(),
                                source = "protected_time",
                            ),
                        )
                    }
                }
            }
        }
    }.sortedWith(compareBy({ Instant.parse(it.start) }, { Instant.parse(it.end) }, { it.id }))
    if (expandedFixedBlocks.map(FixedScheduleBlockRequest::id).toSet().size != expandedFixedBlocks.size) {
        throw ScheduleProfileExpansionException()
    }
    return ExpandedScheduleCompositionProfile(
        planningZone = planningZone,
        availability = expandedAvailability,
        fixedBlocks = expandedFixedBlocks,
    )
}

private fun expandAvailabilityWindow(
    date: LocalDate,
    window: ScheduleLocalTimeWindow,
    zone: ZoneId,
    horizonStart: Instant,
    horizonEnd: Instant,
    legacyEndOfDay: Boolean = false,
): ScheduleAvailabilityRequest? {
    val start = wallInstant(date, window.startMinute, zone, RepeatedTimePolicy.LAST)
    val end = if (legacyEndOfDay) {
        wallInstant(date.plusDays(1), 0, zone, RepeatedTimePolicy.LAST)
    } else {
        wallInstant(date, window.endMinute, zone, RepeatedTimePolicy.FIRST)
    }
    if (start >= end) throw ScheduleProfileExpansionException()
    val clippedStart = maxOf(start, horizonStart)
    val clippedEnd = minOf(end, horizonEnd)
    return if (clippedStart < clippedEnd) {
        ScheduleAvailabilityRequest(start = clippedStart.toString(), end = clippedEnd.toString())
    } else {
        null
    }
}

private enum class RepeatedTimePolicy { FIRST, LAST }

private fun wallInstant(
    date: LocalDate,
    minute: Int,
    zone: ZoneId,
    repeatedTimePolicy: RepeatedTimePolicy,
): Instant {
    if (minute !in 0 until MINUTES_PER_DAY) throw ScheduleProfileExpansionException()
    val localDateTime = LocalDateTime.of(date, LocalTime.of(minute / 60, minute % 60))
    val offsets = zone.rules.getValidOffsets(localDateTime)
    if (offsets.isEmpty()) throw ScheduleProfileExpansionException()
    val offset: ZoneOffset = when (repeatedTimePolicy) {
        RepeatedTimePolicy.FIRST -> offsets.first()
        RepeatedTimePolicy.LAST -> offsets.last()
    }
    return ZonedDateTime.ofStrict(localDateTime, offset, zone).toInstant()
}

private fun scheduleProfileFixedBlockId(
    kind: String,
    timezoneName: String,
    anchorDate: LocalDate,
    startMinute: Int,
    endMinute: Int,
): String {
    val identity = listOf(
        "dayweave-schedule-profile-v1",
        kind,
        timezoneName,
        anchorDate.format(DateTimeFormatter.ISO_LOCAL_DATE),
        startMinute.toString(),
        endMinute.toString(),
    ).joinToString("|")
    val bytes = MessageDigest.getInstance("SHA-256")
        .digest(identity.toByteArray(Charsets.UTF_8))
        .copyOfRange(0, 16)
    bytes[6] = ((bytes[6].toInt() and 0x0f) or 0x80).toByte()
    bytes[8] = ((bytes[8].toInt() and 0x3f) or 0x80).toByte()
    val buffer = ByteBuffer.wrap(bytes)
    return UUID(buffer.long, buffer.long).toString()
}

private const val MINUTES_PER_DAY = 24 * 60
private const val MAX_EXPANDED_LOCAL_DAYS = 92
