/*
 * Recording in the background, and the history files: where they are, sharing them, and
 * importing the desktop app's `~/.local/share/thermo` files.
 */
package dev.artur.thermo.ui

import android.content.Intent
import android.net.Uri
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import dev.artur.thermo.BuildConfig
import dev.artur.thermo.Controller
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DataScreen(controller: Controller, snackbar: SnackbarHostState, onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var background by remember { mutableStateOf(controller.recordInBackground) }
    var refresh by remember { mutableIntStateOf(0) }
    val importer = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        controller.importFiles(uris) { message ->
            refresh++
            scope.launch { snackbar.showSnackbar(message) }
        }
    }
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                title = { Text("Data and recording") },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(horizontal = 12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Section("Recording") {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("Keep recording when closed", style = MaterialTheme.typography.bodyLarge)
                        Text(
                            "Calibration needs hours of live readings. A notification shows while recording.",
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Switch(background, onCheckedChange = {
                        background = it
                        controller.recordInBackground = it
                    })
                }
                val power = context.getSystemService(PowerManager::class.java)
                if (!power.isIgnoringBatteryOptimizations(context.packageName)) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        "Battery optimisation may pause recording after a while with the screen off.",
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedButton(onClick = {
                        context.startActivity(
                            Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${context.packageName}"))
                        )
                    }) { Text("Allow unrestricted battery use") }
                }
            }

            Section("History files") {
                @Suppress("UNUSED_EXPRESSION") refresh
                val files = controller.model.store?.files().orEmpty()
                Fact("Folder", controller.dataDir.absolutePath)
                val bytes = files.sumOf { it.length() }
                Fact("Files", "${files.size} · " + if (bytes < 1_000_000) "${(bytes + 999) / 1000} kB" else "%.1f MB".format(bytes / 1e6))
                Text(
                    "The same CSV files as the desktop app's ~/.local/share/thermo, so they can be copied between both.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(vertical = 6.dp),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(enabled = files.isNotEmpty(), onClick = {
                        val uris = ArrayList(files.map { FileProvider.getUriForFile(context, "${context.packageName}.files", it) })
                        val send = Intent(Intent.ACTION_SEND_MULTIPLE).apply {
                            type = "text/csv"
                            putParcelableArrayListExtra(Intent.EXTRA_STREAM, uris)
                            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                        }
                        context.startActivity(Intent.createChooser(send, "Share history"))
                    }) { Text("Share") }
                    OutlinedButton(onClick = { importer.launch(arrayOf("text/*", "application/octet-stream")) }) { Text("Import…") }
                }
            }

            Section("About") {
                Fact("Version", BuildConfig.VERSION_NAME)
                Text(
                    "For thermometers running the pvvx ATC_MiThermometer firmware with BTHome v2 advertising, " +
                        "e.g. Xiaomi LYWSD03MMC. Flash it with TelinkMiFlasher in Chrome on this phone.",
                    style = MaterialTheme.typography.bodySmall,
                )
                OutlinedButton(onClick = {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://pvvx.github.io/ATC_MiThermometer/TelinkMiFlasher.html")))
                }, modifier = Modifier.padding(top = 6.dp)) { Text("Open TelinkMiFlasher") }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}
