package com.greengolddog.dayweave.health

import com.greengolddog.dayweave.model.DerivedEnergySnapshot
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.EnergySignalSource
import com.greengolddog.dayweave.model.RecoveryBand
import java.time.Instant

enum class EnergyProviderAvailability {
    AVAILABLE,
    UPDATE_REQUIRED,
    UNAVAILABLE,
}

sealed interface EnergyProviderResult {
    data class Snapshot(val value: DerivedEnergySnapshot) : EnergyProviderResult
    data object NoData : EnergyProviderResult
    data object PermissionDenied : EnergyProviderResult
}

/** Provider boundary shared by Health Connect today and a future WHOOP adapter. */
interface EnergySignalProvider {
    val requiredPermissions: Set<String>

    fun availability(): EnergyProviderAvailability

    suspend fun hasRequiredPermissions(): Boolean

    /** Returns only a derived snapshot. Implementations must not leak raw provider records. */
    suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult
}

/**
 * Deterministic provider for unit tests and local previews. It contains synthetic bands only—never
 * copied Health Connect records or user data.
 */
class FakeEnergySignalProvider(
    var currentAvailability: EnergyProviderAvailability = EnergyProviderAvailability.AVAILABLE,
    var permissionsGranted: Boolean = true,
    var result: EnergyProviderResult = EnergyProviderResult.NoData,
    override val requiredPermissions: Set<String> = setOf("dayweave.synthetic.READ_ENERGY"),
) : EnergySignalProvider {
    var readCount: Int = 0
        private set

    override fun availability(): EnergyProviderAvailability = currentAvailability

    override suspend fun hasRequiredPermissions(): Boolean = permissionsGranted

    override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult {
        readCount += 1
        return result
    }
}

/** App-specific planning heuristic; it is deliberately not presented as a medical score. */
internal fun deriveEnergyFromRecentSleep(
    sleepMinutes: Long,
    calculatedAt: Instant,
): DerivedEnergySnapshot? {
    if (sleepMinutes !in 1..MAX_REASONABLE_RECENT_SLEEP_MINUTES) return null
    val (energy, recovery) = when {
        sleepMinutes < LOW_SLEEP_BOUNDARY_MINUTES -> EnergyLevel.LOW to RecoveryBand.LOW
        sleepMinutes < HIGH_SLEEP_BOUNDARY_MINUTES ->
            EnergyLevel.MEDIUM to RecoveryBand.BALANCED
        else -> EnergyLevel.DEEP to RecoveryBand.HIGH
    }
    return DerivedEnergySnapshot(
        energy = energy,
        recovery = recovery,
        source = EnergySignalSource.HEALTH_CONNECT_SLEEP,
        calculatedAt = calculatedAt.toString(),
    )
}

private const val LOW_SLEEP_BOUNDARY_MINUTES = 6L * 60L
private const val HIGH_SLEEP_BOUNDARY_MINUTES = 8L * 60L
private const val MAX_REASONABLE_RECENT_SLEEP_MINUTES = 16L * 60L
