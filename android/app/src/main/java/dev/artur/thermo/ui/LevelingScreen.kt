/*
 * Signal levelling: listen for a while, then set each device's TX power so it arrives at
 * about the target strength, still reliable but with no battery spent on a louder radio.
 */
package dev.artur.thermo.ui

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
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Remove
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.artur.thermo.model.Leveling
import dev.artur.thermo.model.MIN_ADVERTS
import dev.artur.thermo.model.Metric
import dev.artur.thermo.model.RSSI_FINE
import dev.artur.thermo.model.TARGET_RANGE
import dev.artur.thermo.model.ThermoModel
import kotlin.math.abs
import kotlin.math.roundToInt

/** A planned TX power this far from the wanted one means the radio's range ends there. */
private const val CLAMPED_DB = 1.0f

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LevelingScreen(
    model: ThermoModel,
    leveling: Leveling,
    revision: Long,
    now: Long,
    snackbar: SnackbarHostState,
    onBack: () -> Unit,
) {
    var confirm by remember { mutableStateOf<String?>(null) }
    val plan = model.levelingPlan() ?: return
    val changes = model.levelingChanges()
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = { Text("Signal levelling") },
                actions = {
                    IconButton(onClick = { model.restartLeveling() }) { Icon(Icons.Default.Refresh, "Listen anew") }
                    var menu by remember { mutableStateOf(false) }
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, "More") }
                    DropdownMenu(menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Restore factory TX power…") }, onClick = {
                            menu = false
                            confirm = "reset"
                        })
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
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("Target", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                        Text("${leveling.target} dBm", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        IconButton(onClick = { model.setLevelingTarget(leveling.target - 1) }) { Icon(Icons.Default.Remove, "1 dB weaker") }
                        Slider(
                            value = leveling.target.toFloat(),
                            onValueChange = { model.setLevelingTarget(it.roundToInt()) },
                            valueRange = TARGET_RANGE.first.toFloat()..TARGET_RANGE.last.toFloat(),
                            modifier = Modifier.weight(1f),
                        )
                        IconButton(onClick = { model.setLevelingTarget(leveling.target + 1) }) { Icon(Icons.Default.Add, "1 dB stronger") }
                    }
                    Text(
                        "Each device's median signal here is moved to the target by changing its TX power. " +
                            "Reception counts as reliable from $RSSI_FINE dBm. Listening for ${formatDuration(now - leveling.startedAt)}; " +
                            "each device needs $MIN_ADVERTS adverts.",
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(vertical = 4.dp)) {
                    plan.rows.forEachIndexed { i, row ->
                        if (i > 0) HorizontalDivider(Modifier.padding(horizontal = 12.dp))
                        val device = model.devices[row.address] ?: return@forEachIndexed
                        val state = model.state(row.address)
                        val config = state.config
                        val included = row.address in model.levelingDevices
                        Row(Modifier.padding(horizontal = 4.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(included, onCheckedChange = {
                                model.levelingDevices = if (included) model.levelingDevices - row.address else model.levelingDevices + row.address
                                model.onTick()
                            })
                            ColorDot(deviceColor(i))
                            Column(Modifier.weight(1f).padding(start = 8.dp)) {
                                Text(device.name, style = MaterialTheme.typography.titleSmall)
                                Text(
                                    listOfNotNull(
                                        config?.txPower?.toString() ?: "TX ?",
                                        config?.let { "adv ${formatInterval(it.advertisingInterval)}" },
                                        row.median?.let { "median $it dBm" },
                                    ).joinToString(" · "),
                                    style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                if (row.median == null) {
                                    LinearProgressIndicator(
                                        progress = { row.adverts.toFloat() / MIN_ADVERTS },
                                        modifier = Modifier.fillMaxWidth().padding(top = 4.dp).height(3.dp),
                                    )
                                }
                            }
                            Column(horizontalAlignment = Alignment.End, modifier = Modifier.width(110.dp)) {
                                when {
                                    !included -> Text("left out", color = MaterialTheme.colorScheme.outline, style = MaterialTheme.typography.labelLarge)
                                    state.busy != null -> Row(verticalAlignment = Alignment.CenterVertically) {
                                        CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
                                        Text(" ${state.busy}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                                    }
                                    row.median == null -> Text("${row.adverts}/$MIN_ADVERTS", style = MaterialTheme.typography.labelLarge)
                                    row.change == null -> Text("ok", color = Fine, style = MaterialTheme.typography.labelLarge)
                                    else -> {
                                        Text("%+d dB".format(row.change), fontWeight = FontWeight.Bold)
                                        planned(config?.txPower?.dbm, row.change)?.let {
                                            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            Button(onClick = { confirm = "apply" }, enabled = changes.isNotEmpty(), modifier = Modifier.fillMaxWidth()) {
                Text(if (changes.isEmpty()) "Nothing to change yet" else "Change TX power of ${changes.size} device(s)")
            }

            Card(Modifier.fillMaxWidth()) {
                val series = model.devices.all().mapIndexed { i, d -> ChartSeries(d.address, d.name, deviceColor(i), d.history) }
                    .filter { it.address in model.levelingDevices }
                HistoryChart(
                    series, Metric.Rssi, now - model.window, now, revision,
                    Modifier.fillMaxWidth().height(200.dp).padding(12.dp),
                    // Single adverts scatter by several dB; lines would only show the noise.
                    points = true,
                )
            }
            Spacer(Modifier.height(24.dp))
        }
    }
    when (confirm) {
        "apply" -> ConfirmDialog(
            "Change TX power?",
            changes.joinToString("\n") { (address, change) -> "${model.devices[address]?.name}: %+d dB".format(change) } +
                "\n\nEach change is written to the device's flash.",
            "Change",
            onConfirm = { model.applyLeveling() },
            onDismiss = { confirm = null },
        )
        "reset" -> {
            val resets = model.factoryResets()
            ConfirmDialog(
                "Restore factory TX power?",
                if (resets.isEmpty()) "All devices already use the factory TX power (+0.0 dBm)."
                else "${resets.size} device(s) go back to +0.0 dBm.",
                "Restore",
                onConfirm = { model.resetTxPower() },
                onDismiss = { confirm = null },
            )
        }
    }
}

/** `"→ −5.0 dBm"`, with "(limit)" when the radio's range ends before the wanted level. */
private fun planned(currentDbm: Float?, change: Int): String? {
    val dbm = currentDbm ?: return null
    val target = dev.artur.thermo.atc.TxPower.nearest(dbm + change)
    val reached = target.dbm ?: return null
    val clamped = abs(reached - (dbm + change)) > CLAMPED_DB
    return "→ $target" + if (clamped) " (limit)" else ""
}
