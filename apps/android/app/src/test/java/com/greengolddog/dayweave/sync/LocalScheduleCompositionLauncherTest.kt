package com.greengolddog.dayweave.sync

import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalScheduleCompositionLauncherTest {
    @Test
    fun `cancel before lazy start releases gate and permits a later canonical admission`() {
        val gate = CanonicalActionGate()
        val compositions = AtomicInteger()
        lateinit var launcher: LocalScheduleCompositionLauncher
        launcher = LocalScheduleCompositionLauncher(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined),
            actionGate = gate,
            compose = { compositions.incrementAndGet() },
            startJob = { launcher.setForegroundActive(false) },
        )
        launcher.setForegroundActive(true)

        assertTrue(launcher.launch())
        assertTrue(gate.tryEnter())
        gate.leave()
        assertTrue(compositions.get() == 0)
    }

    @Test
    fun `background after admission but before coroutine dispatch prevents native work`() {
        val gate = CanonicalActionGate()
        val compositions = AtomicInteger()
        var admittedJob: Job? = null
        val launcher = LocalScheduleCompositionLauncher(
            scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined),
            actionGate = gate,
            compose = { compositions.incrementAndGet() },
            startJob = { admittedJob = it },
        )
        launcher.setForegroundActive(true)

        assertTrue(launcher.launch())
        launcher.setForegroundActive(false)

        assertFalse(requireNotNull(admittedJob).start())
        assertTrue(compositions.get() == 0)
        assertTrue(gate.tryEnter())
        gate.leave()
    }
}
