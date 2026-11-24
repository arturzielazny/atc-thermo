/*
 * Debug builds only: `adb shell am start -n dev.artur.thermo/.MainActivity --ez demo true`
 * adds five simulated thermometers with 14 h of history, to look at screens that need
 * hours of data: 12 h side by side inside, then moved 2 h ago to a place 13 °C colder and
 * 20 %RH more humid. One of them has a slow humidity sensor, an hour behind the air like
 * one on 2026-10-29, so it is still settling. `--ez demo_session true` also starts a
 * calibration of them over all of it, moved 2 h ago; end it from its menu. Nothing of the
 * history is saved.
 * Debug builds also show over the lock screen, so they can be tested over adb while
 * the phone stays locked.
 */
package dev.artur.thermo

import dev.artur.thermo.model.Measurement
import dev.artur.thermo.model.Sample
import dev.artur.thermo.model.Source
import dev.artur.thermo.model.ThermoModel
import java.util.Random
import kotlin.math.PI
import kotlin.math.sin

object DebugHooks {
    fun onCreate(activity: android.app.Activity) {
        if (android.os.Build.VERSION.SDK_INT >= 27) {
            activity.setShowWhenLocked(true)
            activity.setTurnScreenOn(true)
        }
    }

    private const val HOUR = 3_600_000L

    /**
     * `(temperature gain, offset, humidity gain, offset, temperature and humidity time
     * constants in s)` per device.
     */
    private val SENSORS = listOf(
        doubleArrayOf(1.000, 0.20, 1.00, 2.0, 300.0, 360.0),
        doubleArrayOf(1.015, -0.25, 0.96, -1.5, 400.0, 480.0),
        doubleArrayOf(0.990, 0.05, 1.04, 0.5, 540.0, 650.0),
        doubleArrayOf(1.005, -0.10, 1.02, -3.0, 360.0, 3600.0),
        doubleArrayOf(0.995, 0.70, 0.98, 1.0, 480.0, 580.0),
    )

    fun loadDemo(model: ThermoModel, session: Boolean) {
        val now = System.currentTimeMillis()
        val start = now - 14 * HOUR
        val moved = now - 2 * HOUR
        val random = Random(7)
        SENSORS.forEachIndexed { i, s ->
            val address = "DE:00:00:00:00:%02X".format(i + 1)
            var tCase = 21.0
            var hCase = 55.0
            var t = start
            while (t < now) {
                val elapsed = (t - start).toDouble()
                val outside = t >= moved
                val ta = if (outside) 8.0 + 0.3 * sin(2 * PI * elapsed / (5.0 * HOUR)) else 21.0 + 0.4 * sin(2 * PI * elapsed / (3.0 * HOUR))
                val ha = if (outside) 75.0 else 55.0 + 2.0 * sin(2 * PI * elapsed / (4.0 * HOUR))
                tCase += (ta - tCase) * 10.0 / s[4]
                hCase += (ha - hCase) * 10.0 / s[5]
                // Lossy reception: about half the measurements arrive.
                if (random.nextDouble() < 0.5) {
                    val sample = Sample(
                        at = t,
                        temperature = (s[0] * tCase + s[1] + 0.02 * random.nextGaussian()).toFloat(),
                        humidity = (s[2] * hCase + s[3] + 0.15 * random.nextGaussian()).toFloat(),
                        rssi = -60 - 6 * i + random.nextInt(7) - 3,
                        source = Source.Live,
                    )
                    model.devices.restore(Measurement(address, "Demo ${i + 1}", sample, 90 - 7 * i, 2.9f))
                }
                t += 10_000
            }
        }
        model.onTick()
        if (session) {
            model.startCalibration(SENSORS.indices.map { "DE:00:00:00:00:%02X".format(it + 1) }, start = start)
            model.markMoved(moved)
        }
    }
}
