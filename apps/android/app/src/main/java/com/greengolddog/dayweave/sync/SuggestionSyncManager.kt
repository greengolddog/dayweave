package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.RemoteSuggestion
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.SuggestionApiException
import com.greengolddog.dayweave.network.SuggestionsTransport
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.DateTimeException
import java.time.Instant
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

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

class SuggestionSyncManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: SuggestionsTransport,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) {
    private val operationMutex = Mutex()
    private val initialRefreshStarted = AtomicBoolean(false)
    private val mutableState = MutableStateFlow(stateFrom(credentialStore.snapshot()))
    val state: StateFlow<SuggestionSyncState> = mutableState.asStateFlow()

    suspend fun refreshIfNeeded() {
        if (initialRefreshStarted.compareAndSet(false, true)) refresh()
    }

    suspend fun refresh() = operationMutex.withLock {
        val configuration = authenticatedConfiguration() ?: return@withLock
        updateBusy("Refreshing Suggestions Inbox…")
        try {
            val remoteSuggestions = transport.list(configuration)
            if (remoteSuggestions.map(RemoteSuggestion::id).distinct().size != remoteSuggestions.size) {
                throw RemoteSuggestionMappingException()
            }
            val suggestions = remoteSuggestions.map(::toPlanningSuggestion)
            if (!plannerStore.replaceRemoteSuggestions(suggestions)) {
                updateError("Encrypted planner storage is unavailable; cached proposals were not replaced.")
                return@withLock
            }
            markSuccessful("Suggestions are up to date.")
        } catch (error: Throwable) {
            handleFailure(error, "Suggestions could not be refreshed.")
        }
    }

    suspend fun accept(id: String) {
        val suggestion = plannerStore.state.value.suggestions.firstOrNull { it.id == id } ?: return
        val revision = suggestion.remoteRevision
        if (revision == null) {
            plannerStore.approveSuggestion(id)
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
        val configuration = authenticatedConfiguration() ?: return@withLock
        updateBusy(progressMessage)
        try {
            val response = operation(configuration)
            if (
                response.id != id ||
                response.revision <= expectedRevision ||
                response.status != expectedStatus
            ) {
                throw RemoteSuggestionMappingException()
            }
            val reconciled = toPlanningSuggestion(response)
            if (!plannerStore.reconcileRemoteSuggestion(reconciled)) {
                updateError("Encrypted planner storage is unavailable; the server result was not cached.")
                return@withLock
            }
            markSuccessful(successMessage)
        } catch (error: Throwable) {
            handleFailure(error, "The proposal could not be updated.")
        }
    }

    private fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        val snapshot = credentialStore.snapshot()
        if (snapshot.baseUrl == null) {
            mutableState.value = stateFrom(snapshot)
            return null
        }
        if (!snapshot.hasBearerToken) {
            mutableState.value = stateFrom(snapshot)
            return null
        }
        return try {
            credentialStore.authenticatedConfiguration().also { configuration ->
                if (configuration == null) mutableState.value = stateFrom(snapshot)
            }
        } catch (error: SecureCredentialException) {
            mutableState.value = SuggestionSyncState(
                phase = SuggestionSyncPhase.AUTH_REQUIRED,
                message = "The encrypted bearer token is unavailable. Re-enter it to reconnect.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
            null
        } catch (error: InvalidApiConfigurationException) {
            mutableState.value = failureState(
                snapshot,
                "The stored API URL is invalid. Update the connection settings.",
            )
            null
        } catch (error: IllegalStateException) {
            mutableState.value = failureState(
                snapshot,
                "Secure API credentials are unavailable on this device.",
            )
            null
        }
    }

    private fun markSuccessful(message: String) {
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
    }

    private fun handleFailure(error: Throwable, fallbackMessage: String) {
        if (error is CancellationException) throw error
        val snapshot = credentialStore.snapshot()
        mutableState.value = when (error) {
            is SuggestionApiException.Authentication -> SuggestionSyncState(
                phase = SuggestionSyncPhase.AUTH_REQUIRED,
                message = "Authentication failed. Check or replace the stored bearer token.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
            is SuggestionApiException.Conflict -> failureState(
                snapshot,
                "This proposal changed on the server. Refresh and review the latest version.",
            )
            is SuggestionApiException.InvalidResponse -> failureState(
                snapshot,
                "The server response was not compatible with this version of DayWeave.",
            )
            is SuggestionApiException.Http -> failureState(
                snapshot,
                "The DayWeave API returned HTTP ${error.statusCode}. Try again later.",
            )
            is RemoteSuggestionMappingException -> failureState(
                snapshot,
                "The server returned a proposal this version of DayWeave cannot read.",
            )
            is IOException -> SuggestionSyncState(
                phase = SuggestionSyncPhase.OFFLINE,
                message = "Offline or unable to reach the API. Showing the encrypted cached Inbox.",
                baseUrl = snapshot.baseUrl,
                hasStoredToken = snapshot.hasBearerToken,
                lastSuccessfulSyncEpochMillis = snapshot.lastSuccessfulSyncEpochMillis,
            )
            else -> failureState(snapshot, fallbackMessage)
        }
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
        val disposition = when (remote.status) {
            "pending" -> SuggestionDisposition.PENDING
            "accepted" -> SuggestionDisposition.APPROVED_FOR_INBOX
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
        )
    }

    private class RemoteSuggestionMappingException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid remote suggestion", cause)

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
