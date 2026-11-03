/*
 * The dashboard: a chart above one card per thermometer. Tap a card for its details;
 * long-press to select several and act on all of them from the contextual top bar.
 */
package dev.artur.thermo.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ShowChart
import androidx.compose.material.icons.filled.BatteryFull
import androidx.compose.material.icons.filled.BluetoothSearching
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.ScatterPlot
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.SignalCellularAlt
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material.icons.filled.WaterDrop
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.artur.thermo.model.Device
import dev.artur.thermo.model.Metric
import dev.artur.thermo.model.STALE_AFTER_MS
import dev.artur.thermo.model.ScanStatus
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.WINDOWS

/** Devices heard more recently are simply live; from then on the card says how long ago. */
private const val QUIET_AFTER_MS = 30_000L

/** What the home screen's menus can open; handled by the activity. */
interface HomeActions {
    fun openDevice(address: String)
    fun openCalibration()
    fun openLeveling()
    fun openSettings(targets: List<String>)
    fun openRename(address: String)
    fun openData()
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(model: ThermoModel, revision: Long, now: Long, snackbar: SnackbarHostState, actions: HomeActions) {
    val selecting = model.selected.isNotEmpty()
    var points by rememberSaveable { mutableStateOf(false) }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            if (selecting) SelectionBar(model, actions) else TopAppBar(
                title = {
                    Column {
                        Text("Thermometers")
                        Text(scanLine(model, now), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                },
                actions = {
                    var menu by remember { mutableStateOf(false) }
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, "More") }
                    DropdownMenu(menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Calibrate") }, onClick = { menu = false; actions.openCalibration() })
                        DropdownMenuItem(text = { Text("Signal levelling") }, onClick = { menu = false; actions.openLeveling() })
                        DropdownMenuItem(text = { Text("Select all") }, onClick = { menu = false; model.selectAll() })
                        DropdownMenuItem(text = { Text("Data and recording") }, onClick = { menu = false; actions.openData() })
                    }
                },
            )
        },
    ) { padding ->
        val devices = model.devices.all().toList()
        LazyColumn(
            Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(start = 12.dp, end = 12.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item(key = "chart") {
                ChartCard(model, devices, revision, now, points, onTogglePoints = { points = !points })
            }
            if (devices.isEmpty()) {
                item(key = "empty") { EmptyState(model) }
            }
            items(devices, key = { it.address }) { device ->
                DeviceCard(model, device, now, selecting,
                    onClick = { if (selecting) model.toggleSelected(device.address) else actions.openDevice(device.address) },
                    onLongClick = { model.toggleSelected(device.address) })
            }
        }
    }
}

private fun scanLine(model: ThermoModel, now: Long): String {
    val heard = model.devices.all().count { now - it.lastSeen < STALE_AFTER_MS }
    return when (val s = model.scanStatus) {
        ScanStatus.Starting -> "Starting…"
        ScanStatus.Scanning -> "Listening · $heard of ${model.devices.size} in range" + if (model.busy) " · connecting" else ""
        is ScanStatus.Failed -> "Not scanning: ${s.reason}"
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SelectionBar(model: ThermoModel, actions: HomeActions) {
    val targets = model.selected.toList()
    TopAppBar(
        colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        navigationIcon = { IconButton(onClick = { model.clearSelection() }) { Icon(Icons.Default.Close, "Clear selection") } },
        title = { Text("${targets.size}", maxLines = 1) },
        actions = {
            IconButton(onClick = { model.identify(targets) }) { Icon(Icons.Default.Visibility, "Identify") }
            IconButton(onClick = { model.syncClocks(targets) }) { Icon(Icons.Default.Schedule, "Sync clocks") }
            IconButton(onClick = { model.download(targets) }) { Icon(Icons.Default.Download, "Download logs") }
            IconButton(onClick = { actions.openSettings(targets) }) { Icon(Icons.Default.Tune, "Settings") }
            var menu by remember { mutableStateOf(false) }
            IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, "More") }
            DropdownMenu(menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("Select all") }, onClick = { menu = false; model.selectAll() })
                DropdownMenuItem(text = { Text("Read settings") }, onClick = { menu = false; model.readConfig(targets) })
                if (targets.size == 1) {
                    DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; actions.openRename(targets[0]) })
                }
                if (targets.size >= 2) {
                    DropdownMenuItem(text = { Text("Calibrate these") }, onClick = { menu = false; actions.openCalibration() })
                }
                DropdownMenuItem(text = { Text("Level signal of these") }, onClick = { menu = false; actions.openLeveling() })
            }
        },
    )
}

@Composable
fun MetricSelector(metric: Metric, onSelect: (Metric) -> Unit, modifier: Modifier = Modifier) {
    SingleChoiceSegmentedButtonRow(modifier) {
        Metric.entries.forEachIndexed { i, m ->
            SegmentedButton(
                selected = metric == m,
                onClick = { onSelect(m) },
                shape = SegmentedButtonDefaults.itemShape(i, Metric.entries.size),
                icon = {},
                label = { Text(if (m == Metric.Temperature) "Temp" else m.label, maxLines = 1) },
            )
        }
    }
}

@Composable
fun SpanChips(selected: Int, onSelect: (Int) -> Unit, modifier: Modifier = Modifier) {
    Row(modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        WINDOWS.forEachIndexed { i, span ->
            FilterChip(selected = selected == i, onClick = { onSelect(i) }, label = { Text(formatSpan(span)) })
        }
    }
}

@Composable
private fun ChartCard(model: ThermoModel, devices: List<Device>, revision: Long, now: Long, points: Boolean, onTogglePoints: () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp)) {
            MetricSelector(model.metric, { model.metric = it }, Modifier.fillMaxWidth())
            val selected = model.selected
            val series = devices.mapIndexed { i, d -> ChartSeries(d.address, d.name, deviceColor(i), d.history) }
                .filter { selected.isEmpty() || it.address in selected }
            HistoryChart(
                series, model.metric, now - model.window, now, revision,
                Modifier.fillMaxWidth().height(240.dp).padding(top = 12.dp),
                points = points,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                SpanChips(model.windowIndex, { model.windowIndex = it }, Modifier.weight(1f))
                IconButton(onClick = onTogglePoints) {
                    Icon(if (points) Icons.AutoMirrored.Filled.ShowChart else Icons.Default.ScatterPlot, if (points) "Lines" else "Points")
                }
            }
        }
    }
}

@Composable
private fun EmptyState(model: ThermoModel) {
    Column(Modifier.fillMaxWidth().padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        if (model.scanStatus == ScanStatus.Scanning) CircularProgressIndicator(Modifier.size(32.dp))
        else Icon(Icons.Default.BluetoothSearching, null, Modifier.size(32.dp))
        Spacer(Modifier.height(12.dp))
        Text("Looking for thermometers…", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "Thermometers need the pvvx ATC_MiThermometer firmware with advertising set to BTHome v2. " +
                "They show up here as soon as one of their adverts is heard.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DeviceCard(
    model: ThermoModel,
    device: Device,
    now: Long,
    selecting: Boolean,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
) {
    val number = model.deviceNumber(device.address) ?: 0
    val selected = device.address in model.selected
    val stale = now - device.lastSeen > STALE_AFTER_MS
    val state = model.state(device.address)
    Card(
        Modifier.fillMaxWidth().combinedClickable(onClick = onClick, onLongClick = onLongClick),
        border = if (selected) BorderStroke(2.dp, MaterialTheme.colorScheme.primary) else null,
    ) {
        Row(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            if (selecting) {
                Checkbox(checked = selected, onCheckedChange = { onClick() })
            } else {
                Box(Modifier.width(36.dp), contentAlignment = Alignment.Center) {
                    ColorDot(deviceColor(number - 1), 14.dp)
                }
            }
            Column(Modifier.weight(1f).padding(start = 4.dp).alpha(if (stale) 0.55f else 1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(device.name, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                    Text("  #$number", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    Indicator(Icons.Default.BatteryFull, device.battery?.let { "$it %" } ?: "–", batteryColor(device.battery).takeUnless { stale })
                    Indicator(Icons.Default.SignalCellularAlt, device.typicalRssi?.let { "$it dBm" } ?: "–", rssiColor(device.typicalRssi).takeUnless { stale })
                    if (now - device.lastSeen >= QUIET_AFTER_MS) Text(formatAgo(now - device.lastSeen), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, softWrap = false)
                }
                JobStatus(state.busy, state.queued, state.last)
            }
            Column(horizontalAlignment = Alignment.End, modifier = Modifier.padding(start = 8.dp).alpha(if (stale) 0.55f else 1f)) {
                if (device.encrypted) {
                    Text("locked", style = MaterialTheme.typography.titleMedium)
                } else {
                    Text(device.temperature?.let { "%.2f °C".format(it) } ?: "–", fontSize = 22.sp, fontWeight = FontWeight.Medium, maxLines = 1, softWrap = false)
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Default.WaterDrop, null, Modifier.size(14.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        Text(device.humidity?.let { " %.1f %%".format(it) } ?: " –", style = MaterialTheme.typography.bodyLarge)
                    }
                }
            }
        }
    }
}

@Composable
fun Indicator(icon: ImageVector, text: String, tint: androidx.compose.ui.graphics.Color?) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, null, Modifier.size(14.dp), tint = tint ?: MaterialTheme.colorScheme.onSurfaceVariant)
        Text(" $text", style = MaterialTheme.typography.labelMedium, color = tint ?: MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** A running job with a spinner, else the last job's result. */
@Composable
fun JobStatus(busy: String?, queued: Int, last: Pair<Boolean, String>?) {
    AnimatedVisibility(busy != null || queued > 0 || last != null) {
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 2.dp)) {
            when {
                busy != null -> {
                    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                    Text("  $busy…" + if (queued > 1) " (+${queued - 1})" else "", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                }
                queued > 0 -> Text("waiting to connect" + if (queued > 1) " ($queued jobs)" else "", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
                last != null -> {
                    Icon(if (last.first) Icons.Default.CheckCircle else Icons.Default.Error, null, Modifier.size(12.dp), tint = if (last.first) Fine else Bad)
                    Text(" ${last.second}", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}
