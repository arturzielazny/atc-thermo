/*
 * The settings sheet, rename dialog and confirmations for actions that write to devices.
 */
package dev.artur.thermo.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.MenuAnchorType
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import dev.artur.thermo.atc.TxPower
import dev.artur.thermo.model.Settings
import dev.artur.thermo.model.ThermoModel

/** Each field is "Keep" (`null`) or one of a few presets, as in the desktop form. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsSheet(model: ThermoModel, targets: List<String>, onDismiss: () -> Unit) {
    var advertising by remember { mutableStateOf<Double?>(null) }
    var measure by remember { mutableStateOf<Int?>(null) }
    var power by remember { mutableStateOf<TxPower?>(null) }
    var log by remember { mutableStateOf<Double?>(null) }
    val single = targets.singleOrNull()?.let { model.state(it).config }
    // Show what a single device has now: read its settings if they aren't known.
    LaunchedEffect(targets) {
        val unknown = targets.filter { model.state(it).config == null && model.state(it).queued == 0 }
        if (targets.size == 1) model.readConfig(unknown)
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.padding(horizontal = 20.dp).navigationBarsPadding(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(
                if (targets.size == 1) "Settings of ${model.devices[targets[0]]?.name}" else "Settings of ${targets.size} devices",
                style = MaterialTheme.typography.titleLarge,
            )
            Text(
                "Fields left at “Keep” are not changed. Every change is written to the device's flash.",
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Choice("Advertising interval", advertising, Settings.ADVERTISING, { formatInterval(it) }, single?.let { formatInterval(it.advertisingInterval) }) { advertising = it }
            Choice("Measure every", measure, Settings.MEASURE_EVERY, { "$it adverts" }, single?.let { "${it.measureInterval} adverts" }) { measure = it }
            Choice("TX power", power, TxPower.PRESETS, { it.toString() }, single?.txPower?.toString()) { power = it }
            Choice("Log record every", log, Settings.LOG_EVERY, { formatInterval(it) }, single?.let { c -> c.logPeriod?.let { formatInterval(it) } ?: "off" }) { log = it }
            val period = advertising?.let { a -> measure?.let { a * it } }
            if (period != null) {
                Text("A measurement every ${formatInterval(period)}.", style = MaterialTheme.typography.bodyMedium)
            }
            val settings = Settings(advertising, measure, power, log)
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = onDismiss) { Text("Cancel") }
                Button(enabled = !settings.isEmpty, onClick = { model.configure(targets, settings); onDismiss() }) {
                    Text(if (targets.size == 1) "Write" else "Write to ${targets.size}")
                }
            }
            Spacer(Modifier.height(12.dp))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun <T> Choice(label: String, value: T?, options: List<T>, show: (T) -> String, current: String?, onChange: (T?) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val keep = "Keep" + (current?.let { " ($it)" } ?: "")
    ExposedDropdownMenuBox(expanded, onExpandedChange = { expanded = it }) {
        OutlinedTextField(
            value = value?.let(show) ?: keep,
            onValueChange = {},
            readOnly = true,
            label = { Text(label) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor(MenuAnchorType.PrimaryNotEditable),
        )
        ExposedDropdownMenu(expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(text = { Text(keep) }, onClick = { onChange(null); expanded = false })
            for (option in options) {
                DropdownMenuItem(text = { Text(show(option)) }, onClick = { onChange(option); expanded = false })
            }
        }
    }
}

@Composable
fun RenameDialog(model: ThermoModel, address: String, onDismiss: () -> Unit) {
    var name by remember { mutableStateOf(model.devices[address]?.name ?: "") }
    val problem = model.nameProblem(name)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename") },
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    singleLine = true,
                    isError = problem != null,
                    supportingText = { Text(problem ?: "Empty restores the default ATC_xxxxxx. The device restarts with it.") },
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                )
            }
        },
        confirmButton = {
            TextButton(enabled = problem == null, onClick = { model.rename(address, name.trim()); onDismiss() }) { Text("Rename") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Asks before something that writes to device flash. */
@Composable
fun ConfirmDialog(title: String, text: String, confirm: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = { TextButton(onClick = { onConfirm(); onDismiss() }) { Text(confirm) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
