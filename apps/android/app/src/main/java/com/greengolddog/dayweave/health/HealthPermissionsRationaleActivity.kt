package com.greengolddog.dayweave.health

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.ui.theme.DayWeaveTheme

/** Privacy explanation opened by the Health Connect permission surface. */
class HealthPermissionsRationaleActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            DayWeaveTheme(useDynamicColor = false) {
                HealthPermissionsRationale(onClose = ::finish)
            }
        }
    }
}

@Composable
internal fun HealthPermissionsRationale(onClose: () -> Unit) {
    Scaffold(modifier = Modifier.fillMaxSize()) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(24.dp)
                .testTag("health_permissions_rationale"),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text(
                text = "Health Connect privacy",
                style = MaterialTheme.typography.headlineMedium,
                modifier = Modifier.semantics { heading() },
            )
            Text(
                "If you opt in, DayWeave reads only the aggregate duration of recent sleep " +
                    "sessions while the app is in use. It does not request write or background " +
                    "health access.",
            )
            Text(
                "DayWeave converts that aggregate into Low, Medium, or Deep energy and a broad " +
                    "recovery band. Raw Health Connect records, session times, stages, titles, " +
                    "notes, and record identifiers are never stored, sent to the DayWeave " +
                    "server, or written to logs.",
            )
            Text(
                "Only the derived bands and calculation time are retained in the encrypted " +
                    "on-device planner snapshot. You can correct the estimate with a manual " +
                    "check-in, turn sync off, or manage access in Health Connect at any time.",
            )
            Text(
                "This signal is a planning aid, not medical guidance, diagnosis, or a safety " +
                    "recommendation.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Button(
                onClick = onClose,
                modifier = Modifier.testTag("close_health_permissions_rationale"),
            ) {
                Text("Done")
            }
        }
    }
}
