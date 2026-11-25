/*
 * Says a calibration's progress aloud ([dev.artur.thermo.model.ThermoModel.takeSpeech])
 * through the phone's text-to-speech: in English like the rest of the app, at the media
 * volume like a navigation app, lowering other audio meanwhile. Quiet during a call and
 * while Do Not Disturb is on.
 */
package dev.artur.thermo

import android.app.NotificationManager
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.util.Log
import java.util.Locale

private const val TAG = "Speaker"

class Speaker(context: Context) {
    private val context = context.applicationContext
    private val audio = this.context.getSystemService(AudioManager::class.java)
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()
    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK).setAudioAttributes(attributes).build()

    /** The engine, bound on the first [say] and released by [shutdown]. */
    private var tts: TextToSpeech? = null
    private var ready = false
    /** What to say once the engine is ready: only the latest. */
    private var waiting: String? = null
    private var utterances = 0

    /** Says [text] after whatever is being said, unless the phone should be quiet now. */
    fun say(text: String) {
        if (quiet()) {
            Log.i(TAG, "quiet now, not saying: $text")
            return
        }
        val engine = tts ?: TextToSpeech(context, ::onInit).also { tts = it }
        if (ready) speak(engine, text) else waiting = text
    }

    /** Stops what is being said. */
    fun stop() {
        tts?.stop()
        audio.abandonAudioFocusRequest(focus)
    }

    /** Stops and releases the engine; the next [say] binds it again. */
    fun shutdown() {
        tts?.shutdown()
        tts = null
        ready = false
        waiting = null
        audio.abandonAudioFocusRequest(focus)
    }

    /** On the main thread, once the engine is bound. */
    private fun onInit(status: Int) {
        val engine = tts ?: return
        if (status != TextToSpeech.SUCCESS) {
            Log.w(TAG, "text-to-speech unavailable ($status)")
            shutdown()
            return
        }
        // The app's texts are English: the phone's own English if it has one, else American.
        val english = Locale.getDefault().takeIf { it.language == "en" } ?: Locale.US
        val language = engine.setLanguage(english)
        if (language < TextToSpeech.LANG_AVAILABLE) Log.w(TAG, "no $english voice ($language), using the default")
        engine.setAudioAttributes(attributes)
        engine.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
            override fun onStart(utteranceId: String?) = Unit
            override fun onDone(utteranceId: String?) {
                audio.abandonAudioFocusRequest(focus)
            }
            @Deprecated("Deprecated in Java")
            override fun onError(utteranceId: String?) {
                audio.abandonAudioFocusRequest(focus)
            }
        })
        ready = true
        waiting?.let { speak(engine, it) }
        waiting = null
    }

    private fun speak(engine: TextToSpeech, text: String) {
        Log.i(TAG, "saying: $text")
        // Without focus (Android 15 refuses it to an app neither in front nor with a foreground service), say it anyway.
        audio.requestAudioFocus(focus)
        engine.speak(text, TextToSpeech.QUEUE_ADD, null, "calibration-${utterances++}")
    }

    /** During a call, or with Do Not Disturb on. */
    private fun quiet(): Boolean {
        val filter = context.getSystemService(NotificationManager::class.java).currentInterruptionFilter
        val doNotDisturb = filter != NotificationManager.INTERRUPTION_FILTER_ALL && filter != NotificationManager.INTERRUPTION_FILTER_UNKNOWN
        val call = audio.mode == AudioManager.MODE_IN_CALL || audio.mode == AudioManager.MODE_IN_COMMUNICATION || audio.mode == AudioManager.MODE_RINGTONE
        return doNotDisturb || call
    }
}
