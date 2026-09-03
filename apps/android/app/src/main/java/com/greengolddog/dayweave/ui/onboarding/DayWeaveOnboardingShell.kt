package com.greengolddog.dayweave.ui.onboarding

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.AddTask
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.DateRange
import androidx.compose.material.icons.outlined.Devices
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.RestartAlt
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Security
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.paneTitle
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.greengolddog.dayweave.onboarding.OnboardingStep

/**
 * An isolated, caller-controlled Android onboarding surface.
 *
 * This composable never owns navigation, readiness, credentials, provider details, or durable
 * progress. In particular, it uses no saveable state. The only remembered value is the ephemeral
 * visibility of the exact-reset confirmation dialog.
 */
@Composable
fun DayWeaveOnboardingShell(
    state: OnboardingUiState,
    callbacks: OnboardingCallbacks,
    modifier: Modifier = Modifier,
) {
    val presentedStep = state.presentedStep
    val privacyOnly = !state.privacyBoundaryOpen
    var showResetConfirmation by remember(state.recovery) { mutableStateOf(false) }

    Surface(
        modifier = modifier
            .fillMaxSize()
            .testTag(OnboardingTestTags.ROOT)
            .semantics { paneTitle = "DayWeave setup" },
        color = MaterialTheme.colorScheme.background,
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .background(MaterialTheme.colorScheme.background)
                .windowInsetsPadding(WindowInsets.safeDrawing),
        ) {
            OnboardingHeader(
                step = presentedStep,
                privacyOnly = privacyOnly,
            )
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f))

            LazyColumn(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .then(
                        if (privacyOnly) {
                            Modifier.testTag(OnboardingTestTags.OPAQUE_PRIVACY)
                        } else {
                            Modifier
                        },
                    ),
                verticalArrangement = Arrangement.spacedBy(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                item {
                    Box(
                        modifier = Modifier
                            .fillMaxWidth()
                            .widthIn(max = 720.dp)
                            .padding(horizontal = 20.dp, vertical = 28.dp),
                    ) {
                        OnboardingPage(
                            step = presentedStep,
                            state = state,
                            callbacks = callbacks,
                            onRequestReset = { showResetConfirmation = true },
                        )
                    }
                }
            }

            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.6f))
            OnboardingFooter(state = state, callbacks = callbacks)
        }
    }

    if (showResetConfirmation) {
        ExactResetConfirmation(
            recovery = state.recovery,
            onConfirm = {
                showResetConfirmation = false
                callbacks.onResetProgressAfterWarning()
            },
            onDismiss = { showResetConfirmation = false },
        )
    }
}

@Composable
private fun OnboardingHeader(
    step: OnboardingStep,
    privacyOnly: Boolean,
) {
    val position = step.ordinalInFlow + 1
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 20.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Box(
                modifier = Modifier
                    .size(40.dp)
                    .clip(RoundedCornerShape(12.dp))
                    .background(
                        Brush.linearGradient(
                            listOf(
                                MaterialTheme.colorScheme.primary,
                                MaterialTheme.colorScheme.tertiary,
                            ),
                        ),
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = Icons.Outlined.AutoAwesome,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onPrimary,
                )
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = "DayWeave",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                )
                Text(
                    text = if (privacyOnly) {
                        "Private setup"
                    } else {
                        step.title
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Surface(
                shape = RoundedCornerShape(100.dp),
                color = MaterialTheme.colorScheme.primaryContainer,
            ) {
                Text(
                    text = "$position of ${onboardingSteps.size}",
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
        }
        LinearProgressIndicator(
            progress = { position.toFloat() / onboardingSteps.size.toFloat() },
            modifier = Modifier
                .fillMaxWidth()
                .height(5.dp)
                .clip(CircleShape)
                .semantics {
                    stateDescription = "Setup step $position of ${onboardingSteps.size}"
                },
            trackColor = MaterialTheme.colorScheme.surfaceVariant,
        )
    }
}

@Composable
private fun OnboardingPage(
    step: OnboardingStep,
    state: OnboardingUiState,
    callbacks: OnboardingCallbacks,
    onRequestReset: () -> Unit,
) {
    when (step) {
        OnboardingStep.WELCOME -> WelcomePrivacyPage(
            acknowledged = state.privacyBoundaryOpen,
            recovery = state.recovery,
            onAcknowledgementChanged = callbacks.onPrivacyAcknowledgementChanged,
            onRequestReset = onRequestReset,
        )
        OnboardingStep.API -> ActionPage(
            step = step,
            eyebrow = "PRIVATE BACKEND",
            title = "Connect this phone",
            explanation = "Use a one-time code to enroll this Pixel with your private DayWeave API. Google and AI accounts remain separate.",
            check = state.readiness.api,
            actionLabel = if (state.readiness.api == OnboardingCheckState.READY) {
                "Review phone connection"
            } else {
                "Connect with a one-time code"
            },
            actionIcon = Icons.Outlined.Devices,
            onAction = callbacks.onConnectThisPhone,
            specialContent = { ApiConnectionGuidance() },
            note = "The secure enrollment flow handles the code. This setup screen never receives or retains it.",
        )
        OnboardingStep.GOOGLE -> ActionPage(
            step = step,
            eyebrow = "CALENDAR & TASKS",
            title = "Choose what helps shape your day",
            explanation = "Connect Google, then choose the calendars and task lists DayWeave may read for planning.",
            check = state.readiness.google,
            actionLabel = if (state.readiness.google == OnboardingCheckState.READY) {
                "Review Google resources"
            } else {
                "Choose Google resources"
            },
            actionIcon = Icons.Outlined.CalendarMonth,
            onAction = callbacks.onChooseGoogleResources,
            specialContent = { GoogleResourceGuidance() },
            note = "Publishing to Google Calendar is a separate, explicit approval. Connecting here does not publish anything.",
        )
        OnboardingStep.PROFILE -> ActionPage(
            step = step,
            eyebrow = "YOUR WEEK",
            title = "Give planning real boundaries",
            explanation = "Review the week DayWeave should work within, including your time zone, sleep, availability, and protected time.",
            check = state.readiness.profile,
            actionLabel = "Review week & profile",
            actionIcon = Icons.Outlined.DateRange,
            onAction = callbacks.onReviewWeekProfile,
            specialContent = { ProfileGuidance() },
            note = "These are visible scheduling constraints, not empty gaps. You can refine every value later.",
        )
        OnboardingStep.NOTIFICATIONS -> ActionPage(
            step = step,
            eyebrow = "CONTEXTUAL REMINDERS",
            title = "Ask only when it becomes useful",
            explanation = "DayWeave does not need notification permission during setup. The default is to ask when your first eligible reminder is actually needed.",
            check = state.readiness.notifications,
            actionLabel = "Open notification settings",
            actionIcon = Icons.Outlined.Notifications,
            onAction = callbacks.onOpenNotificationSettings,
            specialContent = { NotificationDefaultCard() },
            note = "Setup never invents a break or reminder to trigger permission, and sensitive lock-screen text stays redacted.",
        )
        OnboardingStep.FIRST_ITEM -> ActionPage(
            step = step,
            eyebrow = "CAPTURE",
            title = "Add something real to plan",
            explanation = "Create one reviewed Planned leaf item with a duration, or one fully timed event, so your first plan has real demand.",
            check = state.readiness.firstItem,
            actionLabel = if (state.readiness.firstItem == OnboardingCheckState.READY) {
                "Review first item"
            } else {
                "Create first item"
            },
            actionIcon = Icons.Outlined.AddTask,
            onAction = callbacks.onCreateFirstItem,
            specialContent = { FirstItemGuidance() },
            note = "The item and its opaque onboarding anchor belong in encrypted planner storage—not in setup progress.",
        )
        OnboardingStep.FIRST_PLAN -> ActionPage(
            step = step,
            eyebrow = "COMPOSE",
            title = "Publish one exact plan",
            explanation = "Sync the reviewed item, compose the deterministic seven-day schedule, and publish that exact revision to DayWeave.",
            check = state.readiness.firstPlan,
            actionLabel = if (state.readiness.firstPlan == OnboardingCheckState.READY) {
                "Review first plan"
            } else {
                "Compose first plan"
            },
            actionIcon = Icons.Outlined.AutoAwesome,
            onAction = callbacks.onComposeFirstPlan,
            specialContent = { FirstPlanGuidance() },
            note = "Only an exact published proof counts. Google Calendar still follows your selected source roles and approval rules.",
        )
        OnboardingStep.READY -> ReadyPage(state)
    }
}

@Composable
private fun WelcomePrivacyPage(
    acknowledged: Boolean,
    recovery: OnboardingRecoveryUiState,
    onAcknowledgementChanged: (Boolean) -> Unit,
    onRequestReset: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(OnboardingTestTags.page(OnboardingStep.WELCOME)),
        verticalArrangement = Arrangement.spacedBy(22.dp),
    ) {
        PageHero(
            icon = Icons.Outlined.Security,
            eyebrow = "WELCOME",
            title = "A calmer, executable day",
            explanation = "First, review what stays private and when DayWeave may act outside this phone.",
        )

        if (recovery != OnboardingRecoveryUiState.NONE) {
            RecoveryWarning(recovery = recovery, onRequestReset = onRequestReset)
        }

        FeatureRow(
            icon = Icons.Outlined.Lock,
            title = "Private by default",
            detail = "Planner data is encrypted locally. Sensitive content stays out of assistant context and lock-screen detail by default.",
        )
        FeatureRow(
            icon = Icons.Outlined.CloudOff,
            title = "Useful offline",
            detail = "Capture, viewing, execution, and local composition keep working without Google, AI, or the network.",
        )
        FeatureRow(
            icon = Icons.Outlined.CheckCircle,
            title = "You approve external effects",
            detail = "Publishing, destructive changes, and relaxed hard constraints cross an explicit review boundary.",
        )

        val acknowledgementEnabled = recovery == OnboardingRecoveryUiState.NONE
        Surface(
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(18.dp))
                .toggleable(
                    value = acknowledged,
                    enabled = acknowledgementEnabled,
                    role = Role.Checkbox,
                    onValueChange = onAcknowledgementChanged,
                )
                .testTag(OnboardingTestTags.PRIVACY_CHECKBOX)
                .semantics(mergeDescendants = true) {
                    stateDescription = if (acknowledged) "Acknowledged" else "Not acknowledged"
                },
            color = MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.55f),
        ) {
            Row(
                modifier = Modifier.padding(16.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Checkbox(
                    checked = acknowledged,
                    onCheckedChange = null,
                    enabled = acknowledgementEnabled,
                )
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = "I understand the privacy and approval boundaries",
                        style = MaterialTheme.typography.titleSmall,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = if (acknowledgementEnabled) {
                            "Required before connecting accounts or starting network work."
                        } else {
                            "Repair setup progress before this choice can be saved."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun ActionPage(
    step: OnboardingStep,
    eyebrow: String,
    title: String,
    explanation: String,
    check: OnboardingCheckState,
    actionLabel: String,
    actionIcon: ImageVector,
    onAction: () -> Unit,
    specialContent: @Composable () -> Unit,
    note: String,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(OnboardingTestTags.page(step)),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        PageHero(
            icon = step.icon,
            eyebrow = eyebrow,
            title = title,
            explanation = explanation,
        )
        specialContent()
        ReadinessCard(step = step, check = check)
        Button(
            onClick = onAction,
            enabled = check != OnboardingCheckState.WORKING,
            modifier = Modifier
                .fillMaxWidth()
                .height(52.dp)
                .testTag(OnboardingTestTags.PRIMARY_ACTION),
        ) {
            Icon(actionIcon, contentDescription = null)
            Spacer(Modifier.size(10.dp))
            Text(actionLabel)
        }
        InformationNote(note)
    }
}

@Composable
private fun PageHero(
    icon: ImageVector,
    eyebrow: String,
    title: String,
    explanation: String,
) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Surface(
            shape = RoundedCornerShape(14.dp),
            color = MaterialTheme.colorScheme.primaryContainer,
        ) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                modifier = Modifier.padding(12.dp).size(28.dp),
                tint = MaterialTheme.colorScheme.onPrimaryContainer,
            )
        }
        Text(
            text = eyebrow,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
            fontWeight = FontWeight.Bold,
            letterSpacing = 1.2.sp,
        )
        Text(
            text = title,
            style = MaterialTheme.typography.headlineLarge,
            modifier = Modifier.semantics { heading() },
        )
        Text(
            text = explanation,
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun FeatureRow(
    icon: ImageVector,
    title: String,
    detail: String,
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {},
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        border = CardDefaults.outlinedCardBorder(),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            horizontalArrangement = Arrangement.spacedBy(14.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Surface(
                shape = CircleShape,
                color = MaterialTheme.colorScheme.primaryContainer,
            ) {
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    modifier = Modifier.padding(9.dp).size(20.dp),
                    tint = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(title, style = MaterialTheme.typography.titleSmall)
                Text(
                    detail,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun ApiConnectionGuidance() {
    GuidanceCard {
        GuidanceLine(
            icon = Icons.Outlined.Devices,
            title = "Primary · One-time code",
            detail = "Create a short-lived code on an already trusted DayWeave device, then enter it in the secure enrollment flow.",
        )
        HorizontalDivider()
        GuidanceLine(
            icon = Icons.Outlined.Info,
            title = "Advanced bootstrap",
            detail = "Manual bootstrap remains available for recovery or administration, but is not the normal phone setup path.",
        )
    }
}

@Composable
private fun GoogleResourceGuidance() {
    GuidanceCard {
        GuidanceLine(
            icon = Icons.Outlined.CalendarMonth,
            title = "Google Calendar · read for planning",
            detail = "Selected busy events become constraints only after the current import completes.",
        )
        HorizontalDivider()
        GuidanceLine(
            icon = Icons.Outlined.AddTask,
            title = "Google Tasks · read selected lists",
            detail = "Choose the task lists that belong in DayWeave; leave unrelated lists unselected.",
        )
    }
}

@Composable
private fun ProfileGuidance() {
    GuidanceCard {
        GuidanceLine(
            icon = Icons.Outlined.Schedule,
            title = "Time zone, sleep & availability",
            detail = "Set the usable shape of weekdays and weekends before asking the scheduler to compose them.",
        )
        HorizontalDivider()
        GuidanceLine(
            icon = Icons.Outlined.Security,
            title = "Protected time & preferences",
            detail = "Review protected free time, energy, contexts, locations, and planning stability.",
        )
    }
}

@Composable
private fun NotificationDefaultCard() {
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {
                stateDescription = "Selected default"
            },
        color = MaterialTheme.colorScheme.secondaryContainer,
        shape = RoundedCornerShape(18.dp),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Icon(
                Icons.Outlined.CheckCircle,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSecondaryContainer,
            )
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    "Default · Ask when first needed",
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    "No permission prompt is launched by onboarding.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
            }
        }
    }
}

@Composable
private fun FirstItemGuidance() {
    GuidanceCard {
        GuidanceLine(
            icon = Icons.Outlined.AddTask,
            title = "Use a leaf item",
            detail = "A task with positive duration or a fully timed event creates concrete planning demand.",
        )
        HorizontalDivider()
        GuidanceLine(
            icon = Icons.Outlined.Lock,
            title = "Review before sync",
            detail = "Saving the item and its opaque anchor is atomic; its title never enters setup progress.",
        )
    }
}

@Composable
private fun FirstPlanGuidance() {
    GuidanceCard {
        GuidanceLine(
            icon = Icons.Outlined.Sync,
            title = "Current constraints",
            detail = "The exact item revision and current imported resources must be ready before composition.",
        )
        HorizontalDivider()
        GuidanceLine(
            icon = Icons.Outlined.CloudDone,
            title = "Exact publication proof",
            detail = "Continue only after the published schedule contains that reviewed revision in the current horizon.",
        )
    }
}

@Composable
private fun GuidanceCard(content: @Composable ColumnScope.() -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.42f),
        ),
    ) {
        Column(content = content)
    }
}

@Composable
private fun GuidanceLine(
    icon: ImageVector,
    title: String,
    detail: String,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(16.dp)
            .semantics(mergeDescendants = true) {},
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(
            icon,
            contentDescription = null,
            modifier = Modifier.size(22.dp),
            tint = MaterialTheme.colorScheme.primary,
        )
        Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun ReadinessCard(
    step: OnboardingStep,
    check: OnboardingCheckState,
) {
    val presentation = readinessPresentation(step, check)
    val color = when (check) {
        OnboardingCheckState.READY -> MaterialTheme.colorScheme.secondary
        OnboardingCheckState.NEEDS_ATTENTION -> MaterialTheme.colorScheme.error
        OnboardingCheckState.WORKING -> MaterialTheme.colorScheme.primary
        OnboardingCheckState.PENDING -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(OnboardingTestTags.READINESS)
            .semantics(mergeDescendants = true) {
                stateDescription = presentation.stateLabel
            },
        color = color.copy(alpha = 0.09f),
        shape = RoundedCornerShape(18.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, color.copy(alpha = 0.32f)),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            horizontalArrangement = Arrangement.spacedBy(13.dp),
            verticalAlignment = Alignment.Top,
        ) {
            if (check == OnboardingCheckState.WORKING) {
                CircularProgressIndicator(
                    modifier = Modifier.size(22.dp),
                    strokeWidth = 2.5.dp,
                    color = color,
                )
            } else {
                Icon(
                    imageVector = presentation.icon,
                    contentDescription = null,
                    tint = color,
                )
            }
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(
                    presentation.stateLabel,
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    color = color,
                )
                Text(
                    presentation.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun InformationNote(note: String) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.38f))
            .padding(14.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(
            Icons.Outlined.Info,
            contentDescription = null,
            modifier = Modifier.size(20.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            note,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ReadyPage(state: OnboardingUiState) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(OnboardingTestTags.page(OnboardingStep.READY)),
        verticalArrangement = Arrangement.spacedBy(22.dp),
    ) {
        PageHero(
            icon = Icons.Outlined.CheckCircle,
            eyebrow = "READY TO WEAVE",
            title = "Your planning workspace is ready",
            explanation = "Review the live checklist, then finish setup. Every setting remains editable as your week changes.",
        )

        Card(
            modifier = Modifier
                .fillMaxWidth()
                .testTag(OnboardingTestTags.READY_CHECKLIST),
            colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
            border = CardDefaults.outlinedCardBorder(),
        ) {
            ChecklistRow(
                title = "Privacy boundaries",
                ready = state.privacyAcknowledged,
                position = 0,
            )
            onboardingSteps
                .filter { it != OnboardingStep.WELCOME && it != OnboardingStep.READY }
                .forEachIndexed { index, step ->
                    HorizontalDivider(modifier = Modifier.padding(start = 52.dp))
                    ChecklistRow(
                        title = step.title,
                        ready = state.readiness.checkFor(step) == OnboardingCheckState.READY,
                        position = index + 1,
                    )
                }
        }

        InformationNote(
            "Finish records only the completion milestone. Credentials, Google resources, item content, and schedules remain in their protected authoritative stores.",
        )
    }
}

@Composable
private fun ChecklistRow(
    title: String,
    ready: Boolean,
    position: Int,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 14.dp)
            .semantics(mergeDescendants = true) {
                stateDescription = if (ready) "Ready" else "Needs attention"
            },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Surface(
            modifier = Modifier.size(28.dp),
            shape = CircleShape,
            color = if (ready) {
                MaterialTheme.colorScheme.secondary.copy(alpha = 0.13f)
            } else {
                MaterialTheme.colorScheme.surfaceVariant
            },
        ) {
            Box(contentAlignment = Alignment.Center) {
                if (ready) {
                    Icon(
                        Icons.Outlined.CheckCircle,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.secondary,
                        modifier = Modifier.size(19.dp),
                    )
                } else {
                    Text(
                        text = (position + 1).toString(),
                        style = MaterialTheme.typography.labelSmall,
                        textAlign = TextAlign.Center,
                    )
                }
            }
        }
        Text(title, modifier = Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
        Text(
            if (ready) "Ready" else "Review",
            style = MaterialTheme.typography.labelMedium,
            color = if (ready) {
                MaterialTheme.colorScheme.secondary
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
    }
}

@Composable
private fun RecoveryWarning(
    recovery: OnboardingRecoveryUiState,
    onRequestReset: () -> Unit,
) {
    val title = when (recovery) {
        OnboardingRecoveryUiState.CORRUPT -> "Setup progress is unreadable"
        OnboardingRecoveryUiState.UNSUPPORTED_FUTURE_VERSION ->
            "Setup progress came from a newer app version"
        OnboardingRecoveryUiState.NONE -> return
    }
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(OnboardingTestTags.RECOVERY_WARNING),
        color = MaterialTheme.colorScheme.errorContainer,
        shape = RoundedCornerShape(18.dp),
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Icon(
                    Icons.Outlined.WarningAmber,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onErrorContainer,
                )
                Text(
                    title,
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    color = MaterialTheme.colorScheme.onErrorContainer,
                )
            }
            Text(
                "DayWeave is staying offline and hiding private setup details until you choose an exact recovery action.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
            OutlinedButton(
                onClick = onRequestReset,
                modifier = Modifier.testTag(OnboardingTestTags.RECOVERY_RESET),
            ) {
                Icon(Icons.Outlined.RestartAlt, contentDescription = null)
                Spacer(Modifier.size(8.dp))
                Text("Reset setup progress only…")
            }
        }
    }
}

@Composable
private fun ExactResetConfirmation(
    recovery: OnboardingRecoveryUiState,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val source = when (recovery) {
        OnboardingRecoveryUiState.CORRUPT -> "unreadable"
        OnboardingRecoveryUiState.UNSUPPORTED_FUTURE_VERSION -> "newer-version"
        OnboardingRecoveryUiState.NONE -> "unavailable"
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(Icons.Outlined.ErrorOutline, contentDescription = null) },
        title = { Text("Reset only guided-setup progress?") },
        text = {
            Text(
                "This replaces the $source setup checkpoint. It does not remove planner data, accounts, credentials, Google recovery, permissions, or schedules.",
            )
        },
        confirmButton = {
            Button(
                onClick = onConfirm,
                modifier = Modifier.testTag(OnboardingTestTags.RECOVERY_CONFIRM),
            ) {
                Text("Reset setup progress")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}

@Composable
private fun OnboardingFooter(
    state: OnboardingUiState,
    callbacks: OnboardingCallbacks,
) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        BoxWithConstraints(modifier = Modifier.fillMaxWidth()) {
            val compact = maxWidth < 420.dp
            Column(
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(9.dp),
            ) {
                Text(
                    "Set up later keeps setup incomplete and returns you to this step next time.",
                    modifier = Modifier.fillMaxWidth(),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = if (compact) TextAlign.Center else TextAlign.Start,
                )
                if (compact) {
                    TextButton(
                        onClick = callbacks.onSetUpLater,
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(OnboardingTestTags.SET_UP_LATER),
                    ) {
                        Text("Set up later")
                    }
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        BackButton(
                            state = state,
                            onClick = callbacks.onBack,
                            modifier = Modifier.weight(1f),
                        )
                        ContinueButton(
                            state = state,
                            callbacks = callbacks,
                            modifier = Modifier.weight(1f),
                        )
                    }
                } else {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        BackButton(state = state, onClick = callbacks.onBack)
                        TextButton(
                            onClick = callbacks.onSetUpLater,
                            modifier = Modifier.testTag(OnboardingTestTags.SET_UP_LATER),
                        ) {
                            Text("Set up later")
                        }
                        Spacer(Modifier.weight(1f))
                        ContinueButton(state = state, callbacks = callbacks)
                    }
                }
            }
        }
    }
}

@Composable
private fun BackButton(
    state: OnboardingUiState,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    OutlinedButton(
        onClick = onClick,
        enabled = state.canGoBack,
        modifier = modifier.testTag(OnboardingTestTags.BACK),
    ) {
        Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = null)
        Spacer(Modifier.size(6.dp))
        Text("Back")
    }
}

@Composable
private fun ContinueButton(
    state: OnboardingUiState,
    callbacks: OnboardingCallbacks,
    modifier: Modifier = Modifier,
) {
    val isFinish = state.presentedStep == OnboardingStep.READY
    Button(
        onClick = if (isFinish) callbacks.onFinish else callbacks.onContinue,
        enabled = state.canContinue,
        modifier = modifier.testTag(
            if (isFinish) OnboardingTestTags.FINISH else OnboardingTestTags.CONTINUE,
        ),
    ) {
        Text(if (isFinish) "Finish setup" else "Continue")
    }
}

private data class ReadinessPresentation(
    val stateLabel: String,
    val message: String,
    val icon: ImageVector,
)

private fun readinessPresentation(
    step: OnboardingStep,
    check: OnboardingCheckState,
): ReadinessPresentation {
    val message = when (check) {
        OnboardingCheckState.PENDING -> when (step) {
            OnboardingStep.API -> "This phone still needs an authenticated API check."
            OnboardingStep.GOOGLE -> "Connect and refresh the selected Calendar and Tasks sources."
            OnboardingStep.PROFILE -> "Review and save the current week profile."
            OnboardingStep.NOTIFICATIONS -> "Keep the contextual default or review Android settings."
            OnboardingStep.FIRST_ITEM -> "Create or review one eligible real item."
            OnboardingStep.FIRST_PLAN -> "Compose and publish the exact reviewed revision."
            OnboardingStep.WELCOME,
            OnboardingStep.READY,
            -> "This setup check is not ready."
        }
        OnboardingCheckState.WORKING -> when (step) {
            OnboardingStep.API -> "Finishing the exact phone-enrollment request."
            OnboardingStep.GOOGLE -> "Refreshing the selected Google resources."
            OnboardingStep.PROFILE -> "Saving the encrypted week profile."
            OnboardingStep.NOTIFICATIONS -> "Checking the contextual notification choice."
            OnboardingStep.FIRST_ITEM -> "Saving and synchronizing the reviewed item."
            OnboardingStep.FIRST_PLAN -> "Composing and publishing the exact plan."
            OnboardingStep.WELCOME,
            OnboardingStep.READY,
            -> "Finishing this setup check."
        }
        OnboardingCheckState.READY -> when (step) {
            OnboardingStep.API -> "This phone completed an authenticated API request."
            OnboardingStep.GOOGLE -> "Selected read-only resources are current and saved."
            OnboardingStep.PROFILE -> "The current week profile is encrypted and ready."
            OnboardingStep.NOTIFICATIONS -> "Ask when first needed is selected."
            OnboardingStep.FIRST_ITEM -> "The reviewed item and exact anchor are ready."
            OnboardingStep.FIRST_PLAN -> "The exact current schedule revision is published."
            OnboardingStep.WELCOME,
            OnboardingStep.READY,
            -> "This setup check is ready."
        }
        OnboardingCheckState.NEEDS_ATTENTION -> when (step) {
            OnboardingStep.API -> "Phone enrollment needs attention before continuing."
            OnboardingStep.GOOGLE -> "Google authorization or import needs attention."
            OnboardingStep.PROFILE -> "The week profile must be repaired or saved again."
            OnboardingStep.NOTIFICATIONS -> "Review Android notification settings to continue."
            OnboardingStep.FIRST_ITEM -> "The reviewed item is no longer exact or available."
            OnboardingStep.FIRST_PLAN -> "The schedule proof is stale or incomplete."
            OnboardingStep.WELCOME,
            OnboardingStep.READY,
            -> "This setup check needs attention."
        }
    }
    return when (check) {
        OnboardingCheckState.PENDING -> ReadinessPresentation(
            stateLabel = "Not ready yet",
            message = message,
            icon = Icons.Outlined.RadioButtonUnchecked,
        )
        OnboardingCheckState.WORKING -> ReadinessPresentation(
            stateLabel = "Working",
            message = message,
            icon = Icons.Outlined.Sync,
        )
        OnboardingCheckState.READY -> ReadinessPresentation(
            stateLabel = "Ready",
            message = message,
            icon = Icons.Outlined.CheckCircle,
        )
        OnboardingCheckState.NEEDS_ATTENTION -> ReadinessPresentation(
            stateLabel = "Needs attention",
            message = message,
            icon = Icons.Outlined.WarningAmber,
        )
    }
}

private val OnboardingStep.icon: ImageVector
    get() = when (this) {
        OnboardingStep.WELCOME -> Icons.Outlined.Security
        OnboardingStep.API -> Icons.Outlined.Devices
        OnboardingStep.GOOGLE -> Icons.Outlined.CalendarMonth
        OnboardingStep.PROFILE -> Icons.Outlined.DateRange
        OnboardingStep.NOTIFICATIONS -> Icons.Outlined.Notifications
        OnboardingStep.FIRST_ITEM -> Icons.Outlined.AddTask
        OnboardingStep.FIRST_PLAN -> Icons.Outlined.AutoAwesome
        OnboardingStep.READY -> Icons.Outlined.CheckCircle
    }
