/*
 * Consensus calibration: pick the devices and a time window with a range slider over
 * the chart, check the proposed corrections, and write them.
 */
package dev.artur.thermo.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RangeSlider
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.model.CalibrationRun
import dev.artur.thermo.model.CalibrationSession
import dev.artur.thermo.model.HISTORY_SPAN_MS
import dev.artur.thermo.model.Metric
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.WINDOW_STEP_MS
import dev.artur.thermo.model.corrections
import dev.artur.thermo.model.formatLag
import dev.artur.thermo.model.netCorrection
import dev.artur.thermo.model.notes
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.roundToLong

/** How a correction bar is scaled and colored, in the quantity's unit. */
private class BarScale(
    /** Correction that fills one side. */
    val full: Double,
    /** Green up to this, amber up to [large], red beyond: about half the SHT4x's rated accuracy and the accuracy itself. */
    val small: Double,
    val large: Double,
)

private val TEMPERATURE_BAR = BarScale(0.3, 0.1, 0.2)
private val HUMIDITY_BAR = BarScale(3.0, 1.0, 2.0)

/** The window offered never needs to be shorter than this. */
private const val MIN_WINDOW_MS = 2 * 3600_000L

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CalibrationScreen(
    model: ThermoModel,
    session: CalibrationSession,
    revision: Long,
    now: Long,
    snackbar: SnackbarHostState,
    onWindowChanged: () -> Unit,
    onBack: () -> Unit,
) {
    var confirm by remember { mutableStateOf<String?>(null) }
    var chartMetric by remember { mutableStateOf(Metric.Temperature) }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = { Text("Calibration") },
                actions = {
                    var menu by remember { mutableStateOf(false) }
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, "More") }
                    DropdownMenu(menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Sync clocks of these devices") }, onClick = { menu = false; model.syncClocks(session.devices) })
                        DropdownMenuItem(text = { Text("Restore factory calibration…") }, onClick = { menu = false; confirm = "reset" })
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            DeviceChips(model, session, onWindowChanged)
            WindowCard(model, session, revision, now, chartMetric, { chartMetric = it }, onWindowChanged)
            when (val result = session.result) {
                null -> Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                    Text("  Fitting…")
                }
                else -> result.fold(
                    onSuccess = { run -> ResultCards(model, run, session.computing) { confirm = "apply" } },
                    onFailure = { NoResult(it.message ?: it.toString()) },
                )
            }
            Spacer(Modifier.height(24.dp))
        }
    }
    when (confirm) {
        "apply" -> {
            val writes = model.calibrationWrites()
            ConfirmDialog(
                "Write calibration?",
                "Corrected coefficients are written to the flash of ${writes.size} device(s). " +
                    "They add to what each device reports now; the factory values can be restored from the menu.",
                "Write",
                onConfirm = { model.applyCalibration() },
                onDismiss = { confirm = null },
            )
        }
        "reset" -> ConfirmDialog(
            "Restore factory calibration?",
            "The factory coefficients are written back to ${session.devices.size} device(s).",
            "Restore",
            onConfirm = { model.resetCalibration(session.devices) },
            onDismiss = { confirm = null },
        )
    }
}

@Composable
private fun DeviceChips(model: ThermoModel, session: CalibrationSession, onChanged: () -> Unit) {
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        for (device in model.devices.all()) {
            val included = device.address in session.devices
            FilterChip(
                selected = included,
                onClick = {
                    val next = if (included) session.devices - device.address else session.devices + device.address
                    if (next.size >= 2) {
                        model.setCalibrationDevices(model.devices.all().map { it.address }.filter { it in next })
                        onChanged()
                    }
                },
                leadingIcon = { ColorDot(model.colorOf(device.address), 10.dp) },
                label = { Text(device.name) },
            )
        }
    }
}

@Composable
private fun WindowCard(
    model: ThermoModel,
    session: CalibrationSession,
    revision: Long,
    now: Long,
    metric: Metric,
    onMetric: (Metric) -> Unit,
    onChanged: () -> Unit,
) {
    val earliest = model.earliestCalibrationStart(session.devices)
    val historyStart = model.historyStart(session.devices) ?: (now - MIN_WINDOW_MS)
    val from = minOf(maxOf(earliest, historyStart, now - HISTORY_SPAN_MS).coerceAtMost(now - MIN_WINDOW_MS), session.start)
    val span = (now - from).toFloat()
    // Slider positions while dragging, before the model snaps and refits.
    var dragging by remember { mutableStateOf<ClosedFloatingPointRange<Float>?>(null) }
    val start = dragging?.let { from + (it.start * span).toLong() } ?: session.start
    val end = dragging?.let { e -> (from + (e.endInclusive * span).toLong()).takeIf { it < now - WINDOW_STEP_MS / 2 } } ?: session.end
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            val hours = ((end ?: now) - start) / 3_600_000.0
            Text(
                "${formatMoment(start, now)} → ${end?.let { formatMoment(it, now) } ?: "now"}  ·  %.1f h".format(hours),
                style = MaterialTheme.typography.titleMedium,
            )
            if (earliest > 0 && start <= earliest + WINDOW_STEP_MS / 2) {
                Text(
                    "Starts at the last calibration of these devices, the earliest usable data.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            val series = model.devices.all().filter { it.address in session.devices }
                .map { ChartSeries(it.address, it.name, model.colorOf(it.address), it.history) }
            HistoryChart(
                series, metric, from, now, revision,
                Modifier.fillMaxWidth().height(200.dp).padding(top = 8.dp),
                window = start to (end ?: now),
            )
            RangeSlider(
                value = ((start - from) / span).coerceIn(0f, 1f)..(((end ?: now) - from) / span).coerceIn(0f, 1f),
                onValueChange = { dragging = it },
                onValueChangeFinished = {
                    dragging?.let { range ->
                        val newEnd = from + (range.endInclusive * span).roundToLong()
                        model.setCalibrationWindow(from + (range.start * span).roundToLong(), newEnd.takeIf { it < now - WINDOW_STEP_MS / 2 })
                        onChanged()
                    }
                    dragging = null
                },
                modifier = Modifier.padding(start = 40.dp),
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Cover the steady hours in both places, but not the moves.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
                )
                FilterChip(selected = metric == Metric.Temperature, onClick = { onMetric(Metric.Temperature) }, label = { Text("°C") })
                Spacer(Modifier.width(4.dp))
                FilterChip(selected = metric == Metric.Humidity, onClick = { onMetric(Metric.Humidity) }, label = { Text("%RH") })
            }
        }
    }
}

@Composable
private fun NoResult(error: String) {
    Section("No result yet") {
        Text(error, color = MaterialTheme.colorScheme.error)
        Spacer(Modifier.height(8.dp))
        for (hint in listOf(
            "Put the thermometers side by side, a few cm apart, away from heaters, sun and people.",
            "Offsets need a few undisturbed hours; gains also need a second, different condition " +
                "(e.g. a few hours outside) to cover at least 5 °C and 15 %RH.",
            "Only live readings count, and only after the last calibration of these devices; keep this phone in range.",
        )) {
            Text("• $hint", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(vertical = 2.dp))
        }
    }
}

@Composable
private fun ResultCards(model: ThermoModel, run: CalibrationRun, computing: Boolean, onApply: () -> Unit) {
    val r = run.report
    Section("Summary" + if (computing) " · updating…" else "") {
        Fact("Steady data", "${(r.temperature.steady / 60).toInt()} min °C · ${(r.humidity.steady / 60).toInt()} min %RH")
        Fact("Phases", "${r.phases.size}" + if (r.phases.size < 2) " (gains need two)" else "")
        Fact("Range", "%.1f–%.1f °C · %.0f–%.0f %%RH".format(r.temperature.range.first, r.temperature.range.second, r.humidity.range.first, r.humidity.range.second))
        Fact("Disagreement", "%.3f → %.3f °C · %.2f → %.2f %%RH".format(r.temperature.spread.before, r.temperature.spread.after, r.humidity.spread.before, r.humidity.spread.after))
    }
    val midT = (r.temperature.range.first + r.temperature.range.second) / 2
    val midH = (r.humidity.range.first + r.humidity.range.second) / 2
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(vertical = 8.dp)) {
            Text(
                "Corrections", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
            run.devices.zip(r.sensors).forEachIndexed { i, (device, sensor) ->
                if (i > 0) HorizontalDivider(Modifier.padding(horizontal = 16.dp))
                val (tWritten, hWritten) = corrections(sensor)
                Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ColorDot(model.colorOf(device.first))
                        Text("  ${device.second}", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                        val note = notes(sensor.temperature, sensor.humidity)
                        when {
                            tWritten != null && hWritten != null -> Icon(Icons.Default.CheckCircle, "Applies", tint = Fine, modifier = Modifier.size(18.dp))
                            else -> {
                                Icon(Icons.Default.Warning, null, tint = Marginal, modifier = Modifier.size(16.dp))
                                Text(
                                    (if (tWritten != null || hWritten != null) " partly: " else " ") + note,
                                    style = MaterialTheme.typography.labelMedium, color = Marginal,
                                )
                            }
                        }
                    }
                    FitRow("°C", sensor.temperature, netCorrection(sensor.temperature, midT), TEMPERATURE_BAR, tWritten != null, 3, sensor.temperature.lag?.let { "lag ${formatLag(it)}" })
                    FitRow("%RH", sensor.humidity, netCorrection(sensor.humidity, midH), HUMIDITY_BAR, hWritten != null, 2, null)
                }
            }
        }
    }
    val writes = model.calibrationWrites().size
    Button(onClick = onApply, enabled = writes > 0, modifier = Modifier.fillMaxWidth()) {
        Text(if (writes > 0) "Write corrections to $writes device(s)" else "Nothing to write")
    }
    Text(
        "Bars show the net correction mid-range. Agreement isn't absolute accuracy; a salt test (75.3 %RH over NaCl) pins humidity.",
        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun FitRow(unit: String, fit: Fit, net: Double, scale: BarScale, written: Boolean, decimals: Int, extra: String?) {
    Row(Modifier.fillMaxWidth().padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(
            "${signed(fit.offset, decimals)} $unit", style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium,
            modifier = Modifier.width(100.dp),
        )
        CorrectionBar(net, scale, written, Modifier.width(72.dp).height(12.dp))
        Column(Modifier.padding(start = 12.dp)) {
            Text(
                "gain " + (if (fit.gainFitted) "%.4f".format(fit.gain) else "–") + " · rms %.${decimals}f".format(fit.residualRms),
                style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1,
            )
            extra?.let {
                Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
            }
        }
    }
}

/** [value] as a bar left (negative) or right of a zero line; grey if it won't be written. */
@Composable
private fun CorrectionBar(value: Double, scale: BarScale, written: Boolean, modifier: Modifier) {
    val color = when {
        !written -> MaterialTheme.colorScheme.outline
        abs(value) <= scale.small -> Fine
        abs(value) <= scale.large -> Marginal
        else -> Bad
    }
    val axis = MaterialTheme.colorScheme.outlineVariant
    Canvas(modifier) {
        val mid = size.width / 2
        val length = (abs(value) / scale.full).coerceAtMost(1.0).toFloat() * mid
        val left = if (value < 0) mid - length else mid
        drawRect(color, Offset(left, size.height * 0.2f), Size(max(length, 1f), size.height * 0.6f))
        if (abs(value) > scale.full) {
            // Past full scale: a notch at the end shows it doesn't fit.
            val x = if (value < 0) 0f else size.width - 3f
            drawRect(Color.Black.copy(alpha = 0.35f), Offset(x, size.height * 0.2f), Size(3f, size.height * 0.6f))
        }
        drawLine(axis, Offset(mid, 0f), Offset(mid, size.height), 2f)
    }
}
