package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.model.isApplicationReady
import com.greengolddog.dayweave.model.usesReservedChangeSetNamespace
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.RemoteSuggestion
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.SuggestionApiException
import com.greengolddog.dayweave.network.SuggestionsTransport
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.DateTimeException
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonPrimitive

enum class SuggestionSyncPhase {
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    READY,
    SYNCING,
    CONNECTED,
    OFFLINE,
    ERROR,
}

data class SuggestionSyncState(
    val phase: SuggestionSyncPhase,
    val message: String,
    val baseUrl: String?,
    val hasStoredToken: Boolean,
    val lastSuccessfulSyncEpochMillis: Long?,
) {
    val isBusy: Boolean get() = phase == SuggestionSyncPhase.SYNCING
}

enum class SuggestionRefreshOutcome {
    SUCCESS,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    CONFIGURATION_ERROR,
    TRANSIENT_NETWORK_FAILURE,
    RETRYABLE_SERVER_FAILURE,
    PERMANENT_SERVER_FAILURE,
    PROTOCOL_FAILURE,
    LOCAL_STORAGE_FAILURE,
    UNEXPECTED_FAILURE,
}

class SuggestionSyncManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: SuggestionsTransport,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) {
    private val operationMutex = Mutex()
    private val mutableState = MutableStateFlow(stateFrom(credentialStore.snapshot()))
    val state: StateFlow<SuggestionSyncState> = mutableState.asStateFlow()

    /** Called only while the process-wide binding writer excludes every old response mutation. */
    internal fun quarantineBindingState() {
        mutableState.value = stateFrom(ApiConnectionSnapshot(null, false, null, null))
    }

    suspend fun refresh(): SuggestionRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; cached proposals were not replaced.")
            return SuggestionRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        return operationMutex.withLock {
            val resolution = authenticatedConfiguration()
            if (resolution is ConfigurationResolution.Failed) return@withLock resolution.outcome
            val configuration = (resolution as ConfigurationResolution.Ready).configuration
            updateBusy("Refreshing Suggestions Inbox…")
            try {
                configuration.withBindingOperation {
                    val remoteSuggestions = transport.list(configuration)
                    if (
                        remoteSuggestions.map(RemoteSuggestion::id).distinct().size !=
                        remoteSuggestions.size
                    ) {
                        throw RemoteSuggestionMappingException()
                    }
                    val suggestions = remoteSuggestions.map(::toPlanningSuggestion)
                    val persistence = plannerStore.replaceRemoteSuggestions(suggestions)
                    if (persistence == null || !persistence.awaitDurable()) {
                        updateError(
                            "Encrypted planner storage is unavailable; cached proposals were not replaced.",
                        )
                        return@withBindingOperation SuggestionRefreshOutcome.LOCAL_STORAGE_FAILURE
                    }
                    markSuccessful("Suggestions are up to date.")
                }
            } catch (error: Throwable) {
                handleFailure(error, "Suggestions could not be refreshed.")
            }
        }
    }

    suspend fun accept(id: String) {
        val suggestion = plannerStore.state.value.suggestions.firstOrNull { it.id == id } ?: return
        val revision = suggestion.remoteRevision
        if (revision == null) {
            plannerStore.approveSuggestion(id)
            return
        }
        if (suggestion.usesReservedChangeSetNamespace) {
            updateError(
                if (suggestion.isApplicationReady) {
                    "Review the exact typed changes and explicitly confirm them before applying."
                } else {
                    "This proposal uses a newer protected change-set format. Update DayWeave before applying it."
                },
            )
            return
        }
        mutateRemote(
            id,
            revision,
            "Accepting proposal…",
            "Accepted as a reviewable Inbox draft.",
            expectedStatus = "accepted",
        ) {
                configuration ->
            transport.accept(configuration, id, revision)
        }
    }

    suspend fun reject(id: String) {
        val suggestion = plannerStore.state.value.suggestions.firstOrNull { it.id == id } ?: return
        val revision = suggestion.remoteRevision
        if (revision == null) {
            plannerStore.rejectSuggestion(id)
            return
        }
        mutateRemote(
            id,
            revision,
            "Rejecting proposal…",
            "Suggestion rejected; the plan was unchanged.",
            expectedStatus = "rejected",
        ) {
                configuration ->
            transport.reject(configuration, id, revision)
        }
    }

    suspend fun edit(id: String, title: String, explanation: String) {
        val safeTitle = title.trim()
        val safeExplanation = explanation.trim()
        if (safeTitle.isEmpty() || safeExplanation.isEmpty()) return
        val suggestion = plannerStore.state.value.suggestions.firstOrNull { it.id == id } ?: return
        val revision = suggestion.remoteRevision
        if (revision == null) {
            plannerStore.updateSuggestion(id, safeTitle, safeExplanation)
            return
        }
        mutateRemote(
            id,
            revision,
            "Saving proposal…",
            "Proposal draft updated.",
            expectedStatus = "pending",
        ) { configuration ->
            transport.edit(
                configuration = configuration,
                id = id,
                expectedRevision = revision,
                title = safeTitle,
                explanation = safeExplanation,
            )
        }
    }

    suspend fun updateConnection(baseUrl: String, bearerToken: String?): Boolean {
        return operationMutex.withLock {
            try {
                credentialStore.update(baseUrl, bearerToken)
                mutableState.value = stateFrom(credentialStore.snapshot())
                true
            } catch (error: InvalidApiConfigurationException) {
                updateError(error.message ?: "The API connection settings are invalid.")
                false
            } catch (error: SecureCredentialException) {
                updateError("Secure API credentials could not be saved. Re-enter the token.")
                false
            } catch (error: IllegalStateException) {
                updateError("API connection settings could not be saved on this device.")
                false
            }
        }
    }

    suspend fun clearConnection(): Boolean = operationMutex.withLock {
        try {
            credentialStore.clear()
            mutableState.value = stateFrom(credentialStore.snapshot())
            true
        } catch (error: IllegalStateException) {
            updateError("API connection settings could not be removed from this device.")
            false
        }
    }

    internal suspend fun reportCredentialClearBlocked() = operationMutex.withLock {
        updateError("Background refresh could not be stopped, so API credentials were not removed.")
    }

    private suspend fun mutateRemote(
        id: String,
        expectedRevision: Long,
        progressMessage: String,
        successMessage: String,
        expectedStatus: String,
        operation: suspend (AuthenticatedApiConfiguration) -> RemoteSuggestion,
    ) = operationMutex.withLock {
        val latest = plannerStore.state.value.suggestions.firstOrNull { it.id == id }
        if (latest?.remoteRevision != expectedRevision) {
            updateError("This proposal changed locally. Refresh before trying again.")
            return@withLock
        }
        val resolution = authenticatedConfiguration()
        if (resolution !is ConfigurationResolution.Ready) return@withLock
        val configuration = resolution.configuration
        updateBusy(progressMessage)
        try {
            configuration.withBindingOperation {
                val response = operation(configuration)
                if (
                    response.id != id ||
                    response.revision <= expectedRevision ||
                    response.status != expectedStatus
                ) {
                    throw RemoteSuggestionMappingException()
                }
                val reconciled = toPlanningSuggestion(response)
                val persistence = plannerStore.reconcileRemoteSuggestion(reconciled)
                if (persistence == null || !persistence.awaitDurable()) {
                    updateError(
                        "Encrypted planner storage is unavailable; the server result was not cached.",
                    )
                    return@withBindingOperation
                }
                markSuccessful(successMessage)
            }
        } catch (error: Throwable) {
            handleFailure(error, "The proposal could not be updated.")
        }
    }

    private fun authenticatedConfiguration(): ConfigurationResolution {
        val snapshot = credentialStore.snapshot()
        if (snapshot.baseUrl == null) {
            mutableState.value = stateFrom(snapshot)
            return ConfigurationResolution.Failed(SuggestionRefreshOutcome.NOT_CONFIGURED)
        }
        if (!snapshot.hasBearerToken) {
            mutableState.value = stateFrom(snapshot)
            return ConfigurationResolution.Failed(SuggestionRefreshOutcome.AUTH_REQUIRED)
        }
        return try {
            val configuration = credentialStore.authenticatedConfiguration()
            if (configuration == null) {
                mutableState.value = stateFrom(snapshot)
                ConfigurationResolution.Failed(SuggestionRefreshOutcome.AUTH_REQUIRED)
            } else {
                ConfigurationResolution.Ready(configuration)
            }
        } catch (error: SecureCredentialException) {
            mutableState.value = SuggestionSyncState(
                phase = SuggestionSyncPhase.AUTH_REQUIRED,
                message = "The encrypted bearer token is unavailable. Re-enter it to reconnect.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
            ConfigurationResolution.Failed(SuggestionRefreshOutcome.AUTH_REQUIRED)
        } catch (error: InvalidApiConfigurationException) {
            mutableState.value = failureState(
                snapshot,
                "The stored API URL is invalid. Update the connection settings.",
            )
            ConfigurationResolution.Failed(SuggestionRefreshOutcome.CONFIGURATION_ERROR)
        } catch (error: IllegalStateException) {
            mutableState.value = failureState(
                snapshot,
                "Secure API credentials are unavailable on this device.",
            )
            ConfigurationResolution.Failed(SuggestionRefreshOutcome.CONFIGURATION_ERROR)
        }
    }

    private fun markSuccessful(message: String): SuggestionRefreshOutcome {
        val now = nowEpochMillis()
        val metadataSaved = runCatching { credentialStore.recordSuccessfulSync(now) }.isSuccess
        val snapshot = credentialStore.snapshot()
        mutableState.value = SuggestionSyncState(
            phase = if (metadataSaved) SuggestionSyncPhase.CONNECTED else SuggestionSyncPhase.ERROR,
            message = if (metadataSaved) {
                message
            } else {
                "$message Last-sync metadata could not be saved."
            },
            baseUrl = snapshot.baseUrl,
            hasStoredToken = snapshot.hasBearerToken,
            lastSuccessfulSyncEpochMillis = now,
        )
        return if (metadataSaved) {
            SuggestionRefreshOutcome.SUCCESS
        } else {
            SuggestionRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
    }

    private fun handleFailure(
        error: Throwable,
        fallbackMessage: String,
    ): SuggestionRefreshOutcome {
        if (error is CancellationException) {
            // WorkManager can stop a running worker when constraints change, work is replaced, or
            // its execution window ends. Never leave the process-wide UI state permanently busy.
            mutableState.value = stateFrom(credentialStore.snapshot())
            throw error
        }
        if (error is ApiBindingChangedException) {
            val snapshot = credentialStore.snapshot()
            mutableState.value = stateFrom(snapshot)
            return if (snapshot.hasBearerToken) {
                SuggestionRefreshOutcome.CONFIGURATION_ERROR
            } else {
                SuggestionRefreshOutcome.NOT_CONFIGURED
            }
        }
        val snapshot = credentialStore.snapshot()
        val (state, outcome) = when (error) {
            is SuggestionApiException.Authentication -> SuggestionSyncState(
                phase = SuggestionSyncPhase.AUTH_REQUIRED,
                message = "Authentication failed. Check or replace the stored bearer token.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            ) to SuggestionRefreshOutcome.AUTH_REQUIRED
            is SuggestionApiException.Conflict -> failureState(
                snapshot,
                "This proposal changed on the server. Refresh and review the latest version.",
            ) to SuggestionRefreshOutcome.PERMANENT_SERVER_FAILURE
            is SuggestionApiException.InvalidResponse -> failureState(
                snapshot,
                "The server response was not compatible with this version of DayWeave.",
            ) to SuggestionRefreshOutcome.PROTOCOL_FAILURE
            is SuggestionApiException.Http -> failureState(
                snapshot,
                "The DayWeave API returned HTTP ${error.statusCode}. Try again later.",
            ) to if (
                error.statusCode == 408 ||
                error.statusCode == 425 ||
                error.statusCode == 429 ||
                error.statusCode in 500..599
            ) {
                SuggestionRefreshOutcome.RETRYABLE_SERVER_FAILURE
            } else {
                SuggestionRefreshOutcome.PERMANENT_SERVER_FAILURE
            }
            is RemoteSuggestionMappingException -> failureState(
                snapshot,
                "The server returned a proposal this version of DayWeave cannot read.",
            ) to SuggestionRefreshOutcome.PROTOCOL_FAILURE
            is IOException -> SuggestionSyncState(
                phase = SuggestionSyncPhase.OFFLINE,
                message = "Offline or unable to reach the API. Showing the encrypted cached Inbox.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            ) to SuggestionRefreshOutcome.TRANSIENT_NETWORK_FAILURE
            else -> failureState(snapshot, fallbackMessage) to SuggestionRefreshOutcome.UNEXPECTED_FAILURE
        }
        mutableState.value = state
        return outcome
    }

    private fun updateBusy(message: String) {
        val snapshot = credentialStore.snapshot()
        mutableState.value = SuggestionSyncState(
            phase = SuggestionSyncPhase.SYNCING,
            message = message,
            baseUrl = snapshot.baseUrl,
            hasStoredToken = snapshot.hasBearerToken,
            lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
        )
    }

    private fun updateError(message: String) {
        mutableState.value = failureState(credentialStore.snapshot(), message)
    }

    private fun toPlanningSuggestion(remote: RemoteSuggestion): PlanningSuggestion {
        if (remote.id.isBlank() || remote.revision <= 0 || remote.title.isBlank()) {
            throw RemoteSuggestionMappingException()
        }
        val expiration = try {
            Instant.parse(remote.expiresAt).toEpochMilli()
        } catch (error: DateTimeException) {
            throw RemoteSuggestionMappingException(error)
        }
        val remainingMillis = (expiration - nowEpochMillis()).coerceAtLeast(0)
        val expiresInDays = ((remainingMillis + MILLIS_PER_DAY - 1) / MILLIS_PER_DAY)
            .coerceAtMost(Int.MAX_VALUE.toLong())
            .toInt()
        val payloadSchema = (remote.payload["schema"] as? JsonPrimitive)
            ?.takeIf(JsonPrimitive::isString)
            ?.content
        val reservesTransactionalNamespace =
            payloadSchema?.startsWith("dayweave.proposal-change-set/") == true
        val disposition = when (remote.status) {
            "pending" -> SuggestionDisposition.PENDING
            "accepted" -> if (reservesTransactionalNamespace) {
                SuggestionDisposition.TRANSACTIONALLY_APPLIED
            } else {
                SuggestionDisposition.APPROVED_FOR_INBOX
            }
            "rejected" -> SuggestionDisposition.REJECTED
            "expired" -> SuggestionDisposition.EXPIRED
            else -> throw RemoteSuggestionMappingException()
        }
        return PlanningSuggestion(
            id = remote.id,
            title = remote.title,
            summary = remote.explanation?.takeIf(String::isNotBlank)
                ?: "Review the structured proposal details before accepting this draft.",
            source = when (remote.source) {
                "app_assistant" -> "DayWeave assistant"
                "chat_gpt" -> "ChatGPT"
                "codex" -> "Codex"
                "external_mcp" -> "External MCP client"
                else -> "External proposal"
            },
            kind = when (remote.kind) {
                "create_item" -> SuggestionKind.NEW_TASK
                "goal_breakdown" -> SuggestionKind.GOAL_BREAKDOWN
                "constraint_change" -> SuggestionKind.CONSTRAINT_CHANGE
                "update_item", "calendar_event", "schedule_plan", "recommendation" ->
                    SuggestionKind.SCHEDULE_CHANGE
                else -> SuggestionKind.SCHEDULE_CHANGE
            },
            expiresInDays = expiresInDays,
            disposition = disposition,
            remoteRevision = remote.revision,
            remotePayloadJson = remote.payload.toString(),
            remoteSourceReference = remote.sourceReference,
            remoteCreatedAt = remote.createdAt,
            remoteExpiresAt = remote.expiresAt,
            remotePayloadSchema = payloadSchema,
        )
    }

    private class RemoteSuggestionMappingException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid remote suggestion", cause)

    private sealed interface ConfigurationResolution {
        data class Ready(
            val configuration: AuthenticatedApiConfiguration,
        ) : ConfigurationResolution

        data class Failed(
            val outcome: SuggestionRefreshOutcome,
        ) : ConfigurationResolution
    }

    companion object {
        private const val MILLIS_PER_DAY = 24L * 60L * 60L * 1_000L

        private fun stateFrom(snapshot: ApiConnectionSnapshot): SuggestionSyncState = when {
            snapshot.baseUrl == null -> SuggestionSyncState(
                phase = SuggestionSyncPhase.NOT_CONFIGURED,
                message = "Add an HTTPS DayWeave API URL and bearer token to sync suggestions.",
                baseUrl = null,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
            !snapshot.hasBearerToken -> SuggestionSyncState(
                phase = SuggestionSyncPhase.AUTH_REQUIRED,
                message = "Add a bearer token to authenticate suggestion sync.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = false,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
            else -> SuggestionSyncState(
                phase = SuggestionSyncPhase.READY,
                message = "Ready to refresh suggestions; encrypted cached data remains available offline.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = true,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
        }

        private fun failureState(
            snapshot: ApiConnectionSnapshot,
            message: String,
        ) = SuggestionSyncState(
            phase = SuggestionSyncPhase.ERROR,
            message = message,
            baseUrl = snapshot.baseUrl,
            hasStoredToken = snapshot.hasBearerToken,
            lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
        )
    }
}
