/*
 * Release builds have no demo data; see the debug variant.
 */
package dev.artur.thermo

import dev.artur.thermo.model.ThermoModel

object DebugHooks {
    fun onCreate(activity: android.app.Activity) = Unit

    fun loadDemo(model: ThermoModel) = Unit
}
