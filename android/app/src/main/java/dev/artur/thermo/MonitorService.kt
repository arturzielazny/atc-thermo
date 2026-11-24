/*
 * Foreground service that keeps the process, and with it the scanner, alive while the app
 * is closed, so history keeps growing for charts and calibration like the desktop app's.
 * [BootReceiver] starts it after a reboot or an update; it is switched off only on the
 * Data screen, as a Stop button on the notification was too easily tapped by accident.
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
/** Channel and id of the notification that a calibration is ready, which alerts unlike the ongoing one. */
private const val CALIBRATION_CHANNEL = "calibration"
private const val CALIBRATION_NOTIFICATION_ID = 2
/** How often the notification's text follows the readings. */
private const val UPDATE_INTERVAL_MS = 30_000L

class MonitorService : Service() {
    private val handler = Handler(Looper.getMainLooper())
    private val update = object : Runnable {
        override fun run() {
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(this@MonitorService))
            handler.postDelayed(this, UPDATE_INTERVAL_MS)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        createChannel()
        val type = if (Build.VERSION.SDK_INT >= 29) ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE else 0
        try {
            ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(this), type)
        } catch (e: Exception) {
            // Not allowed right now (started from the background without a battery
            // optimisation exemption); the app still records while it is open.
            stopSelf()
            return START_NOT_STICKY
        }
        running = true
        controller.startIfReady()
        handler.removeCallbacks(update)
        handler.postDelayed(update, UPDATE_INTERVAL_MS)
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
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

    companion object {
        /** Whether the service is in the foreground, so that its notification may be redrawn. */
        @Volatile private var running = false

        /** Redraws the ongoing notification now, e.g. when a calibration starts or ends. */
        fun refresh(context: Context) {
            if (running) runCatching { context.getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(context)) }
        }

        private fun notification(context: Context): Notification {
            val model = context.controller.model
            val open = PendingIntent.getActivity(
                context, 0, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
            )
            val now = System.currentTimeMillis()
            val heard = model.devices.all().count { now - it.lastSeen < 5 * 60_000 }
            val range = model.devices.all().mapNotNull { it.temperature }.let { temps ->
                if (temps.isEmpty()) "" else " · %.1f–%.1f °C".format(temps.min(), temps.max())
            }
            val calibrating = model.calibrationStatus()
            return NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_thermometer)
                .setContentTitle(if (calibrating != null) "Calibrating thermometers" else "Recording thermometers")
                .setContentText(calibrating?.substringAfter(" · ") ?: "$heard of ${model.devices.size} heard recently$range")
                .setContentIntent(open)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setSilent(true)
                .build()
        }

        fun start(context: Context) {
            if (!context.controller.hasPermissions()) return
            runCatching {
                ContextCompat.startForegroundService(context, Intent(context, MonitorService::class.java))
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, MonitorService::class.java))
        }

        fun clearAnnouncement(context: Context) {
            context.getSystemService(NotificationManager::class.java).cancel(CALIBRATION_NOTIFICATION_ID)
        }

        /** Says that a calibration reached its target, with a sound, also with the app closed. */
        fun announce(context: Context, text: String) {
            val manager = context.getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(
                NotificationChannel(CALIBRATION_CHANNEL, "Calibration ready", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "When a calibration reaches the precision chosen"
                },
            )
            val open = PendingIntent.getActivity(context, 2, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
            val notification = NotificationCompat.Builder(context, CALIBRATION_CHANNEL)
                .setSmallIcon(R.drawable.ic_thermometer)
                .setContentTitle("Calibration ready")
                .setContentText(text)
                .setStyle(NotificationCompat.BigTextStyle().bigText(text))
                .setContentIntent(open)
                .setAutoCancel(true)
                .build()
            runCatching { manager.notify(CALIBRATION_NOTIFICATION_ID, notification) }
        }
    }
}
