package com.greengolddog.dayweave.health

import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import kotlinx.coroutines.CancellationException
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

/**
 * Coordinates optional provider reads independently of core planning and fails closed on every
 * unavailable, revoked, denied, or failed path.
 */
class EnergySignalManager(
    private val provider: EnergySignalProvider,
    private val plannerStore: PlannerStore,
    private val now: () -> Instant = Instant::now,
) {
    val requiredPermissions: Set<String> = provider.requiredPermissions.toSet()

    private val operationMutex = Mutex()
    private val mutableState = MutableStateFlow(EnergySignalState())
    val state: StateFlow<EnergySignalState> = mutableState.asStateFlow()

    suspend fun refresh() = operationMutex.withLock {
        refreshLocked(enableAfterPermissionCheck = false)
    }

    suspend fun enable() = operationMutex.withLock {
        refreshLocked(enableAfterPermissionCheck = true)
    }

    suspend fun onPermissionResult(granted: Set<String>) = operationMutex.withLock {
        if (!granted.containsAll(requiredPermissions)) {
            plannerStore.disableHealthConnectSync()
            mutableState.value = EnergySignalState(
                phase = EnergySignalPhase.DENIED,
                availability = EnergyProviderAvailability.AVAILABLE,
                permissionGranted = false,
                message = "Sleep access was not granted. Manual energy check-in remains available.",
            )
            return@withLock
        }
        refreshLocked(enableAfterPermissionCheck = true)
    }

    suspend fun disable() = operationMutex.withLock {
        plannerStore.disableHealthConnectSync()
        val availability = safeAvailability()
        mutableState.value = when (availability) {
            EnergyProviderAvailability.AVAILABLE -> EnergySignalState(
                phase = EnergySignalPhase.DISCONNECTED,
                availability = availability,
                permissionGranted = safePermissionCheck(),
                message = "Health Connect sync is off. Manual energy check-in remains available.",
            )
            EnergyProviderAvailability.UPDATE_REQUIRED -> updateRequiredState()
            EnergyProviderAvailability.UNAVAILABLE -> unavailableState()
        }
    }

    private suspend fun refreshLocked(enableAfterPermissionCheck: Boolean) {
        mutableState.value = EnergySignalState()
        try {
            when (val availability = provider.availability()) {
                EnergyProviderAvailability.UPDATE_REQUIRED -> {
                    plannerStore.disableHealthConnectSync()
                    mutableState.value = updateRequiredState()
                }
                EnergyProviderAvailability.UNAVAILABLE -> {
                    plannerStore.disableHealthConnectSync()
                    mutableState.value = unavailableState()
                }
                EnergyProviderAvailability.AVAILABLE -> {
                    val hasPermission = provider.hasRequiredPermissions()
                    if (!hasPermission) {
                        plannerStore.disableHealthConnectSync()
                        mutableState.value = EnergySignalState(
                            phase = EnergySignalPhase.PERMISSION_REQUIRED,
                            availability = availability,
                            permissionGranted = false,
                            message = "Sleep access is required for an on-device estimate. Planning still works without it.",
                        )
                        return
                    }

                    if (enableAfterPermissionCheck) plannerStore.enableHealthConnectSync()
                    if (!plannerStore.state.value.healthConnectSyncEnabled) {
                        mutableState.value = EnergySignalState(
                            phase = EnergySignalPhase.DISCONNECTED,
                            availability = availability,
                            permissionGranted = true,
                            message = "Health Connect access is available; sync is off.",
                        )
                        return
                    }

                    mutableState.value = EnergySignalState(
                        phase = EnergySignalPhase.READING,
                        availability = availability,
                        permissionGranted = true,
                        message = "Updating the on-device energy estimate",
                    )
                    when (val result = provider.readDerivedSnapshot(now())) {
                        is EnergyProviderResult.Snapshot -> {
                            plannerStore.replaceDerivedEnergySnapshot(result.value)
                            mutableState.value = EnergySignalState(
                                phase = EnergySignalPhase.READY,
                                availability = availability,
                                permissionGranted = true,
                                message = "Derived sleep estimate updated on this device",
                            )
                        }
                        EnergyProviderResult.NoData -> {
                            plannerStore.replaceDerivedEnergySnapshot(null)
                            mutableState.value = EnergySignalState(
                                phase = EnergySignalPhase.NO_DATA,
                                availability = availability,
                                permissionGranted = true,
                                message = "No recent sleep aggregate is available. Use a manual check-in.",
                            )
                        }
                        EnergyProviderResult.PermissionDenied -> {
                            plannerStore.disableHealthConnectSync()
                            mutableState.value = EnergySignalState(
                                phase = EnergySignalPhase.PERMISSION_REQUIRED,
                                availability = availability,
                                permissionGranted = false,
                                message = "Sleep access was revoked. The stored estimate was removed.",
                            )
                        }
                    }
                }
            }
        } catch (cancelled: CancellationException) {
            // A ViewModel/scope cancellation must not leave the shared application state looking
            // permanently busy. Remove any previous estimate before propagating cancellation.
            plannerStore.replaceDerivedEnergySnapshot(null)
            mutableState.value = EnergySignalState(
                phase = EnergySignalPhase.ERROR,
                availability = mutableState.value.availability,
                permissionGranted = false,
                message = "Health Connect update stopped. Planning continues with manual input.",
            )
            throw cancelled
        } catch (_: Exception) {
            // Health data and exception details are intentionally excluded from logs and UI.
            plannerStore.replaceDerivedEnergySnapshot(null)
            mutableState.value = EnergySignalState(
                phase = EnergySignalPhase.ERROR,
                availability = EnergyProviderAvailability.AVAILABLE,
                permissionGranted = false,
                message = "Health Connect could not be read. Planning continues with manual input.",
            )
        }
    }

    private fun safeAvailability(): EnergyProviderAvailability = try {
        provider.availability()
    } catch (_: Exception) {
        EnergyProviderAvailability.UNAVAILABLE
    }

    private suspend fun safePermissionCheck(): Boolean = try {
        provider.hasRequiredPermissions()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        false
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
}
