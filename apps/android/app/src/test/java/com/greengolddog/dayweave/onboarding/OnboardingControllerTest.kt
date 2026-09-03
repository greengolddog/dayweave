package com.greengolddog.dayweave.onboarding

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class OnboardingControllerTest {
    @Test
    fun privacyMustBeDurableBeforeAnythingCanMoveBeyondWelcome() {
        val store = FakeCheckpointStore()
        val controller = OnboardingController(store)

        assertFalse(controller.advance())
        assertEquals(OnboardingStep.WELCOME, controller.active().currentStep)
        assertTrue(controller.acknowledgePrivacy())
        assertTrue(controller.advance())

        assertEquals(OnboardingStep.API, controller.active().currentStep)
        assertTrue(controller.active().privacyAcknowledged)
        assertEquals(controller.state, controller.states.value)
        assertEquals(2, store.savedTransitions.size)
    }

    @Test
    fun stateFlowChangesOnlyAfterTheStoreAcceptsTheTransition() {
        val store = FakeCheckpointStore()
        lateinit var controller: OnboardingController
        var observedDuringSave: OnboardingControllerState? = null
        store.onSave = { observedDuringSave = controller.state }
        controller = OnboardingController(store)

        assertTrue(controller.acknowledgePrivacy())

        assertEquals(
            OnboardingControllerState.Active(OnboardingCheckpoint.fresh()),
            observedDuringSave,
        )
        assertTrue(controller.active().privacyAcknowledged)
        assertEquals(controller.state, controller.states.value)
    }

    @Test
    fun advanceBackAndFurthestFollowTheFixedHierarchy() {
        val controller = OnboardingController(FakeCheckpointStore())
        assertTrue(controller.acknowledgePrivacy())
        assertTrue(controller.advance())
        assertFalse(controller.advance())
        assertEquals(OnboardingStep.API, controller.active().currentStep)
        assertTrue(controller.advance(prerequisiteReady = true))
        assertEquals(OnboardingStep.GOOGLE, controller.active().currentStep)
        assertEquals(OnboardingStep.GOOGLE, controller.active().furthestStep)

        assertTrue(controller.back())
        assertEquals(OnboardingStep.API, controller.active().currentStep)
        assertEquals(OnboardingStep.GOOGLE, controller.active().furthestStep)
        assertTrue(controller.back())
        assertEquals(OnboardingStep.WELCOME, controller.active().currentStep)
        assertEquals(OnboardingStep.GOOGLE, controller.active().furthestStep)
        assertFalse(controller.back())

        assertTrue(controller.advance())
        assertEquals(OnboardingStep.API, controller.active().currentStep)
        assertEquals(OnboardingStep.GOOGLE, controller.active().furthestStep)
    }

    @Test
    fun completionIsPossibleOnlyAtTheExactReadyCheckpoint() {
        val controller = OnboardingController(FakeCheckpointStore())
        assertTrue(controller.acknowledgePrivacy())
        assertFalse(controller.complete())

        OnboardingStep.entries.drop(1).forEach { expected ->
            assertTrue(
                controller.advance(
                    prerequisiteReady = controller.active().currentStep != OnboardingStep.WELCOME,
                ),
            )
            assertEquals(expected, controller.active().currentStep)
        }

        assertFalse(controller.advance())
        assertFalse(controller.complete())
        assertFalse(controller.active().completed)
        assertTrue(controller.complete(allPrerequisitesReady = true))
        assertTrue(controller.active().completed)
        assertEquals(OnboardingStep.READY, controller.active().currentStep)
        assertFalse(controller.back())
        assertTrue(controller.complete(allPrerequisitesReady = true))
    }

    @Test
    fun failedWritesLeaveObservableStateUnchanged() {
        val store = FakeCheckpointStore().apply { writesSucceed = false }
        val controller = OnboardingController(store)
        val initial = controller.state

        assertFalse(controller.acknowledgePrivacy())
        assertEquals(initial, controller.state)

        store.writesSucceed = true
        assertTrue(controller.acknowledgePrivacy())
        assertTrue(controller.advance())
        val atApi = controller.state
        store.writesSucceed = false
        assertFalse(controller.back())
        assertEquals(atApi, controller.state)

        val ready = OnboardingCheckpoint(
            currentStep = OnboardingStep.READY,
            furthestStep = OnboardingStep.READY,
            privacyAcknowledged = true,
        )
        val completionStore = FakeCheckpointStore(
            OnboardingCheckpointLoadResult.Loaded(ready),
        ).apply { writesSucceed = false }
        val completionController = OnboardingController(completionStore)
        assertFalse(completionController.complete(allPrerequisitesReady = true))
        assertFalse(completionController.active().completed)
    }

    @Test
    fun setupLaterIsProcessLocalAndNeverAdvancesOrCompletes() {
        val store = FakeCheckpointStore()
        val controller = OnboardingController(store)
        assertTrue(controller.acknowledgePrivacy())
        val durableBeforeDeferral = store.result
        val saveCount = store.savedTransitions.size

        assertTrue(controller.deferSetupForSession())
        assertTrue(controller.active().setupDeferredForSession)
        assertEquals(OnboardingStep.WELCOME, controller.active().currentStep)
        assertEquals(saveCount, store.savedTransitions.size)
        assertEquals(durableBeforeDeferral, store.result)
        assertFalse(controller.advance())
        assertFalse(controller.back())
        assertFalse(controller.complete())

        val restarted = OnboardingController(store)
        assertFalse(restarted.active().setupDeferredForSession)
        assertEquals(OnboardingStep.WELCOME, restarted.active().currentStep)
        assertTrue(controller.resumeSetup())
        assertTrue(controller.advance())
        assertEquals(OnboardingStep.API, controller.active().currentStep)
    }

    @Test
    fun corruptStateBlocksAllProgressUntilExactRecoverySucceeds() {
        val firstIdentity = OnboardingCorruptArtifactIdentity("first")
        val otherIdentity = OnboardingCorruptArtifactIdentity("other")
        val store = FakeCheckpointStore(OnboardingCheckpointLoadResult.Corrupt(firstIdentity))
        val controller = OnboardingController(store)

        assertTrue(controller.state is OnboardingControllerState.RecoveryRequired)
        assertFalse(controller.acknowledgePrivacy())
        assertFalse(controller.advance())
        assertFalse(controller.back())
        assertFalse(controller.deferSetupForSession())
        assertFalse(controller.complete())
        assertFalse(controller.recoverCorruptExact(otherIdentity))

        store.resetsSucceed = false
        assertFalse(controller.recoverCorruptExact(firstIdentity))
        assertTrue(controller.state is OnboardingControllerState.RecoveryRequired)

        store.resetsSucceed = true
        assertTrue(controller.recoverCorruptExact(firstIdentity))
        assertEquals(OnboardingCheckpoint.fresh(), controller.active().checkpoint)
    }

    @Test
    fun refreshSynchronouslyObservesASeparatelyPersistedCheckpoint() {
        val store = FakeCheckpointStore()
        val controller = OnboardingController(store)
        val acknowledged = OnboardingCheckpoint.fresh().copy(privacyAcknowledged = true)
        store.result = OnboardingCheckpointLoadResult.Loaded(acknowledged)

        assertEquals(
            OnboardingControllerState.Active(acknowledged),
            controller.refreshFromStore(),
        )
    }

    @Test
    fun invalidCheckpointInvariantsAreRejectedAtConstruction() {
        assertTrue(
            runCatching {
                OnboardingCheckpoint(
                    currentStep = OnboardingStep.API,
                    furthestStep = OnboardingStep.WELCOME,
                    privacyAcknowledged = true,
                )
            }.isFailure,
        )
        assertTrue(
            runCatching {
                OnboardingCheckpoint(
                    currentStep = OnboardingStep.API,
                    furthestStep = OnboardingStep.API,
                )
            }.isFailure,
        )
        assertTrue(
            runCatching {
                OnboardingCheckpoint(
                    currentStep = OnboardingStep.FIRST_PLAN,
                    furthestStep = OnboardingStep.READY,
                    privacyAcknowledged = true,
                    completed = true,
                )
            }.isFailure,
        )
    }

    private fun OnboardingController.active(): OnboardingControllerState.Active =
        state as OnboardingControllerState.Active

    private class FakeCheckpointStore(
        var result: OnboardingCheckpointLoadResult = OnboardingCheckpointLoadResult.Loaded(
            OnboardingCheckpoint.fresh(),
        ),
    ) : OnboardingCheckpointStore {
        var writesSucceed = true
        var resetsSucceed = true
        var onSave: (() -> Unit)? = null
        val savedTransitions = mutableListOf<Pair<OnboardingCheckpoint, OnboardingCheckpoint>>()

        override fun load(): OnboardingCheckpointLoadResult = result

        override fun saveIfCurrent(
            expected: OnboardingCheckpoint,
            replacement: OnboardingCheckpoint,
        ): Boolean {
            savedTransitions += expected to replacement
            onSave?.invoke()
            if (!writesSucceed || !replacement.isPermittedReplacementOf(expected)) return false
            val loaded = result as? OnboardingCheckpointLoadResult.Loaded ?: return false
            if (loaded.checkpoint != expected) return false
            result = OnboardingCheckpointLoadResult.Loaded(replacement)
            return true
        }

        override fun resetCorruptExact(expected: OnboardingCorruptArtifactIdentity): Boolean {
            if (!resetsSucceed) return false
            val corrupt = result as? OnboardingCheckpointLoadResult.Corrupt ?: return false
            if (corrupt.artifactIdentity != expected) return false
            result = OnboardingCheckpointLoadResult.Loaded(OnboardingCheckpoint.fresh())
            return true
        }
    }
}
