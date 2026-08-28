package com.greengolddog.dayweave.ui.navigation

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.MoreHoriz
import androidx.compose.material.icons.outlined.Today
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Badge
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import com.greengolddog.dayweave.model.AppDestination

@Composable
fun DayWeaveNavigationBar(
    selected: AppDestination,
    pendingSuggestionCount: Int,
    onSelect: (AppDestination) -> Unit,
) {
    NavigationBar {
        AppDestination.entries.forEach { destination ->
            NavigationBarItem(
                selected = selected == destination,
                onClick = { onSelect(destination) },
                icon = {
                    BadgedBox(
                        badge = {
                            if (destination == AppDestination.INBOX && pendingSuggestionCount > 0) {
                                Badge { Text(pendingSuggestionCount.toString()) }
                            }
                        },
                    ) {
                        Icon(
                            imageVector = when (destination) {
                                AppDestination.TODAY -> Icons.Outlined.Today
                                AppDestination.CALENDAR -> Icons.Outlined.CalendarMonth
                                AppDestination.INBOX -> Icons.Outlined.Inbox
                                AppDestination.ASSISTANT -> Icons.Outlined.AutoAwesome
                                AppDestination.MORE -> Icons.Outlined.MoreHoriz
                            },
                            contentDescription = destination.label,
                        )
                    }
                },
                label = { Text(destination.label) },
                alwaysShowLabel = true,
            )
        }
    }
}
