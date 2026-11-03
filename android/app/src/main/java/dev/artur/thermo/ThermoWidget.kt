/*
 * Home-screen widget with each thermometer's latest temperature and humidity. It is
 * redrawn while the app records, so it is as current as the recording.
 */
package dev.artur.thermo

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import androidx.compose.ui.graphics.toArgb
import dev.artur.thermo.model.STALE_AFTER_MS
import dev.artur.thermo.ui.deviceColor
import dev.artur.thermo.ui.formatAgo
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** Rows beyond this don't fit a widget of usual size; the app shows the rest. */
private const val MAX_ROWS = 8

class ThermoWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        update(context)
    }

    companion object {
        /** Redraws every placed widget from the model; cheap when none is placed. */
        fun update(context: Context) {
            val manager = AppWidgetManager.getInstance(context)
            val ids = manager.getAppWidgetIds(ComponentName(context, ThermoWidget::class.java))
            if (ids.isEmpty()) return
            val model = context.controller.model
            val now = System.currentTimeMillis()
            val views = RemoteViews(context.packageName, R.layout.widget)
            views.removeAllViews(R.id.widget_rows)
            model.devices.all().take(MAX_ROWS).forEachIndexed { i, device ->
                val row = RemoteViews(context.packageName, R.layout.widget_row)
                val stale = now - device.lastSeen > STALE_AFTER_MS
                row.setTextColor(R.id.row_dot, deviceColor(i).toArgb())
                row.setTextViewText(R.id.row_name, device.name + if (stale) " · ${formatAgo(now - device.lastSeen)}" else "")
                row.setTextViewText(R.id.row_temperature, device.temperature?.let { "%.1f °C".format(it) } ?: "–")
                row.setTextViewText(R.id.row_humidity, device.humidity?.let { "%.0f %%".format(it) } ?: "")
                views.addView(R.id.widget_rows, row)
            }
            val time = SimpleDateFormat("HH:mm", Locale.getDefault()).format(Date(now))
            views.setTextViewText(
                R.id.widget_title,
                if (model.devices.size == 0) "Thermometers · open the app to start" else "Thermometers · $time",
            )
            val open = PendingIntent.getActivity(
                context, 0, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
            )
            views.setOnClickPendingIntent(R.id.widget_root, open)
            manager.updateAppWidget(ids, views)
        }
    }
}
