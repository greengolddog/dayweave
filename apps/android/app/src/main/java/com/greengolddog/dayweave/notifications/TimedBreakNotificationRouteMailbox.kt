package com.greengolddog.dayweave.notifications

import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import com.greengolddog.dayweave.MainActivity
import com.greengolddog.dayweave.model.isTimedBreakNotificationDigest
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * App-private, process-owned route mailbox backed by a synchronous durable receipt.
 *
 * MainActivity is non-exported, so only an app-created immutable PendingIntent (or trusted local
 * code) can deliver its notification route. Persisting before composition retains a genuine tap
 * across app lock, task recreation, or process death. Clearing happens only after the encrypted
 * planner route receipt has settled and the UI calls [consume]. Stored values are opaque identity
 * digests only.
 */
internal class TimedBreakNotificationRouteMailbox(
    private val preferences: SharedPreferences,
    private val commit: (SharedPreferences.Editor) -> Boolean = SharedPreferences.Editor::commit,
) {
    constructor(context: Context) : this(
        context.applicationContext.getSharedPreferences(
            TIMED_BREAK_NOTIFICATION_ROUTE_PREFERENCES,
            Context.MODE_PRIVATE,
        ),
    )

    private val mutablePendingDigest = MutableStateFlow(restorePendingDigest())
    val pendingDigest: StateFlow<String?> = mutablePendingDigest.asStateFlow()

    /** Called only from the non-exported MainActivity notification-entry path. */
    fun acceptTrusted(intent: Intent?): Boolean {
        val digest = timedBreakNotificationDigest(intent) ?: return false
        return synchronized(timedBreakNotificationRoutePreferencesLock) {
            if (
                preferences.getString(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY, null) == digest &&
                mutablePendingDigest.value == digest
            ) {
                return@synchronized true
            }
            if (
                preferences.getString(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY, null) !=
                digest
            ) {
                return@synchronized false
            }
            val moved = commit(
                preferences.edit()
                    .remove(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY)
                    .putString(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY, digest),
            )
            if (!moved) {
                return@synchronized false
            }
            mutablePendingDigest.value = digest
            true
        }
    }

    /** Clears exactly the route whose encrypted consume/reject receipt has already settled. */
    fun consume(expectedDigest: String): Boolean {
        if (!isTimedBreakNotificationDigest(expectedDigest)) return false
        return synchronized(timedBreakNotificationRoutePreferencesLock) {
            val persisted = preferences.getString(
                TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY,
                null,
            )
            if (persisted != expectedDigest || mutablePendingDigest.value != expectedDigest) {
                synchronizeFromPreferences(persisted)
                return@synchronized false
            }
            if (!commit(preferences.edit().remove(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY))) {
                return@synchronized false
            }
            mutablePendingDigest.value = null
            true
        }
    }

    /** Commits the exact one-shot capability before NotificationManager may expose its tap. */
    fun issue(expectedDigest: String): Boolean {
        if (!isTimedBreakNotificationDigest(expectedDigest)) return false
        return synchronized(timedBreakNotificationRoutePreferencesLock) {
            if (
                preferences.getString(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY, null) ==
                expectedDigest
            ) {
                // The exact route is already pending in-app; never create a second tap capability.
                return@synchronized false
            }
            commit(
                preferences.edit().putString(
                    TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY,
                    expectedDigest,
                ),
            )
        }
    }

    /** Revokes only the intended issue; null revokes whichever fixed-ID route is current. */
    fun revokeIssued(expectedDigest: String? = null): Boolean =
        synchronized(timedBreakNotificationRoutePreferencesLock) {
            val issued = preferences.getString(
                TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY,
                null,
            ) ?: return@synchronized true
            if (expectedDigest != null && issued != expectedDigest) return@synchronized true
            commit(preferences.edit().remove(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY))
        }

    private fun restorePendingDigest(): String? = synchronized(
        timedBreakNotificationRoutePreferencesLock,
    ) {
        val persisted = preferences.getString(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY, null)
        val issued = preferences.getString(
            TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY,
            null,
        )
        if (issued != null && !isTimedBreakNotificationDigest(issued)) {
            commit(
                preferences.edit().remove(
                    TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY,
                ),
            )
        }
        if (persisted != null && persisted == issued && isTimedBreakNotificationDigest(issued)) {
            // A pending route already crossed the trust boundary; pending wins over duplicate issue.
            commit(preferences.edit().remove(TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY))
        }
        if (persisted == null || isTimedBreakNotificationDigest(persisted)) {
            return@synchronized persisted
        }
        // Invalid predecessor/corrupt values have no routing authority and are dropped fail-closed.
        commit(preferences.edit().remove(TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY))
        null
    }

    private fun synchronizeFromPreferences(persisted: String?) {
        mutablePendingDigest.value = persisted?.takeIf(::isTimedBreakNotificationDigest)
    }
}

/**
 * Admits the route only at the non-exported planner boundary, then replaces the Activity intent
 * with a newly constructed route-free value before any UI can observe or later recreate it.
 */
internal fun admitTrustedTimedBreakRouteAndSanitizeMainIntent(
    context: Context,
    candidate: Intent?,
    mailbox: TimedBreakNotificationRouteMailbox,
): Intent {
    mailbox.acceptTrusted(candidate)
    return Intent(context, MainActivity::class.java).setAction(Intent.ACTION_MAIN)
}

internal const val TIMED_BREAK_NOTIFICATION_ROUTE_PREFERENCES =
    "dayweave-timed-break-notification-route"
internal const val TIMED_BREAK_NOTIFICATION_ROUTE_DIGEST_KEY = "pending-opaque-digest"
internal const val TIMED_BREAK_NOTIFICATION_ROUTE_ISSUED_DIGEST_KEY = "issued-opaque-digest"

/** Worker/gateway and Activity mailbox instances share one atomic preferences protocol. */
private val timedBreakNotificationRoutePreferencesLock = Any()
