package com.greengolddog.dayweave.model

import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.time.Instant

/**
 * Content-free identity for exactly one authoritative timed pause.
 *
 * The raw session id is used only transiently while deriving [digest]. WorkManager and Android
 * notification surfaces receive the domain-separated digest, never planner content or raw ids.
 */
internal data class TimedBreakNotificationIdentity(
    val executionRevision: Long,
    val sessionId: String,
    val sessionRevision: Long,
    val deadlineEpochMillis: Long,
) {
    val digest: String by lazy(LazyThreadSafetyMode.NONE) {
        val canonical = buildString {
            append("dayweave.timed-break-notification.v1\n")
            append("execution_revision=").append(executionRevision).append('\n')
            append("session_id=").append(sessionId).append('\n')
            append("session_revision=").append(sessionRevision).append('\n')
            append("deadline_epoch_millis=").append(deadlineEpochMillis).append('\n')
        }
        val bytes = MessageDigest.getInstance("SHA-256")
            .digest(canonical.toByteArray(StandardCharsets.UTF_8))
        "sha256:" + bytes.joinToString(separator = "") { byte -> "%02x".format(byte) }
    }
}

/** Returns a notification identity only while both canonical truth and its local projection agree. */
internal fun DayWeaveUiState.authoritativeTimedBreakNotificationIdentity():
    TimedBreakNotificationIdentity? {
    if (pendingExecutionCommand?.commandType in TIMED_BREAK_RESOLVING_COMMAND_TYPES) return null
    val execution = canonicalExecutionSession ?: return null
    if (execution.status != "paused") return null
    val deadline = execution.pauseUntil?.let { raw ->
        runCatching { Instant.parse(raw) }.getOrNull()
    } ?: return null
    val local = activeSession ?: return null
    if (
        local.canonicalExecutionSessionId != execution.id ||
        local.itemId.isBlank() ||
        !local.isPaused ||
        local.pauseUntilEpochMillis != deadline.toEpochMilli() ||
        canonicalExecutionRevision <= 0L ||
        execution.revision <= 0L ||
        execution.revision > canonicalExecutionRevision
    ) {
        return null
    }
    return TimedBreakNotificationIdentity(
        executionRevision = canonicalExecutionRevision,
        sessionId = execution.id,
        sessionRevision = execution.revision,
        deadlineEpochMillis = deadline.toEpochMilli(),
    )
}

/** A Keep-paused acknowledgement suppresses only the exact revision/deadline it reviewed. */
internal fun DayWeaveUiState.unacknowledgedTimedBreakNotificationIdentity():
    TimedBreakNotificationIdentity? = authoritativeTimedBreakNotificationIdentity()?.takeUnless {
    acknowledgedBreakEndDigest == it.digest
}

internal fun isTimedBreakNotificationDigest(value: String?): Boolean =
    value?.matches(TIMED_BREAK_NOTIFICATION_DIGEST_PATTERN) == true

/** Malformed legacy/corrupt evidence cannot suppress a future notification. */
internal fun DayWeaveUiState.withInvalidTimedBreakNotificationAttemptAbandoned(): DayWeaveUiState =
    if (
        listOf(
            lastBreakEndNotificationAttemptDigest,
            lastConsumedBreakEndNotificationDigest,
            lastRejectedBreakEndNotificationDigest,
            acknowledgedBreakEndDigest,
        ).all { it == null || isTimedBreakNotificationDigest(it) }
    ) {
        this
    } else {
        copy(
            lastBreakEndNotificationAttemptDigest = lastBreakEndNotificationAttemptDigest
                ?.takeIf(::isTimedBreakNotificationDigest),
            lastConsumedBreakEndNotificationDigest = lastConsumedBreakEndNotificationDigest
                ?.takeIf(::isTimedBreakNotificationDigest),
            lastRejectedBreakEndNotificationDigest = lastRejectedBreakEndNotificationDigest
                ?.takeIf(::isTimedBreakNotificationDigest),
            acknowledgedBreakEndDigest = acknowledgedBreakEndDigest
                ?.takeIf(::isTimedBreakNotificationDigest),
        )
    }

private val TIMED_BREAK_NOTIFICATION_DIGEST_PATTERN = Regex("sha256:[0-9a-f]{64}")
private val TIMED_BREAK_RESOLVING_COMMAND_TYPES = setOf(
    "pause",
    "resume",
    "complete",
    "skip",
    "defer",
)
