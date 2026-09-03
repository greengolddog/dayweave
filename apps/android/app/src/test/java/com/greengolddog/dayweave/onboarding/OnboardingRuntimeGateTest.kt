package com.greengolddog.dayweave.onboarding

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class OnboardingRuntimeGateTest {
    @Test
    fun everyPrivateBoundaryFailsClosedBeforeAcknowledgement() {
        val gate = OnboardingRuntimeGate(privacyAcknowledged = false)

        gate.setAppUnlocked(true)
        gate.setActivityStarted(true)

        assertFalse(gate.backgroundWorkAllowed())
        assertFalse(gate.privatePresentationAllowed())
        assertFalse(gate.foregroundProviderWorkAllowed())
    }

    @Test
    fun foregroundProviderWorkRequiresConsentUnlockAndStartedActivity() {
        val gate = OnboardingRuntimeGate(privacyAcknowledged = true)

        assertTrue(gate.backgroundWorkAllowed())
        assertFalse(gate.foregroundProviderWorkAllowed())
        gate.setAppUnlocked(true)
        assertFalse(gate.foregroundProviderWorkAllowed())
        gate.setActivityStarted(true)
        assertTrue(gate.foregroundProviderWorkAllowed())
        gate.setAppUnlocked(false)
        assertFalse(gate.foregroundProviderWorkAllowed())
    }

    @Test
    fun revokingRuntimeProjectionClosesEveryBoundary() {
        val gate = OnboardingRuntimeGate(privacyAcknowledged = true)
        gate.setAppUnlocked(true)
        gate.setActivityStarted(true)
        assertTrue(gate.privatePresentationAllowed())

        gate.setDurablePrivacyAcknowledgement(false)

        assertFalse(gate.backgroundWorkAllowed())
        assertFalse(gate.privatePresentationAllowed())
        assertFalse(gate.foregroundProviderWorkAllowed())
    }

    @Test
    fun bootstrapLaunchesExactlyOnceAcrossConcurrentCallers() {
        val gate = OnboardingRuntimeGate(privacyAcknowledged = true)
        val launches = AtomicInteger()
        val bootstrap = OnboardingConsentBootstrap { launches.incrementAndGet() }
        val ready = CountDownLatch(8)
        val start = CountDownLatch(1)
        val executor = Executors.newFixedThreadPool(8)

        try {
            val results = (1..8).map {
                executor.submit<Boolean> {
                    ready.countDown()
                    start.await(2, TimeUnit.SECONDS)
                    bootstrap.launchIfAllowed(gate)
                }
            }
            assertTrue(ready.await(2, TimeUnit.SECONDS))
            start.countDown()

            assertEquals(1, results.count { it.get(2, TimeUnit.SECONDS) })
            assertEquals(1, launches.get())
        } finally {
            executor.shutdownNow()
        }
    }

    @Test
    fun bootstrapDoesNotLaunchBeforeConsentAndCanRetryAfterLauncherFailure() {
        val gate = OnboardingRuntimeGate(privacyAcknowledged = false)
        var attempts = 0
        val bootstrap = OnboardingConsentBootstrap {
            attempts += 1
            if (attempts == 1) error("synthetic launch failure")
        }

        assertFalse(bootstrap.launchIfAllowed(gate))
        assertEquals(0, attempts)
        gate.setDurablePrivacyAcknowledgement(true)
        assertThrows(IllegalStateException::class.java) { bootstrap.launchIfAllowed(gate) }
        assertTrue(bootstrap.launchIfAllowed(gate))
        assertEquals(2, attempts)
    }
}
