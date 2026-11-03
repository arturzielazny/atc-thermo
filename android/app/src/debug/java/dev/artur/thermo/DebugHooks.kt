/*
 * Debug builds only: `adb shell am start -n dev.artur.thermo/.MainActivity --ez demo true`
 * adds five simulated thermometers with a 14 h calibration session (inside, outside,
 * inside), to look at screens that need hours of data. Nothing of it is saved.
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

    /** `(temperature gain, offset, humidity gain, offset, case time constant in s)` per device. */
    private val SENSORS = listOf(
        doubleArrayOf(1.000, 0.20, 1.00, 2.0, 300.0),
        doubleArrayOf(1.015, -0.25, 0.96, -1.5, 400.0),
        doubleArrayOf(0.990, 0.05, 1.04, 0.5, 540.0),
        doubleArrayOf(1.005, -0.10, 1.02, -3.0, 360.0),
        doubleArrayOf(0.995, 0.70, 0.98, 1.0, 480.0),
    )

    fun loadDemo(model: ThermoModel) {
        val now = System.currentTimeMillis()
        val start = now - 14 * HOUR
        val random = Random(7)
        SENSORS.forEachIndexed { i, s ->
            val address = "DE:00:00:00:00:%02X".format(i + 1)
            var tCase = 21.0
            var hCase = 55.0
            var t = start
            while (t < now) {
                val elapsed = (t - start).toDouble()
                val outside = elapsed >= 6.0 * HOUR && elapsed < 10.0 * HOUR
                val ta = if (outside) 8.0 else 21.0 + 0.4 * sin(2 * PI * elapsed / (3.0 * HOUR))
                val ha = if (outside) 85.0 else 55.0 + 2.0 * sin(2 * PI * elapsed / (4.0 * HOUR))
                val k = 10.0 / s[4]
                tCase += (ta - tCase) * k
                hCase += (ha - hCase) * k
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
    }
}
