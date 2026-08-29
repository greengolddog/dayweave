package com.greengolddog.dayweave.health

import android.content.Context
import androidx.health.connect.client.HealthConnectClient
import androidx.health.connect.client.permission.HealthPermission
import androidx.health.connect.client.records.SleepSessionRecord
import androidx.health.connect.client.request.AggregateRequest
import androidx.health.connect.client.time.TimeRangeFilter
import java.time.Duration
import java.time.Instant

/** Foreground-only, read-only Health Connect adapter for the first CTX-006 slice. */
class HealthConnectEnergyProvider(context: Context) : EnergySignalProvider {
    private val applicationContext = context.applicationContext

    override val requiredPermissions: Set<String> = setOf(
        HealthPermission.getReadPermission(SleepSessionRecord::class),
    )

    override fun availability(): EnergyProviderAvailability = when (
        HealthConnectClient.getSdkStatus(applicationContext)
    ) {
        HealthConnectClient.SDK_AVAILABLE -> EnergyProviderAvailability.AVAILABLE
        HealthConnectClient.SDK_UNAVAILABLE_PROVIDER_UPDATE_REQUIRED ->
            EnergyProviderAvailability.UPDATE_REQUIRED
        else -> EnergyProviderAvailability.UNAVAILABLE
    }

    override suspend fun hasRequiredPermissions(): Boolean {
        if (availability() != EnergyProviderAvailability.AVAILABLE) return false
        return client().permissionController.getGrantedPermissions()
            .containsAll(requiredPermissions)
    }

    override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult {
        if (!hasRequiredPermissions()) return EnergyProviderResult.PermissionDenied

        // Aggregate in-process so record IDs, session bounds, stages, titles, and notes never cross
        // the provider boundary or enter DayWeave persistence.
        val result = client().aggregate(
            AggregateRequest(
                metrics = setOf(SleepSessionRecord.SLEEP_DURATION_TOTAL),
                timeRangeFilter = TimeRangeFilter.between(
                    at.minus(RECENT_SLEEP_WINDOW),
                    at,
                ),
            ),
        )
        val duration = result[SleepSessionRecord.SLEEP_DURATION_TOTAL]
            ?: return EnergyProviderResult.NoData
        val snapshot = deriveEnergyFromRecentSleep(duration.toMinutes(), at)
            ?: return EnergyProviderResult.NoData
        return EnergyProviderResult.Snapshot(snapshot)
    }

    private fun client(): HealthConnectClient =
        HealthConnectClient.getOrCreate(applicationContext)

    private companion object {
        val RECENT_SLEEP_WINDOW: Duration = Duration.ofHours(24)
    }
}
