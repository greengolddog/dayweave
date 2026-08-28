package com.greengolddog.dayweave.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.state.DayWeaveViewModel
import com.greengolddog.dayweave.ui.components.ActiveSessionBar
import com.greengolddog.dayweave.ui.components.EditSuggestionDialog
import com.greengolddog.dayweave.ui.components.PauseChooserDialog
import com.greengolddog.dayweave.ui.components.QuickCaptureSheet
import com.greengolddog.dayweave.ui.navigation.DayWeaveNavigationBar
import com.greengolddog.dayweave.ui.screens.AssistantScreen
import com.greengolddog.dayweave.ui.screens.CalendarScreen
import com.greengolddog.dayweave.ui.screens.InboxScreen
import com.greengolddog.dayweave.ui.screens.MoreScreen
import com.greengolddog.dayweave.ui.screens.TodayScreen
import com.greengolddog.dayweave.ui.theme.DayWeaveTheme

@Composable
fun DayWeaveApp(viewModel: DayWeaveViewModel = viewModel()) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    DayWeaveTheme(useDynamicColor = state.useDynamicColor) {
        DayWeaveRoot(viewModel = viewModel)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DayWeaveRoot(viewModel: DayWeaveViewModel) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    var showQuickCapture by remember { mutableStateOf(false) }
    var showPauseChooser by remember { mutableStateOf(false) }
    var editingSuggestion by remember { mutableStateOf<PlanningSuggestion?>(null) }

    Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(if (state.destination == AppDestination.TODAY) "DayWeave" else state.destination.label)
                        if (state.destination == AppDestination.TODAY) {
                            Text(
                                "Your day, composed around you",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                },
                actions = {
                    if (state.destination == AppDestination.TODAY || state.destination == AppDestination.CALENDAR) {
                        IconButton(onClick = viewModel::recompose) {
                            Icon(Icons.Outlined.AutoAwesome, contentDescription = "Recompose schedule")
                        }
                    }
                    Icon(
                        Icons.Outlined.CloudDone,
                        contentDescription = "Synced",
                        tint = MaterialTheme.colorScheme.secondary,
                    )
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
        bottomBar = {
            Column {
                val active = state.activeItem
                val session = state.activeSession
                AnimatedVisibility(visible = active != null && session != null) {
                    if (active != null && session != null) {
                        ActiveSessionBar(
                            item = active,
                            session = session,
                            onPause = { showPauseChooser = true },
                            onResume = viewModel::resumeActive,
                            onComplete = viewModel::completeActive,
                        )
                    }
                }
                DayWeaveNavigationBar(
                    selected = state.destination,
                    pendingSuggestionCount = state.pendingSuggestionCount,
                    onSelect = viewModel::navigate,
                )
            }
        },
        floatingActionButton = {
            if (state.destination != AppDestination.ASSISTANT) {
                FloatingActionButton(onClick = { showQuickCapture = true }) {
                    Icon(Icons.Outlined.Add, contentDescription = "Quick capture")
                }
            }
        },
    ) { innerPadding ->
        when (state.destination) {
            AppDestination.TODAY -> TodayScreen(
                state = state,
                onStart = viewModel::startItem,
                onPause = { showPauseChooser = true },
                onResume = viewModel::resumeActive,
                onComplete = viewModel::completeActive,
                onSkip = viewModel::skipActive,
                onLater = viewModel::doActiveLater,
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.CALENDAR -> CalendarScreen(
                state = state,
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.INBOX -> InboxScreen(
                state = state,
                onApprove = viewModel::approveSuggestion,
                onReject = viewModel::rejectSuggestion,
                onEdit = { editingSuggestion = it },
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.ASSISTANT -> AssistantScreen(
                state = state,
                onSend = viewModel::sendAssistantMessage,
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.MORE -> MoreScreen(
                state = state,
                onToggleCompleted = viewModel::toggleCompleted,
                onToggleQuietSuggestions = viewModel::toggleQuietSuggestions,
                onToggleDynamicColor = viewModel::toggleDynamicColor,
                modifier = Modifier.padding(innerPadding),
            )
        }
    }

    if (showQuickCapture) {
        QuickCaptureSheet(
            onDismiss = { showQuickCapture = false },
            onCapture = viewModel::quickCapture,
        )
    }

    if (showPauseChooser) {
        PauseChooserDialog(
            onDismiss = { showPauseChooser = false },
            onPause = { minutes ->
                viewModel.pauseActive(minutes)
                showPauseChooser = false
            },
        )
    }

    editingSuggestion?.let { suggestion ->
        EditSuggestionDialog(
            suggestion = suggestion,
            onDismiss = { editingSuggestion = null },
            onSave = { title, summary ->
                viewModel.updateSuggestion(suggestion.id, title, summary)
                editingSuggestion = null
            },
        )
    }
}
