package com.greengolddog.dayweave.health

import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class EnergySignalPhase {
    CHECKING,
    DISCONNECTED,
    PERMISSION_REQUIRED,
    DENIED,
    READING,
    READY,
    NO_DATA,
    UPDATE_REQUIRED,
    UNAVAILABLE,
    ERROR,
}

data class EnergySignalState(
    val phase: EnergySignalPhase = EnergySignalPhase.CHECKING,
    val availability: EnergyProviderAvailability? = null,
    val permissionGranted: Boolean = false,
    val message: String = "Checking Health Connect availability",
) {
    val isBusy: Boolean get() = phase in setOf(EnergySignalPhase.CHECKING, EnergySignalPhase.READING)
}

/** Foreground/privacy generation owned by the application lifecycle boundary. */
interface EnergySignalGenerationFence {
    fun captureGeneration(): Long
    fun isCurrent(generation: Long): Boolean
}

/** Source-compatible default for tests and embedders that do not yet install a lifecycle fence. */
object AlwaysAllowedEnergySignalGenerationFence : EnergySignalGenerationFence {
    override fun captureGeneration(): Long = 0L
    override fun isCurrent(generation: Long): Boolean = generation == 0L
}

/**
 * Coordinates optional provider reads independently of core planning and fails closed on every
 * unavailable, revoked, denied, or failed path.
 */
class EnergySignalManager(
    private val provider: EnergySignalProvider,
    private val plannerStore: PlannerStore,
    private val now: () -> Instant = Instant::now,
    private val generationFence: EnergySignalGenerationFence =
        AlwaysAllowedEnergySignalGenerationFence,
) {
    val requiredPermissions: Set<String>
        get() {
            val generation = generationFence.captureGeneration()
            if (!generationFence.isCurrent(generation)) return emptySet()
            val permissions = try {
                provider.requiredPermissions.toSet()
            } catch (_: Exception) {
                return emptySet()
            }
            return permissions.takeIf { generationFence.isCurrent(generation) }.orEmpty()
        }

    private val operationMutex = Mutex()
    private val quarantineGeneration = AtomicLong(1L)
    private val activeOperationLock = Any()
    private val activeOperationJobs = mutableSetOf<Job>()
    private val mutableState = MutableStateFlow(EnergySignalState())
    val state: StateFlow<EnergySignalState> = mutableState.asStateFlow()

    suspend fun refresh() = runProviderOperation { admission ->
        refreshLocked(admission, enableAfterPermissionCheck = false)
    }

    suspend fun enable() = runProviderOperation { admission ->
        refreshLocked(admission, enableAfterPermissionCheck = true)
    }

    suspend fun onPermissionResult(granted: Set<String>) = runProviderOperation { admission ->
        val exactRequiredPermissions = providerRequiredPermissions(admission)
            ?: return@runProviderOperation
        if (!granted.containsAll(exactRequiredPermissions)) {
            if (!commitPlannerStore(admission) { plannerStore.disableHealthConnectSync() }) return@runProviderOperation
            commitState(
                admission,
                EnergySignalState(
                    phase = EnergySignalPhase.DENIED,
                    availability = EnergyProviderAvailability.AVAILABLE,
                    permissionGranted = false,
                    message = "Sleep access was not granted. Manual energy check-in remains available.",
                ),
            )
            return@runProviderOperation
        }
        refreshLocked(admission, enableAfterPermissionCheck = true)
    }

    suspend fun disable() = runProviderOperation { admission ->
        if (!commitPlannerStore(admission) { plannerStore.disableHealthConnectSync() }) {
            return@runProviderOperation
        }
        val availability = safeAvailability(admission) ?: return@runProviderOperation
        val nextState = when (availability) {
            EnergyProviderAvailability.AVAILABLE -> EnergySignalState(
                phase = EnergySignalPhase.DISCONNECTED,
                availability = availability,
                permissionGranted = safePermissionCheck(admission) ?: return@runProviderOperation,
                message = "Health Connect sync is off. Manual energy check-in remains available.",
            )
            EnergyProviderAvailability.UPDATE_REQUIRED -> updateRequiredState()
            EnergyProviderAvailability.UNAVAILABLE -> unavailableState()
        }
        commitState(admission, nextState)
    }

    /**
     * Invalidates admitted operations before requesting cancellation. The application must close
     * its injected [generationFence] before calling this on background or privacy lock so future
     * calls are rejected too. This method never waits for a provider to cooperate with cancellation.
     */
    fun quarantineForPrivacyBoundary() {
        quarantineGeneration.incrementAndGet()
        val jobs = synchronized(activeOperationLock) { activeOperationJobs.toList() }
        jobs.forEach { job ->
            job.cancel(CancellationException(PRIVACY_BOUNDARY_CANCELLATION_MESSAGE))
        }
        plannerStore.replaceDerivedEnergySnapshot(null)
        mutableState.value = privacyBoundaryState()
    }

    private suspend fun runProviderOperation(
        operation: suspend (EnergySignalAdmission) -> Unit,
    ) = coroutineScope {
        val admission = EnergySignalAdmission(
            fenceGeneration = generationFence.captureGeneration(),
            quarantineGeneration = quarantineGeneration.get(),
        )
        if (!isAdmitted(admission)) return@coroutineScope
        val operationJob = currentCoroutineContext()[Job]
            ?: throw IllegalStateException("Health Connect operation has no coroutine job")
        val registered = synchronized(activeOperationLock) {
            if (!isAdmitted(admission)) {
                false
            } else {
                activeOperationJobs += operationJob
                true
            }
        }
        if (!registered) return@coroutineScope
        try {
            operationMutex.withLock {
                if (isAdmitted(admission)) operation(admission)
            }
        } finally {
            synchronized(activeOperationLock) { activeOperationJobs -= operationJob }
        }
    }

    private suspend fun refreshLocked(
        admission: EnergySignalAdmission,
        enableAfterPermissionCheck: Boolean,
    ) {
        if (!commitState(admission, EnergySignalState())) return
        try {
            val availability = providerAvailability(admission) ?: return
            when (availability) {
                EnergyProviderAvailability.UPDATE_REQUIRED -> {
                    if (!commitPlannerStore(admission) { plannerStore.disableHealthConnectSync() }) {
                        return
                    }
                    commitState(admission, updateRequiredState())
                }
                EnergyProviderAvailability.UNAVAILABLE -> {
                    if (!commitPlannerStore(admission) { plannerStore.disableHealthConnectSync() }) {
                        return
                    }
                    commitState(admission, unavailableState())
                }
                EnergyProviderAvailability.AVAILABLE -> {
                    val hasPermission = permissionCheck(admission) ?: return
                    if (!hasPermission) {
                        if (!commitPlannerStore(admission) { plannerStore.disableHealthConnectSync() }) {
                            return
                        }
                        commitState(
                            admission,
                            EnergySignalState(
                                phase = EnergySignalPhase.PERMISSION_REQUIRED,
                                availability = availability,
                                permissionGranted = false,
                                message = "Sleep access is required for an on-device estimate. Planning still works without it.",
                            ),
                        )
                        return
                    }

                    if (
                        enableAfterPermissionCheck &&
                        !commitPlannerStore(admission) { plannerStore.enableHealthConnectSync() }
                    ) {
                        return
                    }
                    if (!isAdmitted(admission)) return
                    if (!plannerStore.state.value.healthConnectSyncEnabled) {
                        commitState(
                            admission,
                            EnergySignalState(
                                phase = EnergySignalPhase.DISCONNECTED,
                                availability = availability,
                                permissionGranted = true,
                                message = "Health Connect access is available; sync is off.",
                            ),
                        )
                        return
                    }

                    if (
                        !commitState(
                            admission,
                            EnergySignalState(
                                phase = EnergySignalPhase.READING,
                                availability = availability,
                                permissionGranted = true,
                                message = "Updating the on-device energy estimate",
                            ),
                        )
                    ) {
                        return
                    }
                    val result = readSnapshot(admission) ?: return
                    when (result) {
                        is EnergyProviderResult.Snapshot -> {
                            if (
                                !commitPlannerStore(admission) {
                                    plannerStore.replaceDerivedEnergySnapshot(result.value)
                                }
                            ) {
                                return
                            }
                            commitState(
                                admission,
                                EnergySignalState(
                                    phase = EnergySignalPhase.READY,
                                    availability = availability,
                                    permissionGranted = true,
                                    message = "Derived sleep estimate updated on this device",
                                ),
                            )
                        }
                        EnergyProviderResult.NoData -> {
                            if (
                                !commitPlannerStore(admission) {
                                    plannerStore.replaceDerivedEnergySnapshot(null)
                                }
                            ) {
                                return
                            }
                            commitState(
                                admission,
                                EnergySignalState(
                                    phase = EnergySignalPhase.NO_DATA,
                                    availability = availability,
                                    permissionGranted = true,
                                    message = "No recent sleep aggregate is available. Use a manual check-in.",
                                ),
                            )
                        }
                        EnergyProviderResult.PermissionDenied -> {
                            if (!commitPlannerStore(admission) { plannerStore.disableHealthConnectSync() }) {
                                return
                            }
                            commitState(
                                admission,
                                EnergySignalState(
                                    phase = EnergySignalPhase.PERMISSION_REQUIRED,
                                    availability = availability,
                                    permissionGranted = false,
                                    message = "Sleep access was revoked. The stored estimate was removed.",
                                ),
                            )
                        }
                    }
                }
            }
        } catch (cancelled: CancellationException) {
            // A ViewModel/scope cancellation must not leave the shared application state looking
            // permanently busy. Remove any previous estimate before propagating cancellation.
            if (commitPlannerStore(admission) { plannerStore.replaceDerivedEnergySnapshot(null) }) {
                commitState(
                    admission,
                    EnergySignalState(
                        phase = EnergySignalPhase.ERROR,
                        availability = mutableState.value.availability,
                        permissionGranted = false,
                        message = "Health Connect update stopped. Planning continues with manual input.",
                    ),
                )
            }
            throw cancelled
        } catch (_: Exception) {
            // Health data and exception details are intentionally excluded from logs and UI.
            if (commitPlannerStore(admission) { plannerStore.replaceDerivedEnergySnapshot(null) }) {
                commitState(
                    admission,
                    EnergySignalState(
                        phase = EnergySignalPhase.ERROR,
                        availability = EnergyProviderAvailability.AVAILABLE,
                        permissionGranted = false,
                        message = "Health Connect could not be read. Planning continues with manual input.",
                    ),
                )
            }
        }
    }

    private fun providerAvailability(
        admission: EnergySignalAdmission,
    ): EnergyProviderAvailability? {
        if (!isAdmitted(admission)) return null
        val result = provider.availability()
        return result.takeIf { isAdmitted(admission) }
    }

    private fun providerRequiredPermissions(admission: EnergySignalAdmission): Set<String>? {
        if (!isAdmitted(admission)) return null
        val result = try {
            provider.requiredPermissions.toSet()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            return null
        }
        return result.takeIf { isAdmitted(admission) }
    }

    private suspend fun permissionCheck(admission: EnergySignalAdmission): Boolean? {
        if (!isAdmitted(admission)) return null
        val result = provider.hasRequiredPermissions()
        return result.takeIf { isAdmitted(admission) }
    }

    private suspend fun readSnapshot(admission: EnergySignalAdmission): EnergyProviderResult? {
        if (!isAdmitted(admission)) return null
        val result = provider.readDerivedSnapshot(now())
        return result.takeIf { isAdmitted(admission) }
    }

    private fun safeAvailability(
        admission: EnergySignalAdmission,
    ): EnergyProviderAvailability? = try {
        providerAvailability(admission)
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        EnergyProviderAvailability.UNAVAILABLE.takeIf { isAdmitted(admission) }
    }

    private suspend fun safePermissionCheck(admission: EnergySignalAdmission): Boolean? = try {
        permissionCheck(admission)
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        false.takeIf { isAdmitted(admission) }
    }

    private fun isAdmitted(admission: EnergySignalAdmission): Boolean =
        quarantineGeneration.get() == admission.quarantineGeneration &&
            generationFence.isCurrent(admission.fenceGeneration)

    private inline fun commitPlannerStore(
        admission: EnergySignalAdmission,
        mutation: () -> Unit,
    ): Boolean {
        if (!isAdmitted(admission)) return false
        mutation()
        return true
    }

    private fun commitState(
        admission: EnergySignalAdmission,
        next: EnergySignalState,
    ): Boolean {
        if (!isAdmitted(admission)) return false
        mutableState.value = next
        return true
    }

    private fun updateRequiredState() = EnergySignalState(
        phase = EnergySignalPhase.UPDATE_REQUIRED,
        availability = EnergyProviderAvailability.UPDATE_REQUIRED,
        permissionGranted = false,
        message = "Install or update Health Connect to use sleep-based estimates.",
    )

    private fun unavailableState() = EnergySignalState(
        phase = EnergySignalPhase.UNAVAILABLE,
        availability = EnergyProviderAvailability.UNAVAILABLE,
        permissionGranted = false,
        message = "Health Connect is unavailable on this device. Manual check-in still works.",
    )

    private fun privacyBoundaryState() = EnergySignalState(
        phase = EnergySignalPhase.DISCONNECTED,
        availability = null,
        permissionGranted = false,
        message = "Health Connect is paused while DayWeave is protected or in the background.",
    )

    private data class EnergySignalAdmission(
        val fenceGeneration: Long,
        val quarantineGeneration: Long,
    )

    private companion object {
        const val PRIVACY_BOUNDARY_CANCELLATION_MESSAGE =
            "Health Connect operation crossed a privacy boundary"
    }
}
