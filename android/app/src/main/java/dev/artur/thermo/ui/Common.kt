/*
 * Theme, device colors and the small formatting helpers the screens share.
 */
package dev.artur.thermo.ui

import android.os.Build
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.artur.thermo.model.RSSI_FINE
import dev.artur.thermo.model.RSSI_MARGINAL
import dev.artur.thermo.model.ThermoModel
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.math.abs

/** Line colors, assigned to devices in list order; the same as the desktop chart's. */
private val PALETTE = listOf(
    Color(0xff4e9ae0), Color(0xfff28e2b), Color(0xff59c14f), Color(0xffe15759),
    Color(0xffb07ad1), Color(0xffedc948), Color(0xff76d7d2), Color(0xffff9da7),
)

fun deviceColor(index: Int): Color = PALETTE[index.mod(PALETTE.size)]

/** The color of [address] in the model's device order. */
fun ThermoModel.colorOf(address: String): Color = deviceColor((deviceNumber(address) ?: 1) - 1)

/** Battery percentages from which a level counts as fine, then as getting low. */
const val BATTERY_FINE = 50
const val BATTERY_LOW = 20

/** Green, amber and red for good, marginal and bad values, readable on both themes. */
val Fine = Color(0xff3fa34d)
val Marginal = Color(0xffd69e00)
val Bad = Color(0xffd84a3c)

fun rssiColor(rssi: Int?): Color? = when {
    rssi == null -> null
    rssi >= RSSI_FINE -> Fine
    rssi >= RSSI_MARGINAL -> Marginal
    else -> Bad
}

fun batteryColor(percent: Int?): Color? = when {
    percent == null -> null
    percent >= BATTERY_FINE -> Fine
    percent >= BATTERY_LOW -> Marginal
    else -> Bad
}

@Composable
fun ThermoTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val scheme = when {
        Build.VERSION.SDK_INT >= 31 -> if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> darkColorScheme(primary = Color(0xff9ecaff), secondary = Color(0xffffb77c))
        else -> lightColorScheme(primary = Color(0xff1d6aa5), secondary = Color(0xffa45a12))
    }
    MaterialTheme(colorScheme = scheme, content = content)
}

@Composable
fun ColorDot(color: Color, size: Dp = 12.dp, modifier: Modifier = Modifier) {
    Box(modifier.size(size).background(color, CircleShape))
}

/** `"12 s ago"`, `"3 min ago"`, `"2 h ago"`, `"3 d ago"`. */
fun formatAgo(ms: Long): String {
    val s = (ms / 1000).coerceAtLeast(0)
    return when {
        s < 5 -> "just now"
        s < 90 -> "$s s ago"
        s < 90 * 60 -> "${s / 60} min ago"
        s < 48 * 3600 -> "${s / 3600} h ago"
        else -> "${s / 86_400} d ago"
    }
}

/** `"40 s"`, `"12 min"`, `"2 h 5 min"`. */
fun formatDuration(ms: Long): String {
    val s = (ms / 1000).coerceAtLeast(0)
    return when {
        s < 60 -> "$s s"
        s < 3600 -> "${s / 60} min"
        else -> "${s / 3600} h ${s / 60 % 60} min"
    }
}

/** `"2.5 s"`, `"10 s"`, `"5 min"`, `"1 h"`. */
fun formatInterval(seconds: Double): String = when {
    seconds < 60 -> if (seconds == Math.floor(seconds)) "${seconds.toInt()} s" else "%.1f s".format(seconds)
    seconds < 3600 -> if (seconds % 60 == 0.0) "${(seconds / 60).toInt()} min" else "%.1f min".format(seconds / 60)
    else -> if (seconds % 3600 == 0.0) "${(seconds / 3600).toInt()} h" else "%.1f h".format(seconds / 3600)
}

/** Chart span label: `"10 min"`, `"1 h"`, `"48 h"`. */
fun formatSpan(ms: Long): String = if (ms < 3_600_000) "${ms / 60_000} min" else "${ms / 3_600_000} h"

private fun pattern(p: String) = SimpleDateFormat(p, Locale.getDefault())

/** `"14:05"` today, otherwise `"Tue 14:05"`. */
fun formatMoment(ms: Long, now: Long = System.currentTimeMillis()): String {
    val sameDay = pattern("yyyyMMdd").format(Date(ms)) == pattern("yyyyMMdd").format(Date(now))
    return pattern(if (sameDay) "HH:mm" else "EEE HH:mm").format(Date(ms))
}

fun formatClockTime(ms: Long): String = pattern("HH:mm:ss").format(Date(ms))

fun signed(value: Double, decimals: Int): String =
    (if (value >= 0) "+" else "−") + "%.${decimals}f".format(abs(value))
