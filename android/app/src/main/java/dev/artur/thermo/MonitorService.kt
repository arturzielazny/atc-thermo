/*
 * Foreground service that keeps the process, and with it the scanner, alive while the app
 * is closed, so history keeps growing for charts and calibration like the desktop app's.
 */
package dev.artur.thermo

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

private const val CHANNEL = "recording"
private const val NOTIFICATION_ID = 1
private const val ACTION_STOP = "dev.artur.thermo.STOP"
/** How often the notification's text follows the readings. */
private const val UPDATE_INTERVAL_MS = 30_000L

class MonitorService : Service() {
    private val handler = Handler(Looper.getMainLooper())
    private val update = object : Runnable {
        override fun run() {
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification())
            handler.postDelayed(this, UPDATE_INTERVAL_MS)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            controller.recordInBackground = false
            if (!App.visible) controller.stopScanning()
            stopSelf()
            return START_NOT_STICKY
        }
        createChannel()
        val type = if (Build.VERSION.SDK_INT >= 29) ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE else 0
        try {
            ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(), type)
        } catch (e: Exception) {
            // Not allowed right now (e.g. started from the background); the app still
            // records while it is open.
            stopSelf()
            return START_NOT_STICKY
        }
        controller.startIfReady()
        handler.removeCallbacks(update)
        handler.postDelayed(update, UPDATE_INTERVAL_MS)
        return START_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacks(update)
        super.onDestroy()
    }

    private fun createChannel() {
        if (Build.VERSION.SDK_INT < 26) return
        val channel = NotificationChannel(CHANNEL, "Recording", NotificationManager.IMPORTANCE_LOW).apply {
            description = "Shown while thermometer readings are recorded in the background"
            setShowBadge(false)
        }
        getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    private fun notification(): Notification {
        val model = controller.model
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, MonitorService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        val now = System.currentTimeMillis()
        val heard = model.devices.all().count { now - it.lastSeen < 5 * 60_000 }
        val range = model.devices.all().mapNotNull { it.temperature }.let { temps ->
            if (temps.isEmpty()) "" else " · %.1f–%.1f °C".format(temps.min(), temps.max())
        }
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_thermometer)
            .setContentTitle("Recording thermometers")
            .setContentText("$heard of ${model.devices.size} heard recently$range")
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .addAction(0, "Stop", stop)
            .build()
    }

    companion object {
        fun start(context: Context) {
            if (!context.controller.hasPermissions()) return
            runCatching {
                ContextCompat.startForegroundService(context, Intent(context, MonitorService::class.java))
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, MonitorService::class.java))
        }
    }
}

/** Whether an activity of the app is on screen. */
object App {
    @Volatile var visible = false
}
