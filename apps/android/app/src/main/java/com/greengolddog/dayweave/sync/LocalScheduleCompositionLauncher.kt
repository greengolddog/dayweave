package com.greengolddog.dayweave.sync

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch

/**
 * Process-scoped admission and lifecycle fence for the non-preemptible native scheduler call.
 *
 * The generation is captured before entering the shared canonical-action gate, then checked again
 * after admission. A background or privacy-lock boundary therefore invalidates an already admitted
 * click even when the coroutine has not started yet. Completion owns gate release, including a
 * LAZY job cancelled before its body ever runs.
 */
internal class LocalScheduleCompositionLauncher(
    private val scope: CoroutineScope,
    private val actionGate: CanonicalActionGate,
    private val compose: suspend (admittedGeneration: Long) -> Unit,
    private val startJob: (Job) -> Unit = Job::start,
) : LocalCompositionLifecycleFence {
    private val generation = AtomicLong(1L)
    private val foregroundActive = AtomicBoolean(false)
    private val lock = Any()
    private var retainedJob: Job? = null

    override fun captureGeneration(): Long = generation.get()

    override fun isCurrent(generation: Long): Boolean =
        foregroundActive.get() && generation == this.generation.get()

    fun launch(): Boolean {
        val admittedGeneration = captureGeneration()
        if (!isCurrent(admittedGeneration)) return false
        if (!actionGate.tryEnter()) return false
        if (!isCurrent(admittedGeneration)) {
            actionGate.leave()
            return false
        }

        lateinit var job: Job
        job = scope.launch(start = CoroutineStart.LAZY) {
            compose(admittedGeneration)
        }
        val retained = synchronized(lock) {
            if (retainedJob != null) {
                false
            } else {
                retainedJob = job
                true
            }
        }
        if (!retained) {
            job.cancel()
            actionGate.leave()
            return false
        }

        // Register after publishing the job. Cancellation in the small intervening window is safe:
        // invokeOnCompletion runs immediately for an already completed job and still clears it.
        job.invokeOnCompletion {
            synchronized(lock) {
                if (retainedJob === job) retainedJob = null
            }
            actionGate.leave()
        }
        if (!isCurrent(admittedGeneration)) {
            job.cancel()
        } else {
            startJob(job)
        }
        return true
    }

    /** Invalidates native output before requesting coroutine cancellation. */
    fun cancel() {
        generation.incrementAndGet()
        synchronized(lock) { retainedJob }?.cancel()
    }

    fun setForegroundActive(active: Boolean) {
        if (active) {
            foregroundActive.set(true)
        } else {
            foregroundActive.set(false)
            cancel()
        }
    }

    suspend fun cancelAndDrain() {
        generation.incrementAndGet()
        synchronized(lock) { retainedJob }?.cancelAndJoin()
    }
}
