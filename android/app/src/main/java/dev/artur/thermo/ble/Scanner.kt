/*
 * Background BLE scan that forwards BTHome adverts; the Android counterpart of
 * `crates/thermo/src/scanner.rs`.
 */
package dev.artur.thermo.ble

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import dev.artur.thermo.model.Advert

/** BTHome's 16-bit UUID `0xFCD2` expanded with the Bluetooth base UUID. */
val BTHOME_UUID: ParcelUuid = ParcelUuid.fromString("0000fcd2-0000-1000-8000-00805f9b34fb")

/**
 * Android allows five scan starts per 30 s per app and then silently returns nothing,
 * so restarts are spaced at least this far apart.
 */
private const val MIN_RESTART_INTERVAL_MS = 8_000L
/** Without a single advert for this long, the scan is assumed stuck and restarted. */
private const val SILENCE_RESTART_MS = 90_000L
/** How often the watchdog looks. */
private const val WATCHDOG_INTERVAL_MS = 15_000L

/**
 * Scans for BTHome service data while [start]ed. Calls back on the main thread.
 *
 * The filter on the service data UUID keeps the scan running with the screen off, and
 * exempts it from Android downgrading long scans to opportunistic ones.
 */
@SuppressLint("MissingPermission")
class Scanner(
    private val adapter: BluetoothAdapter,
    private val onAdvert: (Advert) -> Unit,
    private val onStarted: () -> Unit,
    private val onFailed: (String) -> Unit,
) {
    private val main = Handler(Looper.getMainLooper())
    /** Names seen in scan responses; adverts without one reuse the last. */
    private val names = HashMap<String, String>()
    private var running = false
    private var lastAdvert = 0L
    private var lastStart = 0L

    private val callback = object : ScanCallback() {
        override fun onScanResult(callbackType: Int, result: ScanResult) {
            handle(result)
        }

        override fun onBatchScanResults(results: MutableList<ScanResult>) {
            results.forEach(::handle)
        }

        override fun onScanFailed(errorCode: Int) {
            main.post { onFailed(scanError(errorCode)) }
        }
    }

    private fun handle(result: ScanResult) {
        val record = result.scanRecord ?: return
        val payload = record.getServiceData(BTHOME_UUID) ?: return
        val address = result.device.address
        record.deviceName?.takeIf { it.isNotBlank() }?.let { names[address] = it }
        val name = names[address] ?: address
        val advert = Advert(address, name, result.rssi, payload, System.currentTimeMillis())
        main.post {
            lastAdvert = advert.received
            onAdvert(advert)
        }
    }

    private val watchdog = object : Runnable {
        override fun run() {
            if (!running) return
            val now = System.currentTimeMillis()
            if (now - lastAdvert > SILENCE_RESTART_MS && now - lastStart > SILENCE_RESTART_MS) restart()
            main.postDelayed(this, WATCHDOG_INTERVAL_MS)
        }
    }

    fun start() {
        if (running) return
        running = true
        startScan()
        main.postDelayed(watchdog, WATCHDOG_INTERVAL_MS)
    }

    fun stop() {
        if (!running) return
        running = false
        main.removeCallbacks(watchdog)
        runCatching { adapter.bluetoothLeScanner?.stopScan(callback) }
    }

    /** Stops and starts the scan, e.g. after Bluetooth was switched back on. */
    fun restart() {
        if (!running) return
        runCatching { adapter.bluetoothLeScanner?.stopScan(callback) }
        val wait = (lastStart + MIN_RESTART_INTERVAL_MS - System.currentTimeMillis()).coerceAtLeast(500)
        main.postDelayed({ if (running) startScan() }, wait)
    }

    private fun startScan() {
        val scanner = adapter.bluetoothLeScanner
        if (!adapter.isEnabled || scanner == null) {
            onFailed("Bluetooth is off")
            return
        }
        val filter = ScanFilter.Builder().setServiceData(BTHOME_UUID, ByteArray(0), ByteArray(0)).build()
        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .setCallbackType(ScanSettings.CALLBACK_TYPE_ALL_MATCHES)
            .setReportDelay(0)
            .build()
        lastStart = System.currentTimeMillis()
        try {
            scanner.startScan(listOf(filter), settings, callback)
            onStarted()
        } catch (e: Exception) {
            onFailed("scan failed: ${e.message}")
        }
    }

    private fun scanError(code: Int) = when (code) {
        ScanCallback.SCAN_FAILED_ALREADY_STARTED -> "scan already started"
        ScanCallback.SCAN_FAILED_APPLICATION_REGISTRATION_FAILED -> "scan registration failed; toggle Bluetooth"
        ScanCallback.SCAN_FAILED_FEATURE_UNSUPPORTED -> "BLE scanning unsupported"
        ScanCallback.SCAN_FAILED_INTERNAL_ERROR -> "scan failed (internal error)"
        else -> "scan failed (code $code)"
    }
}
