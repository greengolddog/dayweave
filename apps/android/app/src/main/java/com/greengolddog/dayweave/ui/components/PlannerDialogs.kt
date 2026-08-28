package com.greengolddog.dayweave.ui.components

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AddTask
import androidx.compose.material.icons.outlined.Coffee
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PlanningSuggestion

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QuickCaptureSheet(
    onDismiss: () -> Unit,
    onCapture: (String, ItemKind) -> Boolean,
) {
    var title by remember { mutableStateOf("") }
    var kind by remember { mutableStateOf(ItemKind.TASK) }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Icon(Icons.Outlined.AddTask, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                Column {
                    Text("Quick capture", style = MaterialTheme.typography.titleLarge)
                    Text(
                        "Save now; clarify duration and constraints in Inbox.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
            OutlinedTextField(
                value = title,
                onValueChange = { title = it },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("What do you need to do?") },
                minLines = 2,
            )
            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                ItemKind.entries.forEach { option ->
                    FilterChip(
                        selected = option == kind,
                        onClick = { kind = option },
                        label = { Text(option.label) },
                    )
                }
            }
            Button(
                onClick = { if (onCapture(title, kind)) onDismiss() },
                enabled = title.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Add to Inbox")
            }
        }
    }
}

@Composable
fun PauseChooserDialog(
    onDismiss: () -> Unit,
    onPause: (Int?) -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(Icons.Outlined.Coffee, contentDescription = null) },
        title = { Text("Take a break") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(9.dp)) {
                Text("Choose a duration, or pause without an end time. DayWeave will hold later work tentatively.")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(5, 15, 30).forEach { minutes ->
                        OutlinedButton(onClick = { onPause(minutes) }) { Text("${minutes}m") }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onPause(null) }) { Text("No end time") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
fun EditSuggestionDialog(
    suggestion: PlanningSuggestion,
    onDismiss: () -> Unit,
    onSave: (String, String) -> Unit,
) {
    var title by remember(suggestion.id) { mutableStateOf(suggestion.title) }
    var summary by remember(suggestion.id) { mutableStateOf(suggestion.summary) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Edit proposal") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    "Editing still does not apply the proposal to your schedule.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = title,
                    onValueChange = { title = it },
                    label = { Text("Title") },
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = summary,
                    onValueChange = { summary = it },
                    label = { Text("Proposed change") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onSave(title, summary) },
                enabled = title.isNotBlank() && summary.isNotBlank(),
            ) { Text("Save draft") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
fun ApiConnectionDialog(
    currentBaseUrl: String,
    hasStoredToken: Boolean,
    onDismiss: () -> Unit,
    onSave: (baseUrl: String, bearerToken: String?) -> Unit,
    onForget: () -> Unit,
) {
    var baseUrl by remember(currentBaseUrl) { mutableStateOf(currentBaseUrl) }
    var bearerToken by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = {
            bearerToken = ""
            onDismiss()
        },
        title = { Text("DayWeave API connection") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    "Only HTTPS endpoints are allowed. The bearer token is encrypted with an Android Keystore key and is never added to the planner database.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = baseUrl,
                    onValueChange = { baseUrl = it },
                    modifier = Modifier.fillMaxWidth(),
                    label = { Text("API base URL") },
                    placeholder = { Text("https://api.example.com/") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(
                        autoCorrectEnabled = false,
                        keyboardType = KeyboardType.Uri,
                    ),
                )
                OutlinedTextField(
                    value = bearerToken,
                    onValueChange = { bearerToken = it },
                    modifier = Modifier.fillMaxWidth(),
                    label = { Text(if (hasStoredToken) "Replacement bearer token" else "Bearer token") },
                    supportingText = {
                        if (hasStoredToken) Text("Leave blank to keep the encrypted token already on this device.")
                    },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(
                        autoCorrectEnabled = false,
                        keyboardType = KeyboardType.Password,
                    ),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    val submittedToken = bearerToken.takeIf(String::isNotBlank)
                    bearerToken = ""
                    onSave(baseUrl, submittedToken)
                },
                enabled = baseUrl.trim().startsWith("https://", ignoreCase = true) &&
                    (hasStoredToken || bearerToken.isNotBlank()),
            ) { Text("Save & refresh") }
        },
        dismissButton = {
            Row {
                if (hasStoredToken) {
                    TextButton(
                        onClick = {
                            bearerToken = ""
                            onForget()
                        },
                    ) { Text("Forget") }
                }
                TextButton(
                    onClick = {
                        bearerToken = ""
                        onDismiss()
                    },
                ) { Text("Cancel") }
            }
        },
    )
}
