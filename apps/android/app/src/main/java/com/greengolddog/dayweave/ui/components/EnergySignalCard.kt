package com.greengolddog.dayweave.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.EnergySignalSource

@Composable
fun EnergySignalCard(
    state: DayWeaveUiState,
    onCheckIn: (EnergyLevel) -> Unit,
    onClearManualCheckIn: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val signal = state.effectiveEnergySignal()
    val fit = state.energyFitCandidate()
    val signalDescription = signal?.let {
        "${it.energy.label} energy from ${it.source.label}"
    } ?: "No current energy signal"

    Card(
        modifier = modifier
            .fillMaxWidth()
            .testTag("energy_signal_card")
            .semantics { stateDescription = signalDescription },
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Outlined.Bolt, contentDescription = null)
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        "Energy & recovery",
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.semantics { heading() },
                    )
                    Text(
                        signal?.let {
                            buildString {
                                append("${it.energy.label} · ${it.source.label}")
                                it.recovery?.let { recovery ->
                                    append(" · ${recovery.label} recovery")
                                }
                            }
                        } ?: "No current estimate · manual check-in is always available",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            Text(
                "How is your usable energy right now? Tap again anytime to correct it.",
                style = MaterialTheme.typography.bodySmall,
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                EnergyLevel.entries.forEach { level ->
                    FilterChip(
                        selected = signal?.energy == level,
                        onClick = { onCheckIn(level) },
                        label = { Text(level.label) },
                        modifier = Modifier
                            .weight(1f)
                            .testTag("energy_check_in_${level.name.lowercase()}")
                            .semantics {
                                contentDescription = "Set current energy to ${level.label}"
                            },
                    )
                }
            }

            if (state.manualEnergyCheckIn != null) {
                TextButton(
                    onClick = onClearManualCheckIn,
                    modifier = Modifier.testTag("clear_manual_energy_check_in"),
                ) {
                    Text(
                        if (
                            state.healthConnectSyncEnabled &&
                            state.derivedEnergySnapshot != null
                        ) {
                            "Use Health Connect estimate"
                        } else {
                            "Clear manual check-in"
                        },
                    )
                }
            }

            Text(
                fit?.let { "Best current fit in the existing plan: ${it.title}." }
                    ?: if (signal == null) {
                        "Add a check-in to compare your energy with scheduled work."
                    } else {
                        "No remaining flexible block currently fits this energy band."
                    },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.testTag("energy_fit_hint"),
            )
            Text(
                if (signal?.source == EnergySignalSource.HEALTH_CONNECT_SLEEP) {
                    "Sleep duration is only a coarse planning input. This is not medical guidance."
                } else {
                    "Manual input is used only as planning context. It never changes the schedule by itself."
                },
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
