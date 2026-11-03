/*
 * Owns the model, history store, scanner and job worker for the whole process, so they
 * outlive the activity and keep recording in the background (see [MonitorService]).
 */
package dev.artur.thermo

import android.Manifest
import android.app.Application
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import androidx.core.content.ContextCompat
import dev.artur.thermo.ble.JobRunner
import dev.artur.thermo.ble.Scanner
import dev.artur.thermo.calibration.Options
import dev.artur.thermo.calibration.calibrate
import dev.artur.thermo.model.CalibrationRun
import dev.artur.thermo.model.HISTORY_SPAN_MS
import dev.artur.thermo.model.Measurement
import dev.artur.thermo.model.Store
import dev.artur.thermo.model.ThermoModel
import dev.artur.thermo.model.calibrationReadings
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.Executors

/** Name of the preference that keeps recording while the app is closed. */
private const val PREF_BACKGROUND = "record_in_background"
private const val PREFS = "thermo"
/** Seconds between widget redraws; readings arrive every 10 s or so. */
private const val WIDGET_INTERVAL_S = 30

class ThermoApp : Application() {
    lateinit var controller: Controller
        private set

    override fun onCreate() {
        super.onCreate()
        controller = Controller(this)
    }
}

val Context.controller: Controller get() = (applicationContext as ThermoApp).controller

class Controller(private val app: Application) {
    private val scope = MainScope()
    private val mainHandler = Handler(Looper.getMainLooper())
    private val io = Executors.newSingleThreadExecutor()
    private var runner: JobRunner? = null
    private var scanner: Scanner? = null
    private var loaded = false

    val model = ThermoModel(
        jobs = { job -> runner?.submit(job) ?: false },
        io = { io.execute(it) },
        main = { mainHandler.post(it) },
    )

    /** Where the CSV history lives: app storage on shared storage, reachable over USB. */
    val dataDir: File = File(app.getExternalFilesDir(null) ?: app.filesDir, "thermo")

    private val prefs = app.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Whether recording goes on with the app closed, through [MonitorService]. */
    var recordInBackground: Boolean
        get() = prefs.getBoolean(PREF_BACKGROUND, true)
        set(value) {
            prefs.edit().putBoolean(PREF_BACKGROUND, value).apply()
            if (value) MonitorService.start(app) else MonitorService.stop(app)
        }

    val adapter: BluetoothAdapter? get() = app.getSystemService(BluetoothManager::class.java)?.adapter

    /** Whether scanning and connecting are allowed. */
    fun hasPermissions(): Boolean = requiredPermissions().all {
        ContextCompat.checkSelfPermission(app, it) == PackageManager.PERMISSION_GRANTED
    }

    val running: Boolean get() = scanner != null

    private val bluetoothState = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, -1)) {
                BluetoothAdapter.STATE_ON -> scanner?.restart()
                BluetoothAdapter.STATE_OFF -> model.onScanFailed("Bluetooth is off")
            }
        }
    }

    init {
        ContextCompat.registerReceiver(
            app, bluetoothState, IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED), ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        loadHistory()
        scope.launch {
            var ticks = 0
            while (true) {
                delay(1_000)
                model.onTick()
                refitCalibration()
                if (ticks++ % WIDGET_INTERVAL_S == 0) ThermoWidget.update(app)
            }
        }
    }

    /** Reads the last two days of history and the logs once, off the main thread. */
    private fun loadHistory() {
        io.execute {
            val store = Store(dataDir)
            val now = System.currentTimeMillis()
            val history = runCatching { store.load(now - HISTORY_SPAN_MS, now) }
            val calibrations = runCatching { store.lastCalibrations() }.getOrDefault(emptyMap())
            val downloads = runCatching { store.lastDownloads() }.getOrDefault(emptyMap())
            val clocks = runCatching { store.lastClockOffsets() }.getOrDefault(emptyMap())
            mainHandler.post {
                history.onSuccess { model.useStore(store, it, calibrations, downloads, clocks) }
                loaded = true
                startIfReady()
            }
        }
    }

    /** Starts scanning and the job worker once history is loaded and permissions granted. */
    fun startIfReady() {
        if (scanner != null || !loaded || !hasPermissions()) return
        val adapter = adapter ?: return model.onScanFailed("no Bluetooth adapter")
        runner = JobRunner(app, adapter, scope, model::onJobEvent)
        scanner = Scanner(adapter, model::onAdvert, model::onScanStarted, model::onScanFailed).also { it.start() }
        if (recordInBackground) MonitorService.start(app)
    }

    /** Stops scanning, e.g. from the notification; queued jobs still run. */
    fun stopScanning() {
        scanner?.stop()
        scanner = null
        model.onScanFailed("stopped")
    }

    /** Fits the calibration screen's window off the main thread when it is due. */
    private fun refitCalibration(force: Boolean = false) {
        val input = model.calibrationInput(force) ?: return
        val (names, readings) = calibrationReadings(model.devices, input.devices, input.since, input.until)
        scope.launch {
            val result = withContext(Dispatchers.Default) {
                runCatching { CalibrationRun(names, input.since, input.until, calibrate(readings, Options())) }
            }
            model.onCalibrationResult(input, result)
        }
    }

    /** Refits right away, after the window or the devices changed. */
    fun refitNow() = refitCalibration(force = true)

    /** Merges history files picked by the user into the store, then into the model. */
    fun importFiles(uris: List<Uri>, done: (String) -> Unit) {
        val store = model.store ?: return done("History is still loading")
        io.execute {
            var files = 0
            var ignored = 0
            val measurements = mutableListOf<Measurement>()
            for (uri in uris) {
                val name = displayName(uri)
                val added = runCatching {
                    val text = app.contentResolver.openInputStream(uri)!!.use { it.readBytes().decodeToString() }
                    name?.let { store.import(it, text) }
                }.getOrNull()
                if (added == null) {
                    ignored++
                } else {
                    files++
                    measurements += added
                }
            }
            val calibrations = runCatching { store.lastCalibrations() }.getOrDefault(emptyMap())
            val downloads = runCatching { store.lastDownloads() }.getOrDefault(emptyMap())
            mainHandler.post {
                model.importHistory(measurements, calibrations, downloads)
                done(
                    "Imported $files file(s), ${measurements.size} new readings" +
                        if (ignored > 0) "; $ignored not history files" else ""
                )
            }
        }
    }

    private fun displayName(uri: Uri): String? =
        app.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
            if (it.moveToFirst()) it.getString(0) else null
        }

    companion object {
        fun requiredPermissions(): List<String> = buildList {
            if (Build.VERSION.SDK_INT >= 31) {
                add(Manifest.permission.BLUETOOTH_SCAN)
                add(Manifest.permission.BLUETOOTH_CONNECT)
            } else {
                add(Manifest.permission.ACCESS_FINE_LOCATION)
            }
        }
    }
}
