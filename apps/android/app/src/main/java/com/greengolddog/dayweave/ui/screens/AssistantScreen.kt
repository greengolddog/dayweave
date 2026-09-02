package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_MESSAGE_BYTES
import com.greengolddog.dayweave.assistant.isValidAssistantConversationText
import com.greengolddog.dayweave.sync.AssistantPhase
import com.greengolddog.dayweave.sync.AssistantState

@Composable
fun AssistantScreen(
    state: DayWeaveUiState,
    assistantState: AssistantState,
    onSend: (String) -> Boolean,
    onStop: () -> Unit,
    onConfigureConnection: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var draft by remember { mutableStateOf("") }
    val trimmedDraft = draft.trim()
    val draftBytes = trimmedDraft.toByteArray(Charsets.UTF_8).size
    val draftValid = trimmedDraft.isValidAssistantConversationText(MAX_ASSISTANT_MESSAGE_BYTES)
    val canSend = draftValid &&
        !assistantState.isBusy && assistantState.phase !in setOf(
            AssistantPhase.NOT_CONFIGURED,
            AssistantPhase.AUTH_REQUIRED,
        )
    Column(modifier = modifier.fillMaxSize().imePadding()) {
        Column(
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(9.dp)) {
                Icon(Icons.Outlined.AutoAwesome, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                Text("DayWeave Assistant", style = MaterialTheme.typography.headlineSmall)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Outlined.Lock,
                    contentDescription = null,
                    modifier = Modifier.padding(end = 2.dp),
                )
                Text(
                    "Schedule changes are proposals until you approve them.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            AssistantStatusCard(
                state = assistantState,
                onStop = onStop,
                onConfigureConnection = onConfigureConnection,
            )
        }

        LazyColumn(
            modifier = Modifier.weight(1f),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (state.messages.isEmpty()) {
                item {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(vertical = 32.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Icon(
                            Icons.Outlined.AutoAwesome,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.primary,
                        )
                        Text("Ask about today, a goal, or an overloaded week")
                        Text(
                            "The assistant can discuss your plan. It cannot directly change it.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            } else {
                item(key = "assistant-context-notice") {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(
                            modifier = Modifier.fillMaxWidth().padding(12.dp),
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            Text(
                                "Conversation context",
                                style = MaterialTheme.typography.labelLarge,
                            )
                            Text(
                                "Earlier messages stay visible on this device for reference. " +
                                    "Assistant context starts over after app lock, background, " +
                                    "restart, or API connection changes.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
            items(state.messages, key = { it.id }) { message ->
                MessageBubble(message)
            }
        }

        Surface(tonalElevation = 4.dp) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(12.dp),
                verticalAlignment = Alignment.Bottom,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    TextField(
                        value = draft,
                        onValueChange = { draft = it },
                        modifier = Modifier.fillMaxWidth(),
                        placeholder = { Text("Ask about your plan…") },
                        minLines = 1,
                        maxLines = 4,
                        enabled = !assistantState.isBusy,
                        isError = draft.isNotBlank() && !draftValid,
                        supportingText = if (draftBytes > MAX_ASSISTANT_MESSAGE_BYTES) {
                            {
                                Text(
                                    "$draftBytes / $MAX_ASSISTANT_MESSAGE_BYTES bytes",
                                )
                            }
                        } else if (draft.isNotBlank() && !draftValid) {
                            { Text("Remove unsupported control or malformed Unicode characters.") }
                        } else {
                            null
                        },
                        shape = RoundedCornerShape(24.dp),
                        colors = TextFieldDefaults.colors(
                            focusedIndicatorColor = Color.Transparent,
                            unfocusedIndicatorColor = Color.Transparent,
                        ),
                    )
                }
                FilledIconButton(
                    onClick = {
                        if (onSend(draft)) draft = ""
                    },
                    enabled = canSend,
                ) {
                    Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
                }
            }
        }
    }
}

@Composable
private fun AssistantStatusCard(
    state: AssistantState,
    onStop: () -> Unit,
    onConfigureConnection: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                when (state.phase) {
                    AssistantPhase.SENDING -> CircularProgressIndicator(
                        modifier = Modifier.size(20.dp),
                        strokeWidth = 2.dp,
                    )
                    AssistantPhase.OFFLINE -> Icon(
                        Icons.Outlined.CloudOff,
                        contentDescription = null,
                    )
                    AssistantPhase.ERROR,
                    AssistantPhase.AUTH_REQUIRED,
                    AssistantPhase.NOT_CONFIGURED,
                    -> Icon(Icons.Outlined.ErrorOutline, contentDescription = null)
                    AssistantPhase.READY -> Icon(
                        Icons.Outlined.AutoAwesome,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.primary,
                    )
                }
                Text(
                    state.message,
                    modifier = Modifier.weight(1f),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            if (state.isBusy) LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
            state.disclosure?.let { disclosure ->
                Text(
                    "Context · ${disclosure.publicScheduledBlocks} public blocks · " +
                        "${disclosure.privateBusySpans} private busy spans · " +
                        "${disclosure.plannerItems} planner items",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    "Sensitive titles, all notes, stable IDs, and raw constraints are omitted.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            state.model?.let { model ->
                Text(
                    "Model · $model",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            when {
                state.isBusy -> OutlinedButton(onClick = onStop) {
                    Icon(Icons.Outlined.StopCircle, contentDescription = null)
                    Spacer(Modifier.size(8.dp))
                    Text("Stop")
                }
                state.phase in setOf(
                    AssistantPhase.NOT_CONFIGURED,
                    AssistantPhase.AUTH_REQUIRED,
                ) -> Button(onClick = onConfigureConnection) {
                    Text("Configure connection")
                }
            }
        }
    }
}

@Composable
private fun MessageBubble(message: ChatMessage) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = if (message.role == ChatRole.USER) Arrangement.End else Arrangement.Start,
    ) {
        Surface(
            modifier = Modifier.fillMaxWidth(0.86f),
            shape = RoundedCornerShape(
                topStart = 18.dp,
                topEnd = 18.dp,
                bottomStart = if (message.role == ChatRole.ASSISTANT) 4.dp else 18.dp,
                bottomEnd = if (message.role == ChatRole.USER) 4.dp else 18.dp,
            ),
            color = if (message.role == ChatRole.USER) {
                MaterialTheme.colorScheme.primaryContainer
            } else {
                MaterialTheme.colorScheme.surfaceVariant
            },
        ) {
            Text(message.text, modifier = Modifier.padding(14.dp), style = MaterialTheme.typography.bodyLarge)
        }
    }
}
