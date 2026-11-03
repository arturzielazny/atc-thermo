/*
 * The single activity: asks for Bluetooth permissions, then shows the screens on a small
 * back stack. Intent extra `screen` (`calibration`, `leveling`, `data`, or a device
 * address) opens a screen directly, e.g. from `adb shell am start`.
 */
package dev.artur.thermo

import android.bluetooth.BluetoothAdapter
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bluetooth
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.artur.thermo.model.ScanStatus
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.ui.CalibrationScreen
import dev.artur.thermo.ui.DataScreen
import dev.artur.thermo.ui.DeviceScreen
import dev.artur.thermo.ui.HomeActions
import dev.artur.thermo.ui.HomeScreen
import dev.artur.thermo.ui.LevelingScreen
import dev.artur.thermo.ui.RenameDialog
import dev.artur.thermo.ui.SettingsSheet
import dev.artur.thermo.ui.ThermoTheme
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.onEach

/** How often the UI follows the model at most; adverts arrive several times a second. */
private const val REDRAW_INTERVAL_MS = 400L

sealed interface Screen {
    data object Home : Screen
    data class Device(val address: String) : Screen
    data object Calibration : Screen
    data object Leveling : Screen
    data object Data : Screen
}

class MainActivity : ComponentActivity() {
    private val stack = mutableStateListOf<Screen>(Screen.Home)
    private var permitted by mutableStateOf(false)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        DebugHooks.onCreate(this)
        permitted = controller.hasPermissions()
        handle(intent)
        setContent { ThermoTheme { Surface { Root() } } }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    override fun onStart() {
        super.onStart()
        App.visible = true
        permitted = controller.hasPermissions()
        controller.startIfReady()
    }

    override fun onStop() {
        App.visible = false
        // Without background recording, scanning ends with the app; queued jobs still finish.
        if (!controller.recordInBackground && !isChangingConfigurations) controller.stopScanning()
        super.onStop()
    }

    private fun handle(intent: Intent?) {
        val model = controller.model
        if (intent?.getBooleanExtra("demo", false) == true) DebugHooks.loadDemo(model)
        val target = intent?.getStringExtra("screen") ?: return
        open(when (target) {
            "calibration" -> Screen.Calibration
            "leveling" -> Screen.Leveling
            "data" -> Screen.Data
            else -> Screen.Device(target)
        }, model)
    }

    private fun open(screen: Screen, model: ThermoModel) {
        when (screen) {
            Screen.Calibration -> if (model.calibration == null) {
                model.openCalibration()
                controller.refitNow()
            }
            Screen.Leveling -> if (model.leveling == null) model.openLeveling()
            else -> {}
        }
        stack.removeAll { it == screen }
        stack.add(screen)
    }

    private fun back() {
        val model = controller.model
        when (stack.removeAt(stack.lastIndex)) {
            Screen.Calibration -> model.closeCalibration()
            Screen.Leveling -> model.closeLeveling()
            else -> {}
        }
    }

    @Composable
    private fun Root() {
        val permissions = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
            permitted = controller.hasPermissions()
            controller.startIfReady()
        }
        if (!permitted) {
            PermissionScreen(
                onGrant = {
                    val wanted = Controller.requiredPermissions().toMutableList()
                    if (Build.VERSION.SDK_INT >= 33) wanted += android.Manifest.permission.POST_NOTIFICATIONS
                    permissions.launch(wanted.toTypedArray())
                },
                onSettings = {
                    startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$packageName")))
                },
            )
            return
        }
        val model = controller.model
        // StateFlow keeps only the latest value, so a slow collector skips intermediate ones.
        val revision by remember { model.revision.onEach { delay(REDRAW_INTERVAL_MS) } }.collectAsState(0L)
        val now by produceState(System.currentTimeMillis()) {
            while (true) {
                value = System.currentTimeMillis()
                delay(1_000)
            }
        }
        val snackbar = remember { SnackbarHostState() }
        val enableBluetooth = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {}
        LaunchedEffect(model.warningId) {
            model.warning?.let { snackbar.showSnackbar(it, withDismissAction = true, duration = SnackbarDuration.Long) }
        }
        val bluetoothOff = (model.scanStatus as? ScanStatus.Failed)?.reason == "Bluetooth is off"
        LaunchedEffect(bluetoothOff) {
            if (bluetoothOff) {
                val result = snackbar.showSnackbar("Bluetooth is off", "Turn on", duration = SnackbarDuration.Indefinite)
                if (result == SnackbarResult.ActionPerformed) {
                    runCatching { enableBluetooth.launch(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE)) }
                }
            }
        }

        var settingsFor by remember { mutableStateOf<List<String>?>(null) }
        var renaming by remember { mutableStateOf<String?>(null) }
        BackHandler(enabled = stack.size > 1 || model.selected.isNotEmpty()) {
            if (stack.size > 1) back() else model.clearSelection()
        }
        val actions = object : HomeActions {
            override fun openDevice(address: String) = open(Screen.Device(address), model)
            override fun openCalibration() = open(Screen.Calibration, model)
            override fun openLeveling() = open(Screen.Leveling, model)
            override fun openSettings(targets: List<String>) { settingsFor = targets }
            override fun openRename(address: String) { renaming = address }
            override fun openData() = open(Screen.Data, model)
        }
        // Reading the revision makes every screen redraw with new data.
        @Suppress("UNUSED_EXPRESSION") revision
        when (val screen = stack.last()) {
            Screen.Home -> HomeScreen(model, revision, now, snackbar, actions)
            is Screen.Device -> DeviceScreen(
                model, screen.address, revision, now, snackbar,
                onBack = ::back,
                onSettings = { settingsFor = listOf(screen.address) },
                onRename = { renaming = screen.address },
            )
            Screen.Calibration -> model.calibration?.let {
                CalibrationScreen(model, it, revision, now, snackbar, onWindowChanged = controller::refitNow, onBack = ::back)
            }
            Screen.Leveling -> model.leveling?.let { LevelingScreen(model, it, revision, now, snackbar, onBack = ::back) }
            Screen.Data -> DataScreen(controller, snackbar, onBack = ::back)
        }
        settingsFor?.let { SettingsSheet(model, it) { settingsFor = null } }
        renaming?.let { RenameDialog(model, it) { renaming = null } }
    }
}

@Composable
private fun PermissionScreen(onGrant: () -> Unit, onSettings: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(Icons.Default.Bluetooth, null, Modifier.size(56.dp), tint = MaterialTheme.colorScheme.primary)
        Spacer(Modifier.height(16.dp))
        Text("Thermo listens for your thermometers", style = MaterialTheme.typography.headlineSmall, textAlign = TextAlign.Center)
        Spacer(Modifier.height(12.dp))
        Text(
            "It needs permission to find and connect to nearby Bluetooth devices: to read their adverts, " +
                "download their logs, sync their clocks and write calibrations. Notifications show while it records in the background.",
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        Button(onClick = onGrant) { Text("Allow Bluetooth") }
        OutlinedButton(onClick = onSettings, modifier = Modifier.padding(top = 8.dp)) { Text("Open app settings") }
    }
}
