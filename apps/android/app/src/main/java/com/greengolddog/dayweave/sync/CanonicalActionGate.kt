package com.greengolddog.dayweave.sync

import java.util.concurrent.atomic.AtomicBoolean

/**
 * Process-scoped, non-blocking admission gate for UI-triggered canonical actions.
 *
 * The sync manager also serializes its internal critical sections, but admission must happen
 * before a coroutine is launched: two taps can otherwise both observe a non-busy state and queue
 * contradictory transitions. The later tap is deliberately ignored while the first action is
 * still reconciling.
 */
internal class CanonicalActionGate {
    private val inFlight = AtomicBoolean(false)

    fun tryEnter(): Boolean = inFlight.compareAndSet(false, true)

    fun leave() {
        check(inFlight.compareAndSet(true, false)) { "Canonical action gate was not held" }
    }
}
