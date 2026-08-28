package com.greengolddog.dayweave.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val LightColors = lightColorScheme(
    primary = WeaveBlue,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFD9E2FF),
    onPrimaryContainer = Color(0xFF001A42),
    secondary = WeaveMint,
    tertiary = WeaveLilac,
    error = WeaveRed,
    background = WeaveCanvas,
    onBackground = WeaveInk,
    surface = Color.White,
    onSurface = WeaveInk,
    surfaceVariant = Color(0xFFE2E6F0),
    outlineVariant = Color(0xFFC5C9D4),
)

private val DarkColors = darkColorScheme(
    primary = WeaveBlueDark,
    onPrimary = Color(0xFF002E6B),
    primaryContainer = Color(0xFF154690),
    onPrimaryContainer = Color(0xFFD9E2FF),
    secondary = WeaveMintDark,
    tertiary = Color(0xFFE0B9FF),
    error = Color(0xFFFFB4AB),
    background = WeaveCanvasDark,
    onBackground = Color(0xFFE3E2E8),
    surface = Color(0xFF191C22),
    onSurface = Color(0xFFE3E2E8),
    surfaceVariant = Color(0xFF43474F),
    outlineVariant = Color(0xFF43474F),
)

@Composable
fun DayWeaveTheme(
    useDynamicColor: Boolean,
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val colors = when {
        useDynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && darkTheme ->
            dynamicDarkColorScheme(context)
        useDynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
            dynamicLightColorScheme(context)
        darkTheme -> DarkColors
        else -> LightColors
    }

    MaterialTheme(
        colorScheme = colors,
        typography = DayWeaveTypography,
        content = content,
    )
}
