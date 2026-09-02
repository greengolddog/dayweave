package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.assistant.AssistantContext
import com.greengolddog.dayweave.assistant.AssistantContextProjectionException
import com.greengolddog.dayweave.assistant.AssistantContextProjector
import com.greengolddog.dayweave.assistant.AssistantHistoryMessage
import com.greengolddog.dayweave.assistant.AssistantRole
import com.greengolddog.dayweave.assistant.AssistantTurnRequest
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_HISTORY_BYTES
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_HISTORY_ENTRIES
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_MESSAGE_BYTES
import com.greengolddog.dayweave.assistant.isValidAssistantConversationText
import com.greengolddog.dayweave.assistant.utf8Size
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AssistantApiException
import com.greengolddog.dayweave.network.AssistantTransport
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeviceAuthenticationChangedException
import com.greengolddog.dayweave.network.DeviceAuthenticationRequiredException
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerPersistenceReceipt
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import java.util.ArrayDeque
import java.util.UUID
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

enum class AssistantPhase {
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    READY,
    SENDING,
    OFFLINE,
    ERROR,
}

data class AssistantDisclosureSummary(
    val publicScheduledBlocks: Int,
    val privateBusySpans: Int,
    val plannerItems: Int,
    val omittedFields: Int,
)

data class AssistantState(
    val phase: AssistantPhase,
    val message: String,
    val disclosure: AssistantDisclosureSummary? = null,
    val model: String? = null,
    val completedAt: String? = null,
) {
    val isBusy: Boolean get() = phase == AssistantPhase.SENDING
}

private data class AssistantHistoryBinding(
    val baseUrl: String,
    val configurationId: String,
) {
    companion object {
        fun from(snapshot: ApiConnectionSnapshot): AssistantHistoryBinding? {
            val baseUrl = snapshot.baseUrl ?: return null
            val configurationId = snapshot.configurationId ?: return null
            return AssistantHistoryBinding(baseUrl, configurationId)
        }
    }
}

private data class CompletedAssistantTurn(
    val userMessageId: String,
    val assistantMessageId: String,
)

private data class StagedAssistantUser(
    val history: List<AssistantHistoryMessage>,
    val persistence: PlannerPersistenceReceipt?,
)

private data class ReplyCommitAttempt(
    val persistence: PlannerPersistenceReceipt?,
)

/**
 * Owns the only Android model-inference path.
 *
 * Turns are manual, single-flight, foreground-only, and advisory. The exact user message reaches
 * encrypted storage before the provider is called; failures are never replayed automatically.
 */
class AssistantManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: AssistantTransport,
    private val scope: CoroutineScope,
    private val operationAllowed: () -> Boolean = { true },
    private val now: () -> Instant = Instant::now,
    private val newUuid: () -> UUID = UUID::randomUUID,
    private val beforeReplyCommit: suspend () -> Unit = {},
) {
    private val presentationLock = Any()
    private var presentationGeneration = 1L
    private var presentationEnabled = operationAllowed()
    private var activeJob: Job? = null
    private var reusableHistoryBinding: AssistantHistoryBinding? = null
    private val reusableHistoryPairs = ArrayDeque<CompletedAssistantTurn>()
    private val mutableState = MutableStateFlow(
        stateFrom(
            credentialStore.snapshot().takeIf { operationAllowed() } ?: QUARANTINED_SNAPSHOT,
        ),
    )
    val state: StateFlow<AssistantState> = mutableState.asStateFlow()

    /** Returns true only when this exact message was admitted as the sole foreground turn. */
    fun send(text: String): Boolean {
        val message = text.trim()
        if (!message.isValidAssistantConversationText(MAX_ASSISTANT_MESSAGE_BYTES)) {
            publishWhileIdleAndEnabled(errorState("Messages must be between 1 byte and 8 KiB."))
            return false
        }
        if (!operationAllowed()) return false
        val connection = credentialStore.snapshot()
        if (connection.baseUrl == null || !connection.hasBearerToken) {
            publishWhileIdleAndEnabled(stateFrom(connection))
            return false
        }
        synchronized(presentationLock) {
            if (!presentationEnabled || !operationAllowed() || activeJob?.isActive == true) {
                return false
            }
            val generation = presentationGeneration
            val job = scope.launch(start = CoroutineStart.LAZY) {
                try {
                    performTurn(generation, message)
                } finally {
                    val current = coroutineContext[Job]
                    synchronized(presentationLock) {
                        if (activeJob === current) activeJob = null
                    }
                }
            }
            activeJob = job
            job.start()
            return true
        }
    }

    /** Stops inference and invalidates every callback, including one arriving after a later unlock. */
    fun cancelForPrivacyBoundary() {
        val job = synchronized(presentationLock) {
            presentationGeneration = Math.incrementExact(presentationGeneration)
            presentationEnabled = false
            clearReusableHistoryLocked()
            mutableState.value = stateFrom(QUARANTINED_SNAPSHOT)
            activeJob.also { activeJob = null }
        }
        job?.cancel()
    }

    /** Credential replacement uses the same generation fence as a privacy lock. */
    internal fun quarantineBindingState() = cancelForPrivacyBoundary()

    /** Called only after unlocked foreground presentation becomes authoritative again. */
    fun restoreForegroundState() {
        synchronized(presentationLock) {
            if (!operationAllowed()) return
            presentationEnabled = true
            if (activeJob?.isActive == true) return
            mutableState.value = stateFrom(credentialStore.snapshot())
        }
    }

    fun cancelActiveTurn() {
        val job = synchronized(presentationLock) {
            presentationGeneration = Math.incrementExact(presentationGeneration)
            mutableState.value = if (presentationEnabled && operationAllowed()) {
                stateFrom(credentialStore.snapshot()).copy(
                    message = "Assistant turn stopped. No provider change was applied.",
                )
            } else {
                stateFrom(QUARANTINED_SNAPSHOT)
            }
            activeJob.also { activeJob = null }
        }
        job?.cancel()
    }

    private suspend fun performTurn(generation: Long, message: String) {
        val load = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (load != PlannerLoadState.READY) {
            publish(generation, errorState("Encrypted planner storage is unavailable."))
            return
        }
        val binding = credentialStore.snapshot()
        val configuration = authenticatedConfiguration(binding, generation) ?: return
        val ticket = try {
            configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            publish(generation, stateFrom(credentialStore.snapshot()))
            return
        }
        try {
            if (!operationCurrent(generation, binding, configuration)) return
            val snapshot = plannerStore.state.value
            val context = try {
                AssistantContextProjector.project(snapshot, now())
            } catch (_: AssistantContextProjectionException) {
                publish(generation, errorState("The redacted planning context is too large."))
                return
            }
            val disclosure = context.disclosureSummary()
            if (!publish(
                    generation,
                    AssistantState(
                        phase = AssistantPhase.SENDING,
                        message = disclosure.progressMessage(),
                        disclosure = disclosure,
                    ),
                )
            ) return

            val requestId = newUuid().toString()
            val userMessageId = newUuid().toString()
            val assistantMessageId = newUuid().toString()
            val stagedUser = synchronized(presentationLock) {
                if (!operationCurrentLocked(generation, binding, configuration)) {
                    null
                } else {
                    StagedAssistantUser(
                        history = boundedReusableHistoryLocked(snapshot.messages, binding),
                        persistence = plannerStore.appendAssistantUserMessageDurably(
                            userMessageId,
                            message,
                        ),
                    )
                }
            } ?: return
            if (stagedUser.persistence == null || !stagedUser.persistence.awaitDurable()) {
                publish(generation, errorState("The message could not be saved securely."))
                return
            }
            if (!operationCurrent(generation, binding, configuration)) return
            val currentContext = runCatching {
                AssistantContextProjector.project(
                    plannerStore.state.value,
                    Instant.parse(context.generatedAt),
                )
            }.getOrNull()
            if (currentContext != context) {
                publish(
                    generation,
                    errorState(
                        "The plan changed before the request was sent. Review it and ask again.",
                    ),
                )
                return
            }

            val response = try {
                transport.turn(
                    configuration,
                    AssistantTurnRequest(
                        requestId = requestId,
                        message = message,
                        history = stagedUser.history,
                        context = context,
                    ),
                )
            } catch (error: Throwable) {
                handleFailure(generation, error, disclosure)
                return
            }
            if (!operationCurrent(generation, binding, configuration)) return
            if (response.requestId != requestId) {
                publish(
                    generation,
                    errorState("The assistant returned a response for another turn."),
                )
                return
            }
            // A deterministic race seam for lifecycle tests; no private data is exposed to it.
            beforeReplyCommit()
            val replyCommit = synchronized(presentationLock) {
                if (!operationCurrentLocked(generation, binding, configuration)) {
                    null
                } else {
                    ReplyCommitAttempt(
                        persistence = try {
                            plannerStore.appendAssistantReplyDurably(
                                userMessageId = userMessageId,
                                messageId = assistantMessageId,
                                text = response.reply,
                            )
                        } catch (_: IllegalArgumentException) {
                            null
                        },
                    )
                }
            } ?: return
            if (replyCommit.persistence == null || !replyCommit.persistence.awaitDurable()) {
                publish(
                    generation,
                    errorState("The reply arrived but could not be saved securely."),
                )
                return
            }
            val historyRecorded = synchronized(presentationLock) {
                if (!operationCurrentLocked(generation, binding, configuration)) {
                    false
                } else {
                    recordReusableHistoryLocked(binding, userMessageId, assistantMessageId)
                    true
                }
            }
            if (!historyRecorded) return
            runCatching { credentialStore.recordSuccessfulSync(System.currentTimeMillis()) }
            publish(
                generation,
                AssistantState(
                    phase = AssistantPhase.READY,
                    message = "Reply complete. No planner or calendar change was applied.",
                    disclosure = disclosure,
                    model = response.model,
                    completedAt = response.generatedAt,
                ),
            )
        } catch (error: CancellationException) {
            if (operationCurrent(generation)) {
                publish(generation, stateFrom(credentialStore.snapshot()))
            }
            throw error
        } catch (error: Throwable) {
            handleFailure(generation, error, null)
        } finally {
            ticket.release()
        }
    }

    private fun authenticatedConfiguration(
        binding: ApiConnectionSnapshot,
        generation: Long,
    ): AuthenticatedApiConfiguration? = try {
        credentialStore.authenticatedConfiguration()?.also { configuration ->
            if (!configurationMatchesBinding(configuration, binding)) {
                publish(generation, stateFrom(credentialStore.snapshot()))
                return null
            }
        } ?: run {
            publish(generation, stateFrom(binding))
            null
        }
    } catch (_: SecureCredentialException) {
        publish(
            generation,
            AssistantState(
                phase = AssistantPhase.AUTH_REQUIRED,
                message = "The encrypted API credential is unavailable. Reconnect this device.",
            ),
        )
        null
    } catch (_: InvalidApiConfigurationException) {
        publish(generation, errorState("The stored API URL is invalid."))
        null
    } catch (_: IllegalStateException) {
        publish(generation, errorState("Secure API credentials are unavailable on this device."))
        null
    }

    private fun handleFailure(
        generation: Long,
        error: Throwable,
        disclosure: AssistantDisclosureSummary?,
    ) {
        if (error is CancellationException) throw error
        val state = when (error) {
            is ApiBindingChangedException -> stateFrom(credentialStore.snapshot())
            is DeviceAuthenticationRequiredException -> AssistantState(
                phase = AssistantPhase.AUTH_REQUIRED,
                message = "Device authentication expired. Reconnect the DayWeave API.",
            )
            is DeviceAuthenticationChangedException -> stateFrom(credentialStore.snapshot()).copy(
                message = "Device authentication changed. Ask again manually.",
            )
            is AssistantApiException.Authentication -> AssistantState(
                phase = AssistantPhase.AUTH_REQUIRED,
                message = "Authentication failed. Reconnect the DayWeave API.",
            )
            is AssistantApiException.Forbidden -> errorState(
                "This device session does not have assistant read access.",
            )
            is AssistantApiException.RateLimited -> errorState(
                "The assistant rate limit was reached. Try again manually later.",
            )
            is AssistantApiException.Unavailable -> errorState(
                "The assistant provider is unavailable. Your message is saved; it was not retried.",
            )
            is AssistantApiException.Validation,
            is AssistantApiException.InvalidResponse,
            -> errorState("The server could not safely process this assistant turn.")
            is AssistantApiException.Http -> errorState(
                "The assistant API returned HTTP ${error.statusCode}. The turn was not retried.",
            )
            is IOException -> AssistantState(
                phase = AssistantPhase.OFFLINE,
                message = "Offline. Your message is saved locally and will not be resent automatically.",
                disclosure = disclosure,
            )
            is IllegalArgumentException -> errorState(
                "The local assistant request failed its safety limits.",
            )
            else -> errorState("The assistant turn failed safely and was not retried.")
        }
        publish(generation, state.copy(disclosure = state.disclosure ?: disclosure))
    }

    private fun operationCurrent(
        generation: Long,
        binding: ApiConnectionSnapshot? = null,
        configuration: AuthenticatedApiConfiguration? = null,
    ): Boolean = synchronized(presentationLock) {
        operationCurrentLocked(generation, binding, configuration)
    }

    private fun operationCurrentLocked(
        generation: Long,
        binding: ApiConnectionSnapshot? = null,
        configuration: AuthenticatedApiConfiguration? = null,
    ): Boolean {
        return presentationEnabled && presentationGeneration == generation && operationAllowed() &&
            (binding == null || sameBinding(binding, credentialStore.snapshot())) &&
            (
                binding == null || configuration == null ||
                    configurationMatchesBinding(configuration, binding)
            )
    }

    private fun publish(generation: Long, state: AssistantState): Boolean {
        return synchronized(presentationLock) {
            if (!operationCurrentLocked(generation)) {
                false
            } else {
                mutableState.value = state
                true
            }
        }
    }

    private fun publishWhileIdleAndEnabled(state: AssistantState): Boolean {
        return synchronized(presentationLock) {
            if (!presentationEnabled || !operationAllowed() || activeJob?.isActive == true) {
                false
            } else {
                mutableState.value = state
                true
            }
        }
    }

    /** Must be called while [presentationLock] is held. */
    private fun boundedReusableHistoryLocked(
        messages: List<ChatMessage>,
        binding: ApiConnectionSnapshot,
    ): List<AssistantHistoryMessage> {
        val historyBinding = AssistantHistoryBinding.from(binding)
        if (historyBinding == null) {
            clearReusableHistoryLocked()
            return emptyList()
        }
        if (reusableHistoryBinding != historyBinding) {
            clearReusableHistoryLocked()
            reusableHistoryBinding = historyBinding
        }

        val messagesById = messages.associateBy(ChatMessage::id)
        val newestPairs = ArrayDeque<List<AssistantHistoryMessage>>()
        var retainedBytes = 0
        var retainedEntries = 0
        for (pair in reusableHistoryPairs.toList().asReversed()) {
            val user = messagesById[pair.userMessageId]
            val assistant = messagesById[pair.assistantMessageId]
            if (user?.role != ChatRole.USER || assistant?.role != ChatRole.ASSISTANT) continue
            val userContent = user.text.trim()
            val assistantContent = assistant.text.trim()
            if (
                !userContent.isValidAssistantConversationText(MAX_ASSISTANT_MESSAGE_BYTES) ||
                !assistantContent.isValidAssistantConversationText(MAX_ASSISTANT_MESSAGE_BYTES)
            ) {
                continue
            }
            val pairBytes = userContent.utf8Size() + assistantContent.utf8Size()
            if (
                retainedEntries + 2 > MAX_ASSISTANT_HISTORY_ENTRIES ||
                retainedBytes > MAX_ASSISTANT_HISTORY_BYTES - pairBytes
            ) break
            newestPairs.addFirst(
                listOf(
                    AssistantHistoryMessage(AssistantRole.USER, userContent),
                    AssistantHistoryMessage(AssistantRole.ASSISTANT, assistantContent),
                ),
            )
            retainedEntries += 2
            retainedBytes += pairBytes
        }
        return newestPairs.flatten()
    }

    /** Must be called while [presentationLock] is held. */
    private fun recordReusableHistoryLocked(
        binding: ApiConnectionSnapshot,
        userMessageId: String,
        assistantMessageId: String,
    ) {
        val historyBinding = AssistantHistoryBinding.from(binding) ?: run {
            clearReusableHistoryLocked()
            return
        }
        if (reusableHistoryBinding != historyBinding) {
            clearReusableHistoryLocked()
            reusableHistoryBinding = historyBinding
        }
        reusableHistoryPairs.addLast(
            CompletedAssistantTurn(
                userMessageId = userMessageId,
                assistantMessageId = assistantMessageId,
            ),
        )
        while (reusableHistoryPairs.size > MAX_REUSABLE_HISTORY_PAIRS) {
            reusableHistoryPairs.removeFirst()
        }
    }

    /** Must be called while [presentationLock] is held. */
    private fun clearReusableHistoryLocked() {
        reusableHistoryBinding = null
        reusableHistoryPairs.clear()
    }

    private fun AssistantContext.disclosureSummary() = AssistantDisclosureSummary(
        publicScheduledBlocks = scheduledBlocks.size,
        privateBusySpans = privateBusySpans.size,
        plannerItems = plannerItems.size,
        omittedFields = omittedFields.size,
    )

    private fun AssistantDisclosureSummary.progressMessage(): String =
        "Sharing $publicScheduledBlocks public blocks, $privateBusySpans private busy spans, " +
            "and $plannerItems planner items."

    private fun errorState(message: String) = AssistantState(
        phase = AssistantPhase.ERROR,
        message = message,
    )

    private fun stateFrom(snapshot: ApiConnectionSnapshot): AssistantState = when {
        snapshot.baseUrl == null -> AssistantState(
            phase = AssistantPhase.NOT_CONFIGURED,
            message = "Connect the DayWeave API to use the Android assistant.",
        )
        !snapshot.hasBearerToken -> AssistantState(
            phase = AssistantPhase.AUTH_REQUIRED,
            message = "Reconnect this device to use the Android assistant.",
        )
        else -> AssistantState(
            phase = AssistantPhase.READY,
            message = "Ready. Sensitive details stay out of assistant context.",
        )
    }

    private fun sameBinding(first: ApiConnectionSnapshot, second: ApiConnectionSnapshot): Boolean =
        first.baseUrl == second.baseUrl && first.configurationId == second.configurationId &&
            first.hasBearerToken == second.hasBearerToken

    private fun configurationMatchesBinding(
        configuration: AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
    ): Boolean = configuration.baseUrl.toString() == binding.baseUrl &&
        configuration.configurationId == binding.configurationId

    private companion object {
        const val MAX_REUSABLE_HISTORY_PAIRS = MAX_ASSISTANT_HISTORY_ENTRIES / 2
        val QUARANTINED_SNAPSHOT = ApiConnectionSnapshot(null, false, null, null)
    }
}
