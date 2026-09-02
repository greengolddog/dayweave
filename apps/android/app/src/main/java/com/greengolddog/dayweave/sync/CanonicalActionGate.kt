package com.greengolddog.dayweave.sync

import kotlinx.coroutines.sync.Semaphore

/**
 * Process-scoped, non-blocking admission gate for UI-triggered canonical actions.
 *
 * The sync manager also serializes its internal critical sections, but admission must happen
 * before a coroutine is launched: two taps can otherwise both observe a non-busy state and queue
 * contradictory transitions. Ordinary later taps are ignored while the first action is still
 * reconciling; mandatory recovery can explicitly wait for the same permit.
 */
internal class CanonicalActionGate {
    private val permit = Semaphore(permits = 1)

    fun tryEnter(): Boolean = permit.tryAcquire()

    /** FIFO admission for recovery work that must not disappear behind a transient action. */
    suspend fun enter() = permit.acquire()

    fun leave() {
        permit.release()
    }
}
