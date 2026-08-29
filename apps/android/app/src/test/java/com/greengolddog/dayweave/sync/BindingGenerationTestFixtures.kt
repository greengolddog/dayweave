package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiBindingOperationGate
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeviceAuthRequestExecutor
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

/** Synthetic credential owner that exposes the production binding-generation gate to manager tests. */
internal class GenerationBoundCredentialStore(
    private val gate: ApiBindingOperationGate = ApiBindingOperationGate(),
) : ApiCredentialStore {
    @Volatile
    var configurationId: String? = "configuration-a"
    @Volatile
    var enabled: Boolean = true
    @Volatile
    var configurationObserved: (() -> Unit)? = null
    private var lastSync: Long? = null

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = BASE_URL.takeIf { enabled },
        hasBearerToken = enabled,
        lastSuccessfulSyncEpochMillis = lastSync,
        configurationId = configurationId.takeIf { enabled },
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        if (!enabled) return null
        return AuthenticatedApiConfiguration.createCoordinated(
            baseUrl = BASE_URL,
            bearerToken = "synthetic-test-bearer",
            configurationId = requireNotNull(configurationId),
            executor = RejectingTestExecutor,
            bindingGate = gate,
            bindingGeneration = gate.captureGeneration(),
            allowCleartextLoopback = false,
        ).also { configurationObserved?.invoke() }
    }

    suspend fun <T> invalidateBeforeQuarantine(
        nextConfigurationId: String? = null,
        quarantine: suspend () -> T,
    ): T = gate.invalidateBeforeQuarantine {
        val result = quarantine()
        configurationId = nextConfigurationId
        enabled = nextConfigurationId != null
        result
    }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() {
        enabled = false
        configurationId = null
    }

    override fun recordSuccessfulSync(epochMillis: Long) {
        lastSync = epochMillis
    }

    private object RejectingTestExecutor : DeviceAuthRequestExecutor {
        override suspend fun executeAuthenticated(
            configuration: AuthenticatedApiConfiguration,
            client: OkHttpClient,
            request: Request,
        ): Response = error("Synthetic manager transports must not dispatch OkHttp")
    }

    private companion object {
        const val BASE_URL = "https://api.example.test/"
    }
}
