/*
 * A calibration in progress: the chosen thermometers' chart, how precise corrections
 * written now would be and how long each better precision takes, and the next step:
 * write, or move them to a second place for the gains, then check what was written.
 */
package dev.artur.thermo.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.HourglassTop
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.artur.thermo.calibration.Fit
import dev.artur.thermo.model.CalibrationRun
import dev.artur.thermo.model.CalibrationSession
import dev.artur.thermo.model.ETA_SURE_MS
import dev.artur.thermo.model.Eta
import dev.artur.thermo.model.GAIN_SPAN_H
import dev.artur.thermo.model.GAIN_SPAN_T
import dev.artur.thermo.model.Level
import dev.artur.thermo.model.Metric
import dev.artur.thermo.model.SECOND_PLACE_STEADY_MS
import dev.artur.thermo.model.SESSION_OPTIONS
import dev.artur.thermo.model.Stage
import dev.artur.thermo.model.TYPICAL_GAIN_ERROR
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.WINDOW_STEP_MS
import dev.artur.thermo.model.deviceUncertainty
import dev.artur.thermo.model.eta
import dev.artur.thermo.model.firstEstimateIn
import dev.artur.thermo.model.formatEta
import dev.artur.thermo.model.formatLag
import dev.artur.thermo.model.gainStatus
import dev.artur.thermo.model.heldBack
import dev.artur.thermo.model.netCorrection
import dev.artur.thermo.model.notes
import dev.artur.thermo.model.precision
import dev.artur.thermo.model.secondPlaceAdvice
import kotlin.math.abs
import kotlin.math.max

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

/** The chart shows at least this much, and this much before the readings used. */
private const val MIN_CHART_MS = 3600_000L
private const val CHART_CONTEXT_MS = 30 * 60_000L

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CalibrationScreen(
    model: ThermoModel,
    session: CalibrationSession,
    revision: Long,
    now: Long,
    snackbar: SnackbarHostState,
    onChanged: () -> Unit,
    onBack: () -> Unit,
) {
    var confirm by remember { mutableStateOf<String?>(null) }
    var dialog by remember { mutableStateOf<String?>(null) }
    var metric by remember { mutableStateOf(Metric.Temperature) }
    val run = session.result?.getOrNull()
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = {
                    Column {
                        Text("Calibration")
                        Text(
                            "${session.devices.size} thermometers · since ${formatMoment(session.start, now)}",
                            style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1, overflow = TextOverflow.Ellipsis,
                        )
                    }
                },
                actions = {
                    var menu by remember { mutableStateOf(false) }
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, "More") }
                    DropdownMenu(menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Thermometers…") }, onClick = { menu = false; dialog = "devices" })
                        DropdownMenuItem(text = { Text("Correct to…") }, onClick = { menu = false; dialog = "reference" })
                        if (session.stage != Stage.Checking) {
                            DropdownMenuItem(text = { Text("Readings since…") }, onClick = { menu = false; dialog = "start" })
                        }
                        DropdownMenuItem(text = { Text("Sync their clocks") }, onClick = { menu = false; model.syncClocks(session.devices) })
                        DropdownMenuItem(text = { Text("Restore factory calibration…") }, onClick = { menu = false; confirm = "reset" })
                        DropdownMenuItem(text = { Text("End calibration") }, onClick = { menu = false; confirm = "end" })
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            StageRow(session.stage)
            for (stray in model.calibrationStrays()) {
                val name = model.devices[stray.address]?.let { model.numberedName(it) } ?: stray.address
                Notice("$name ${stray.reason}")
            }
            ChartCard(model, session, run, revision, now, metric) { metric = it }
            when (session.stage) {
                Stage.FirstPlace -> FirstPlace(model, session, run, now, onWrite = { confirm = "apply" }, onMoved = { model.markMoved(); onChanged() }, onChanged = onChanged)
                Stage.SecondPlace -> SecondPlace(model, session, run, now, writable = model.calibrationWrites().isNotEmpty(), onWrite = { confirm = "apply" }, onBackToFirst = { model.unmarkMoved(); onChanged() })
                Stage.Checking -> Checking(model, session, run, now, onFinish = { model.endCalibration(); onBack() })
            }
            if (run != null) CorrectionsCard(model, run, checking = session.stage == Stage.Checking)
            Spacer(Modifier.height(24.dp))
        }
    }
    when (confirm) {
        "apply" -> {
            val writes = model.calibrationWrites()
            ConfirmDialog(
                "Write corrections?",
                "New coefficients are written to the flash of ${writes.size} thermometer(s), on top of what each reports now. " +
                    "Keep them together afterwards to check the result; the factory values can be restored from the menu.",
                "Write",
                onConfirm = { model.applyCalibration() },
                onDismiss = { confirm = null },
            )
        }
        "reset" -> ConfirmDialog(
            "Restore factory calibration?",
            "The factory coefficients are written back to ${session.devices.size} thermometer(s). Readings before that no longer count.",
            "Restore",
            onConfirm = { model.resetCalibration(session.devices) },
            onDismiss = { confirm = null },
        )
        "end" -> ConfirmDialog(
            "End calibration?",
            "Nothing more is written; corrections already written stay on the thermometers.",
            "End",
            onConfirm = { model.endCalibration(); onBack() },
            onDismiss = { confirm = null },
        )
    }
    when (dialog) {
        "devices" -> DevicesDialog(model, session, onDone = { dialog = null; onChanged() })
        "reference" -> ReferenceDialog(model, session, onDone = { dialog = null; onChanged() })
        "start" -> StartDialog(model, session, now, onDone = { dialog = null; onChanged() })
    }
}

@Composable
@OptIn(ExperimentalLayoutApi::class)
private fun StageRow(stage: Stage) {
    val steps = listOf("Together", "Second place", "Written")
    val current = when (stage) {
        Stage.FirstPlace -> 0
        Stage.SecondPlace -> 1
        Stage.Checking -> 2
    }
    // Wraps between steps with large text, never inside one.
    FlowRow(
        Modifier.fillMaxWidth().padding(top = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        steps.forEachIndexed { i, label ->
            val color = when {
                i == current -> MaterialTheme.colorScheme.primary
                i < current -> MaterialTheme.colorScheme.onSurfaceVariant
                else -> MaterialTheme.colorScheme.outline
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("${i + 1}", color = color, fontWeight = FontWeight.Bold, style = MaterialTheme.typography.labelLarge)
                Text(
                    label + if (i == 1) " (optional)" else "", color = color, style = MaterialTheme.typography.labelLarge,
                    fontWeight = if (i == current) FontWeight.Bold else FontWeight.Normal,
                )
                if (i < steps.lastIndex) Text("›", color = MaterialTheme.colorScheme.outline)
            }
        }
    }
}

@Composable
private fun Notice(text: String, color: Color = Marginal) {
    Card(colors = CardDefaults.cardColors(containerColor = color.copy(alpha = 0.15f)), modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.padding(12.dp)) {
            Icon(Icons.Default.Warning, null, tint = color, modifier = Modifier.padding(top = 2.dp).size(18.dp))
            Text(text, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f).padding(start = 8.dp))
        }
    }
}

@Composable
private fun ChartCard(
    model: ThermoModel,
    session: CalibrationSession,
    run: CalibrationRun?,
    revision: Long,
    now: Long,
    metric: Metric,
    onMetric: (Metric) -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            val from = minOf(session.start - CHART_CONTEXT_MS, now - MIN_CHART_MS)
            val series = model.devices.all().filter { it.address in session.devices }
                .map { ChartSeries(it.address, it.name, model.colorOf(it.address), it.history) }
            val bin = (SESSION_OPTIONS.bin * 1000).toLong()
            val steady = run?.let { if (metric == Metric.Humidity) it.report.humidity.steadyBins else it.report.temperature.steadyBins }
                .orEmpty().map { (it * 1000).toLong().let { t -> t to t + bin } }
            HistoryChart(
                series, metric, from, now, revision,
                Modifier.fillMaxWidth().height(220.dp),
                window = session.start to now,
                marks = listOfNotNull(session.moved),
                used = steady,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "The strip along the bottom marks the steady readings used." + if (session.moved != null) " Dashed: the move." else "",
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

/** Fitting, collecting, or why there is no fit; `true` if [run] can be shown. */
@Composable
private fun fittedOrProgress(session: CalibrationSession, now: Long): Boolean {
    val result = session.result
    if (result == null) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
            Text("  Fitting…")
        }
        return false
    }
    val error = result.exceptionOrNull() ?: return true
    val wait = firstEstimateIn(error)
    Section(if (wait != null) "Collecting readings" else "No fit") {
        if (wait != null) {
            Text("A first estimate in about ${formatEta(wait)}, if they stay steady.", style = MaterialTheme.typography.bodyLarge)
            Spacer(Modifier.height(6.dp))
            Text(
                "Only steady readings count: not the first ${(SESSION_OPTIONS.steadyWindow / 120).toInt()} minutes, " +
                    "nor a thermometer's readings while it catches up after being moved. This goes on with the app closed.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            Text(error.message ?: error.toString(), color = MaterialTheme.colorScheme.error)
        }
    }
    return false
}

@Composable
private fun FirstPlace(
    model: ThermoModel,
    session: CalibrationSession,
    run: CalibrationRun?,
    now: Long,
    onWrite: () -> Unit,
    onMoved: () -> Unit,
    onChanged: () -> Unit,
) {
    if (!fittedOrProgress(session, now) || run == null) return
    SettlingNote(model, run, now)
    PrecisionCard(run, session)
    LevelsCard(model, session, run, onChanged)
    val place = run.places.firstOrNull()
    val p = precision(run)
    Section("Next") {
        val chosen = p.temperature?.let { "±%.3f °C".format(it) } ?: "this precision"
        Text("If $chosen is enough for you, write the corrections now.", style = MaterialTheme.typography.bodyLarge)
        val target = session.target
        when (val wait = eta(run, target)) {
            is Eta.In -> Text(
                "${target.label} (±%.2f °C) in about ${formatEta(wait.ms)}; you'll get a notification.".format(target.temperature),
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Eta.Later -> Text(
                "${target.label} (±%.2f °C) would take over ${formatEta(ETA_SURE_MS)}, and may not come: ".format(target.temperature) +
                    "a room seldom stays as steady for that long. Pick a lower precision above, or wait and see; you'll get a notification.",
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Eta.Unlikely -> Text(
                "${target.label} is unlikely here: they don't stay steady enough. Pick a lower precision above.",
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else -> {}
        }
        Button(onClick = onWrite, enabled = model.calibrationWrites().isNotEmpty(), modifier = Modifier.padding(vertical = 6.dp)) {
            Text("Write corrections")
        }
        HorizontalDivider(Modifier.padding(vertical = 6.dp))
        if (place != null) {
            val advice = secondPlaceAdvice(place)
            Text(
                "For the gain too, move them together to a place at least ${GAIN_SPAN_T.toInt()} °C colder or warmer: " +
                    "below %.0f °C or above %.0f °C; 10 °C apart (%.0f °C or %.0f °C) halves its error. ".format(
                        advice.colderThan, advice.warmerThan, advice.betterColderThan, advice.betterWarmerThan,
                    ) +
                    "Without it, readings far from %.1f °C may be off by up to %.1f °C per 10 °C.".format(place.temperature, TYPICAL_GAIN_ERROR * 10),
                style = MaterialTheme.typography.bodyMedium,
            )
            val humid = listOfNotNull(advice.drierThan?.let { "below %.0f %%".format(it) }, advice.moreHumidThan?.let { "above %.0f %%".format(it) })
            if (humid.isNotEmpty()) {
                Spacer(Modifier.height(6.dp))
                Text(
                    "If the humidity there also differs by ${GAIN_SPAN_H.toInt()} %RH (${humid.joinToString(" or ")}), the humidity gain is calibrated too.",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Spacer(Modifier.height(6.dp))
            Text(
                "Keep the phone in range, e.g. a balcony or a cold room. Readings there count once the thermometers have caught up, " +
                    "about an hour for temperature: plan on 1½–2 hours, and some hours more for the humidity of slow sensors.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        OutlinedButton(onClick = onMoved, modifier = Modifier.padding(top = 6.dp)) { Text("I've moved them") }
    }
}

/** How precise corrections written now would be. */
@Composable
private fun PrecisionCard(run: CalibrationRun, session: CalibrationSession) {
    val p = precision(run)
    val gains = gainStatus(run)
    Section("If you write now") {
        Row(horizontalArrangement = Arrangement.spacedBy(24.dp)) {
            Figure(p.temperature?.let { "±%.3f".format(it) } ?: "–", "°C")
            Figure(p.humidity?.let { "±%.2f".format(it) } ?: "–", "%RH")
            Figure(gains.uncertainty?.let { "±%.1f".format(it * 100) } ?: "–", "% gain")
        }
        val steadyMin = (run.report.temperature.steady / 60).toInt()
        val relative = run.reference?.let { "the reference" } ?: "their average"
        Text(
            "Each correction within this of $relative, 19 times in 20, at about %.1f °C and %.0f %%RH; ".format(
                (run.report.temperature.range.first + run.report.temperature.range.second) / 2,
                (run.report.humidity.range.first + run.report.humidity.range.second) / 2,
            ) + "from $steadyMin min of steady readings." +
                if (gains.uncertainty == null && session.stage == Stage.FirstPlace) " Gains need a second place." else "",
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 6.dp),
        )
    }
}

@Composable
private fun Figure(value: String, unit: String) {
    Column {
        Text(value, fontSize = 26.sp, fontWeight = FontWeight.Medium)
        Text(unit, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** The precisions to choose from and when each is reached; the chosen one is announced. */
@Composable
private fun LevelsCard(model: ThermoModel, session: CalibrationSession, run: CalibrationRun, onChanged: () -> Unit) {
    Section("Precision or time") {
        for (level in Level.entries) {
            val eta = eta(run, level)
            Row(
                Modifier.fillMaxWidth().clickable { model.setCalibrationTarget(level); onChanged() },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = session.target == level, onClick = { model.setCalibrationTarget(level); onChanged() })
                Column(Modifier.weight(1f)) {
                    Text(level.label, style = MaterialTheme.typography.bodyLarge)
                    Text(
                        "±%.2f °C · ±%.1f %%RH".format(level.temperature, level.humidity),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                when (eta) {
                    Eta.Reached -> Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Default.CheckCircle, null, tint = Fine, modifier = Modifier.size(16.dp))
                        Text(" now", color = Fine, style = MaterialTheme.typography.bodyMedium)
                    }
                    is Eta.In -> Text("in ~${formatEta(eta.ms)}", style = MaterialTheme.typography.bodyMedium)
                    Eta.Later -> Text("over ${formatEta(ETA_SURE_MS)}, if at all", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.outline)
                    Eta.Unlikely -> Text("unlikely here", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.outline)
                    null -> Text("–")
                }
            }
        }
        Text(
            "Times assume they stay as steady as so far. You get a notification when your choice is reached, also with the app closed. " +
                "The screens show %.1f °C: at ±%.2f °C two thermometers rarely differ by more than a digit.".format(0.1, 0.05),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 4.dp),
        )
    }
}

@Composable
private fun SecondPlace(
    model: ThermoModel,
    session: CalibrationSession,
    run: CalibrationRun?,
    now: Long,
    writable: Boolean,
    onWrite: () -> Unit,
    onBackToFirst: () -> Unit,
) {
    val moved = session.moved ?: return
    val first = run?.places?.getOrNull(0)
    val second = run?.places?.getOrNull(1)
    Section("Second place") {
        Text("Moved at ${formatMoment(moved, now)}, ${formatDuration(now - moved)} ago.", style = MaterialTheme.typography.bodyLarge)
        if (first != null && second != null) {
            val dt = second.latestTemperature - first.temperature
            val dh = second.latestHumidity - first.humidity
            Check(
                abs(dt) >= GAIN_SPAN_T,
                "Now %.1f °C: %.1f °C %s than the first place".format(second.latestTemperature, abs(dt), if (dt < 0) "colder" else "warmer") +
                    if (abs(dt) >= GAIN_SPAN_T) "" else "; the gain needs ${GAIN_SPAN_T.toInt()} °C",
            )
            Check(
                abs(dh) >= GAIN_SPAN_H,
                "Now %.0f %%RH: %+.0f %%RH".format(second.latestHumidity, dh) +
                    if (abs(dh) >= GAIN_SPAN_H) ", so the humidity gain too" else "; the humidity gain needs ${GAIN_SPAN_H.toInt()}",
            )
            if (run != null) SettlingNote(model, run, now)
            Spacer(Modifier.height(8.dp))
            Text(
                (if (second.steady > 0) "${formatDuration(second.steady)} of steady readings here" else "No steady readings here yet") +
                    "; at least ${formatDuration(SECOND_PLACE_STEADY_MS)} gives a usable gain, longer a better one.",
                style = MaterialTheme.typography.bodyMedium,
            )
            LinearProgressIndicator(
                progress = { (second.steady.toFloat() / SECOND_PLACE_STEADY_MS).coerceIn(0f, 1f) },
                modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
            )
        } else {
            Text("Waiting for readings from the new place…", style = MaterialTheme.typography.bodyMedium)
        }
        if (run != null) {
            val gains = gainStatus(run)
            Text(
                when {
                    gains.uncertainty != null -> "Temperature gain fitted for ${gains.fitted} of ${gains.of}: ±%.2f %%, so ±%.2f °C 10 °C from the middle.".format(gains.uncertainty * 100, gains.uncertainty * 10)
                    gains.span >= GAIN_SPAN_T -> "Their gains don't differ measurably from 1 yet; offsets alone would be written."
                    else -> "No gain yet: the steady readings span %.1f °C of the ${GAIN_SPAN_T.toInt()} needed.".format(gains.span)
                } + if (gains.humidity) " Humidity gain fitted too." else "",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
    if (!fittedOrProgress(session, now) || run == null) {
        OutlinedButton(onClick = onBackToFirst) { Text("Not moved after all") }
        return
    }
    PrecisionCard(run, session)
    Column {
        Button(onClick = onWrite, enabled = writable) { Text("Write corrections") }
        OutlinedButton(onClick = onBackToFirst) { Text("Not moved after all") }
    }
}

/**
 * Who is still catching up with the others after a move, per quantity, and when they are
 * expected to count; nothing if no one is.
 */
@Composable
private fun SettlingNote(model: ThermoModel, run: CalibrationRun, now: Long) {
    val held = listOf("Temperature" to heldBack(run, temperature = true), "Humidity" to heldBack(run, temperature = false))
        .mapNotNull { (label, devices) -> devices?.let { label to it } }
    if (held.isEmpty()) return
    fun short(i: Int): String {
        val address = run.devices[i].first
        return model.deviceNumber(address)?.let { "#$it" } ?: run.devices[i].second
    }
    Column(Modifier.padding(top = 6.dp)) {
        for ((label, devices) in held) {
            val untils = devices.map { it.second }.distinct()
            val who = if (devices.size == run.devices.size && untils.size == 1) {
                "all" + (untils[0]?.let { if (it > now) ", counting from ${formatMoment(it, now)}" else "" } ?: "")
            } else {
                devices.joinToString(", ") { (i, until) ->
                    short(i) + (until?.takeIf { it > now }?.let { " (about ${formatEta(it - now)} more)" } ?: "")
                }
            }
            Row(Modifier.padding(top = 2.dp)) {
                Icon(Icons.Default.HourglassTop, null, tint = Marginal, modifier = Modifier.padding(top = 2.dp).size(16.dp))
                Text("$label still settling: $who.", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f).padding(start = 8.dp))
            }
        }
        Text(
            "A thermometer's readings count once it has caught up with the others: within about an hour of a move for temperature, " +
                "but some humidity sensors take hours.",
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 2.dp),
        )
    }
}

@Composable
private fun Check(ok: Boolean, text: String) {
    Row(Modifier.padding(top = 4.dp)) {
        Icon(
            if (ok) Icons.Default.CheckCircle else Icons.Default.Warning, null, tint = if (ok) Fine else Marginal,
            modifier = Modifier.padding(top = 2.dp).size(16.dp),
        )
        Text(text, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f).padding(start = 8.dp))
    }
}

@Composable
private fun Checking(model: ThermoModel, session: CalibrationSession, run: CalibrationRun?, now: Long, onFinish: () -> Unit) {
    Section("Written") {
        if (session.pending.isNotEmpty()) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                Text("  Writing to ${session.pending.size} more…", style = MaterialTheme.typography.bodyLarge)
            }
        }
        session.written?.let {
            Text(
                "Corrections written at ${formatMoment(it, now)}. Leave them together a while longer: what is left to correct should stay small.",
                style = MaterialTheme.typography.bodyLarge, modifier = Modifier.padding(vertical = 4.dp),
            )
        }
        for (address in session.failed.keys) {
            val name = model.devices[address]?.let { model.numberedName(it) } ?: address
            val why = model.state(address).last?.takeIf { !it.ok }?.message ?: "failed"
            Text("$name: not written ($why)", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
        }
        if (session.failed.isNotEmpty()) {
            OutlinedButton(onClick = { model.retryCalibrationWrites() }) { Text("Retry ${session.failed.size}") }
        }
        val left = run?.let { r ->
            val midT = (r.report.temperature.range.first + r.report.temperature.range.second) / 2
            val midH = (r.report.humidity.range.first + r.report.humidity.range.second) / 2
            r.report.sensors.maxOfOrNull { abs(netCorrection(it.temperature, midT)) } to r.report.sensors.maxOfOrNull { abs(netCorrection(it.humidity, midH)) }
        }
        when {
            left != null -> Text(
                "Left to correct now: up to %.2f °C and %.1f %%RH.".format(left.first ?: 0.0, left.second ?: 0.0),
                style = MaterialTheme.typography.bodyLarge, modifier = Modifier.padding(top = 4.dp),
            )
            session.result?.exceptionOrNull()?.let(::firstEstimateIn) != null -> Text(
                "A first check in about ${formatEta(firstEstimateIn(session.result!!.exceptionOrNull()!!)!!)}.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        Button(onClick = onFinish, modifier = Modifier.padding(top = 8.dp)) { Text("Finish") }
    }
}

@Composable
private fun CorrectionsCard(model: ThermoModel, run: CalibrationRun, checking: Boolean) {
    var open by remember { mutableStateOf(false) }
    val r = run.report
    val midT = (r.temperature.range.first + r.temperature.range.second) / 2
    val midH = (r.humidity.range.first + r.humidity.range.second) / 2
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(vertical = 8.dp)) {
            Row(Modifier.fillMaxWidth().clickable { open = !open }.padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (checking) "Left to correct, per thermometer" else "Corrections, per thermometer",
                    style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary, modifier = Modifier.weight(1f),
                )
                Icon(if (open) Icons.Default.ExpandLess else Icons.Default.ExpandMore, if (open) "Hide" else "Show")
            }
            if (!open) return@Column
            run.devices.zip(r.sensors).forEachIndexed { i, (device, sensor) ->
                if (i > 0) HorizontalDivider(Modifier.padding(horizontal = 16.dp))
                val (tWritten, hWritten) = run.corrections(i)
                Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ColorDot(model.colorOf(device.first))
                        val number = model.deviceNumber(device.first)?.let { "#$it " } ?: ""
                        Text("  $number${device.second}", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                        val note = notes(sensor.temperature, sensor.humidity)
                        when {
                            run.isReference(i) -> Text(
                                "reference", style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.primary, fontWeight = FontWeight.Bold,
                            )
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
                    FitRow("°C", sensor.temperature, netCorrection(sensor.temperature, midT), TEMPERATURE_BAR, tWritten != null, 3,
                        deviceUncertainty(run, i, true)?.let { "±%.3f".format(it) }, sensor.temperature.lag?.let { "lag ${formatLag(it)}" })
                    FitRow("%RH", sensor.humidity, netCorrection(sensor.humidity, midH), HUMIDITY_BAR, hWritten != null, 2,
                        deviceUncertainty(run, i, false)?.let { "±%.2f".format(it) }, null)
                }
            }
            Text(
                "Bars show the net correction mid-range; ± is its uncertainty. An outlier is left alone: something sets it apart from the others.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
    }
}

@Composable
private fun FitRow(unit: String, fit: Fit, net: Double, scale: BarScale, written: Boolean, decimals: Int, uncertainty: String?, extra: String?) {
    Row(Modifier.fillMaxWidth().padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(
            "${signed(net, decimals)} $unit", style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium,
            modifier = Modifier.width(100.dp),
        )
        CorrectionBar(net, scale, written, Modifier.width(72.dp).height(12.dp))
        Column(Modifier.padding(start = 12.dp)) {
            Text(
                listOfNotNull(uncertainty, "gain " + if (fit.gainFitted) "%.4f".format(fit.gain) else "–").joinToString(" · "),
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

/** Adds or removes thermometers; at least two stay. */
@Composable
private fun DevicesDialog(model: ThermoModel, session: CalibrationSession, onDone: () -> Unit) {
    var picked by remember { mutableStateOf(session.devices.toSet()) }
    AlertDialog(
        onDismissRequest = onDone,
        title = { Text("Thermometers") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                for (device in model.devices.all()) {
                    Row(Modifier.fillMaxWidth().clickable {
                        picked = if (device.address in picked) picked - device.address else picked + device.address
                    }, verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(device.address in picked, onCheckedChange = null)
                        ColorDot(model.colorOf(device.address), 10.dp, Modifier.padding(start = 8.dp))
                        Text("  ${model.numberedName(device)}")
                    }
                }
            }
        },
        confirmButton = {
            TextButton(enabled = picked.size >= 2, onClick = { model.setCalibrationDevices(picked.toList()); onDone() }) { Text("Use") }
        },
        dismissButton = { TextButton(onClick = onDone) { Text("Cancel") } },
    )
}

@Composable
private fun ReferenceDialog(model: ThermoModel, session: CalibrationSession, onDone: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDone,
        title = { Text("Correct to") },
        text = {
            Column {
                Option("Their average", session.reference == null) { model.setCalibrationReference(null); onDone() }
                for (address in session.devices) {
                    val device = model.devices[address] ?: continue
                    Option(model.numberedName(device), session.reference == address) { model.setCalibrationReference(address); onDone() }
                }
                Text(
                    "A thermometer you trust keeps its calibration and the others are corrected to read like it.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = { TextButton(onClick = onDone) { Text("Close") } },
    )
}

@Composable
private fun Option(label: String, selected: Boolean, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onClick), verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected, onClick = onClick)
        Text(label)
    }
}

/** Moves the start of the readings used, in 15-minute steps. */
@Composable
private fun StartDialog(model: ThermoModel, session: CalibrationSession, now: Long, onDone: () -> Unit) {
    val together = remember(session.devices) { model.suggestedCalibrationStart(session.devices) }
    AlertDialog(
        onDismissRequest = onDone,
        title = { Text("Readings since ${formatMoment(session.start, now)}") },
        text = {
            Column {
                Text(
                    "Earlier readings count only if the thermometers were already together then. Not before their last calibration.",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    for ((label, step) in listOf("−1 h" to -4, "−15 min" to -1, "+15 min" to 1, "+1 h" to 4)) {
                        OutlinedButton(onClick = { model.setCalibrationStart(session.start + step * WINDOW_STEP_MS) }) { Text(label, maxLines = 1) }
                    }
                }
                TextButton(onClick = { model.setCalibrationStart(together) }) {
                    Text("Since they read alike (${formatMoment(together, now)})")
                }
                TextButton(onClick = { model.setCalibrationStart(now) }) { Text("From now") }
            }
        },
        confirmButton = { TextButton(onClick = onDone) { Text("Done") } },
    )
}

/** The home screen's card for a calibration in progress, or for starting one. */
@Composable
fun CalibrationCard(model: ThermoModel, now: Long, onOpen: () -> Unit) {
    val session = model.calibration
    Card(
        Modifier.fillMaxWidth().clickable(onClick = onOpen),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer),
    ) {
        Column(Modifier.padding(14.dp)) {
            if (session == null) {
                Text("Calibrate thermometers", style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
                Text(
                    "Put them side by side, pick them, and see how precise they get as time passes.",
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            } else {
                Text(
                    "Calibrating ${session.devices.size} thermometers",
                    style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
                Text(
                    model.calibrationStatus()?.substringAfter(" · ", "")?.ifEmpty { null } ?: "since ${formatMoment(session.start, now)}",
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
        }
    }
}
