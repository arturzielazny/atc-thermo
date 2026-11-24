/*
 * Starting a calibration: put the thermometers side by side, pick them (the nearest come
 * first), choose what they are corrected to, and whether readings they already took
 * together count.
 */
package dev.artur.thermo.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Visibility
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.artur.thermo.model.Device
import dev.artur.thermo.model.RECENTLY_HEARD_MS
import dev.artur.thermo.model.RSSI_FINE
import dev.artur.thermo.model.STALE_AFTER_MS
import dev.artur.thermo.model.ThermoModel

/** Readings taken together for less than this aren't worth offering. */
private const val WORTH_KEEPING_MS = 10 * 60_000L

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CalibrationSetupScreen(
    model: ThermoModel,
    revision: Long,
    now: Long,
    snackbar: SnackbarHostState,
    onStarted: () -> Unit,
    onBack: () -> Unit,
) {
    @Suppress("UNUSED_EXPRESSION") revision
    // Selected on the home screen, or else nothing: picking is the point of this screen.
    var picked by remember { mutableStateOf(model.devices.all().map { it.address }.filter { it in model.selected }.toSet()) }
    var reference by remember { mutableStateOf<String?>(null) }
    var useEarlier by remember { mutableStateOf(true) }
    // Strongest first: thermometers next to the phone are the ones just put together.
    val devices = model.devices.all().sortedByDescending { it.typicalRssi ?: Int.MIN_VALUE }
    val pickedList = model.devices.all().map { it.address }.filter { it in picked }
    // Walks back through history: once a minute is enough.
    val since = remember(pickedList, now / 60_000) {
        if (pickedList.size >= 2) model.suggestedCalibrationStart(pickedList) else now
    }
    val together = now - since >= WORTH_KEEPING_MS
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = { Text("New calibration") },
            )
        },
        bottomBar = {
            Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(12.dp)) {
                Button(
                    onClick = {
                        model.startCalibration(pickedList, reference, if (together && useEarlier) since else now)
                        onStarted()
                    },
                    enabled = pickedList.size >= 2,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(if (pickedList.size >= 2) "Calibrate ${pickedList.size} thermometers" else "Pick at least two")
                }
            }
        },
    ) { padding ->
        LazyColumn(
            Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(horizontal = 12.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item(key = "how") {
                Section("How it works") {
                    Steps(
                        "Put them side by side, a few cm apart, away from heaters, sun and people.",
                        "Pick them below, nearest first. The eye shows a number on its screen.",
                        "Leave them. The app says how precise they'd be now and when better, also with the app closed.",
                    )
                }
            }
            item(key = "chips") {
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    val nearby = devices.filter { now - it.lastSeen < RECENTLY_HEARD_MS && (it.typicalRssi ?: Int.MIN_VALUE) >= RSSI_FINE }
                    FilterChip(
                        selected = nearby.isNotEmpty() && picked == nearby.map { it.address }.toSet(),
                        onClick = { picked = nearby.map { it.address }.toSet() },
                        label = { Text("Nearby (${nearby.size})") },
                        enabled = nearby.isNotEmpty(),
                    )
                    FilterChip(selected = false, onClick = { picked = emptySet() }, label = { Text("None") }, enabled = picked.isNotEmpty())
                }
            }
            items(devices, key = { it.address }) { device ->
                PickRow(model, device, device.address in picked, now) {
                    picked = if (device.address in picked) picked - device.address else picked + device.address
                    if (reference !in picked) reference = null
                }
            }
            if (pickedList.size >= 2) {
                item(key = "options") {
                    Section("Options") {
                        Text("Correct to", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            FilterChip(selected = reference == null, onClick = { reference = null }, label = { Text("Their average") })
                            for (address in pickedList) {
                                val device = model.devices[address] ?: continue
                                FilterChip(
                                    selected = reference == address,
                                    onClick = { reference = address },
                                    leadingIcon = { ColorDot(model.colorOf(address), 10.dp) },
                                    label = { Text(model.numberedName(device)) },
                                )
                            }
                        }
                        Text(
                            if (reference == null) "Every thermometer is corrected to the average of all of them." else "The others are corrected to read like this one, which keeps its own calibration.",
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        if (together) {
                            Spacer(Modifier.height(10.dp))
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Column(Modifier.weight(1f)) {
                                    Text("Use readings since ${formatMoment(since, now)}", style = MaterialTheme.typography.bodyLarge)
                                    Text(
                                        "They have read alike for ${formatDuration(now - since)}, so they were probably together already. Turn off if they weren't.",
                                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                                Switch(useEarlier, onCheckedChange = { useEarlier = it })
                            }
                        }
                    }
                }
            }
            item(key = "end") { Spacer(Modifier.height(8.dp)) }
        }
    }
}

@Composable
private fun PickRow(model: ThermoModel, device: Device, picked: Boolean, now: Long, onToggle: () -> Unit) {
    val stale = now - device.lastSeen > STALE_AFTER_MS
    Card(Modifier.fillMaxWidth().clickable(onClick = onToggle)) {
        Row(Modifier.padding(horizontal = 4.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Checkbox(checked = picked, onCheckedChange = { onToggle() })
            ColorDot(model.colorOf(device.address), 12.dp)
            Column(Modifier.weight(1f).padding(start = 8.dp).alpha(if (stale) 0.55f else 1f)) {
                Text(model.numberedName(device), style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                val reading = if (device.encrypted) "locked" else listOfNotNull(
                    device.temperature?.let { "%.2f °C".format(it) },
                    device.humidity?.let { "%.1f %%".format(it) },
                ).joinToString(" · ")
                val signal = device.typicalRssi?.let { "$it dBm" } ?: "–"
                val heard = if (stale) " · ${formatAgo(now - device.lastSeen)}" else ""
                Text(
                    "$reading · $signal$heard", style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
            }
            IconButton(onClick = { model.identify(listOf(device.address)) }) {
                Icon(Icons.Default.Visibility, "Show its number on its screen")
            }
        }
    }
}

/** A numbered list of short instructions. */
@Composable
fun Steps(vararg steps: String) {
    steps.forEachIndexed { i, step ->
        Row(Modifier.padding(vertical = 3.dp)) {
            Text("${i + 1}.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(end = 8.dp))
            Text(step, style = MaterialTheme.typography.bodyMedium)
        }
    }
}
