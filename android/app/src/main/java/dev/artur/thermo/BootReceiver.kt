/*
 * Starts recording again after a reboot or an app update, when recording all the time is
 * on; both broadcasts may start a foreground service from the background.
 */
package dev.artur.thermo

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED ->
                if (context.controller.recordInBackground) MonitorService.start(context)
        }
    }
}
