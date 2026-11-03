/*
 * One thermometer: its readings, chart, radio and clock, settings and the actions that
 * connect to it.
 */
package dev.artur.thermo.ui

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
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Schedule
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material.icons.filled.WaterDrop
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.formatOffset

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun DeviceScreen(
    model: ThermoModel,
    address: String,
    revision: Long,
    now: Long,
    snackbar: SnackbarHostState,
    onBack: () -> Unit,
    onSettings: () -> Unit,
    onRename: () -> Unit,
) {
    val device = model.devices[address] ?: run {
        LaunchedEffect(address) { onBack() }
        return
    }
    val index = (model.deviceNumber(address) ?: 1) - 1
    val state = model.state(address)
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ColorDot(deviceColor(index))
                        Text("  ${device.name}")
                    }
                },
                actions = {
                    IconButton(onClick = onRename) { Icon(Icons.Default.Edit, "Rename") }
                    var menu by remember { mutableStateOf(false) }
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, "More") }
                    DropdownMenu(menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Read firmware and calibration") }, onClick = { menu = false; model.readInfo(address) })
                        DropdownMenuItem(text = { Text("Read settings") }, onClick = { menu = false; model.readConfig(listOf(address)) })
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp)) {
                    Row(verticalAlignment = Alignment.Bottom) {
                        Text(device.temperature?.let { "%.2f".format(it) } ?: "–", fontSize = 44.sp, fontWeight = FontWeight.Light)
                        Text(" °C", fontSize = 22.sp, modifier = Modifier.padding(bottom = 6.dp))
                        Spacer(Modifier.weight(1f))
                        Icon(Icons.Default.WaterDrop, null, Modifier.size(22.dp).padding(bottom = 8.dp), tint = MaterialTheme.colorScheme.primary)
                        Text(device.humidity?.let { "%.1f".format(it) } ?: "–", fontSize = 32.sp, fontWeight = FontWeight.Light)
                        Text(" %", fontSize = 18.sp, modifier = Modifier.padding(bottom = 4.dp))
                    }
                    Text("#${index + 1} · seen ${formatAgo(now - device.lastSeen)}", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    JobStatus(state.busy, state.queued, state.last)
                }
            }

            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AssistChip(onClick = { model.identify(listOf(address)) }, label = { Text("Identify") },
                    leadingIcon = { Icon(Icons.Default.Visibility, null, Modifier.size(18.dp)) })
                AssistChip(onClick = { model.syncClocks(listOf(address)) }, label = { Text("Sync clock") },
                    leadingIcon = { Icon(Icons.Default.Schedule, null, Modifier.size(18.dp)) })
                AssistChip(onClick = { model.download(listOf(address)) }, label = { Text("Download log") },
                    leadingIcon = { Icon(Icons.Default.Download, null, Modifier.size(18.dp)) })
                AssistChip(onClick = onSettings, label = { Text("Settings") },
                    leadingIcon = { Icon(Icons.Default.Tune, null, Modifier.size(18.dp)) })
            }

            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(12.dp)) {
                    MetricSelector(model.metric, { model.metric = it }, Modifier.fillMaxWidth())
                    HistoryChart(
                        listOf(ChartSeries(address, device.name, deviceColor(index), device.history)),
                        model.metric, now - model.window, now, revision,
                        Modifier.fillMaxWidth().height(220.dp).padding(top = 12.dp),
                    )
                    SpanChips(model.windowIndex, { model.windowIndex = it })
                }
            }

            Section("Device") {
                Fact("Address", device.address)
                Fact("Battery", listOfNotNull(device.battery?.let { "$it %" }, device.voltage?.let { "%.3f V".format(it) }).joinToString(" · ").ifEmpty { "–" })
                Fact("Signal", "${device.rssi?.let { "$it dBm" } ?: "–"} now · ${device.typicalRssi?.let { "$it dBm" } ?: "–"} median (5 min)")
                Fact("Clock", state.clockOffset?.let { formatOffset(it) + if (it >= 1.5) " ahead" else if (it <= -1.5) " behind" else "" } ?: "not measured yet")
                Fact("Firmware", state.firmware?.let { "v$it" } ?: "–")
                state.coefficients?.let {
                    Fact("Temperature coefficients", "k ${it.temperatureGain} · z ${it.temperatureOffset}")
                    Fact("Humidity coefficients", "k ${it.humidityGain} · z ${it.humidityOffset}")
                }
                Fact("History in memory", "${device.history.size} samples")
                if (state.firmware == null) {
                    TextButton(onClick = { model.readInfo(address) }) {
                        Icon(Icons.Default.Info, null, Modifier.size(18.dp))
                        Text("  Read firmware, clock and calibration")
                    }
                }
            }

            Section("Settings") {
                val config = state.config
                if (config == null) {
                    Text("Not read yet.", color = MaterialTheme.colorScheme.onSurfaceVariant)
                } else {
                    Fact("TX power", config.txPower.toString())
                    Fact("Advertising interval", formatInterval(config.advertisingInterval))
                    Fact("Measurement every", "${formatInterval(config.measurementPeriod)} (${config.measureInterval} adverts)")
                    Fact("Log record every", config.logPeriod?.let { formatInterval(it) } ?: "off")
                }
                Row {
                    TextButton(onClick = { model.readConfig(listOf(address)) }) { Text("Read") }
                    TextButton(onClick = onSettings) { Text("Change…") }
                }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable
fun Section(title: String, content: @Composable () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp)) {
            Text(title, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.height(6.dp))
            content()
        }
    }
}

@Composable
fun Fact(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Text(label, Modifier.weight(0.4f), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
        Text(value, Modifier.weight(0.6f), style = MaterialTheme.typography.bodyMedium)
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.4f))
}
