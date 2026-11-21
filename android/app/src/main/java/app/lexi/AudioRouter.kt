package app.lexi

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Handler
import android.os.Looper

/** SPEC §6.3: communication mode, audio focus, and communication-device routing. Main thread only. */
object AudioRouter {
    enum class Route { EARPIECE, SPEAKER, WIRED, BLUETOOTH }

    // Default priority: Bluetooth > wired > speaker (hands-free coach) > earpiece.
    private val priority = listOf(Route.BLUETOOTH, Route.WIRED, Route.SPEAKER, Route.EARPIECE)
    // Order the Route button cycles through.
    private val cycleOrder = listOf(Route.EARPIECE, Route.SPEAKER, Route.WIRED, Route.BLUETOOTH)

    private lateinit var app: Context
    private lateinit var am: AudioManager
    private val main = Handler(Looper.getMainLooper())
    private var focus: AudioFocusRequest? = null
    private var running = false
    private var userChoice: Route? = null

    var current: Route = Route.SPEAKER
        private set

    fun init(context: Context) {
        app = context.applicationContext
        am = app.getSystemService(AudioManager::class.java)
        userChoice = app.getSharedPreferences("lexi", Context.MODE_PRIVATE)
            .getString("route", null)?.let { name -> Route.entries.firstOrNull { it.name == name } }
    }

    private fun routeOf(d: AudioDeviceInfo): Route? = when (d.type) {
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> Route.EARPIECE
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> Route.SPEAKER
        AudioDeviceInfo.TYPE_WIRED_HEADSET, AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
        AudioDeviceInfo.TYPE_USB_HEADSET -> Route.WIRED
        AudioDeviceInfo.TYPE_BLUETOOTH_SCO, AudioDeviceInfo.TYPE_BLE_HEADSET,
        AudioDeviceInfo.TYPE_HEARING_AID -> Route.BLUETOOTH
        else -> null
    }

    fun available(): Map<Route, AudioDeviceInfo> {
        val out = LinkedHashMap<Route, AudioDeviceInfo>()
        for (d in am.availableCommunicationDevices) routeOf(d)?.let { out.putIfAbsent(it, d) }
        return out
    }

    private val deviceCallback = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>) {
            // Auto-switch to a newly connected headset.
            val headset = added.mapNotNull { routeOf(it) }.firstOrNull { it == Route.BLUETOOTH || it == Route.WIRED }
            if (headset != null && available().containsKey(headset)) select(headset) else CallModel.notifyChanged()
        }

        override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>) {
            if (!available().containsKey(current)) applyDefault() else CallModel.notifyChanged()
        }
    }

    private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
        // Permanent loss (incoming phone call): end cleanly. Transient loss is left to the OS.
        if (change == AudioManager.AUDIOFOCUS_LOSS) main.post { CallModel.interrupt() }
    }

    fun start() {
        if (running) return
        running = true
        am.mode = AudioManager.MODE_IN_COMMUNICATION
        val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build()
            )
            .setOnAudioFocusChangeListener(focusListener, main)
            .build()
        focus = req
        am.requestAudioFocus(req)
        am.registerAudioDeviceCallback(deviceCallback, main)
        applyDefault()
    }

    fun stop() {
        if (!running) return
        running = false
        am.unregisterAudioDeviceCallback(deviceCallback)
        am.clearCommunicationDevice()
        am.mode = AudioManager.MODE_NORMAL
        focus?.let { am.abandonAudioFocusRequest(it) }
        focus = null
    }

    private fun applyDefault() {
        val avail = available()
        val target = userChoice?.takeIf { avail.containsKey(it) } ?: priority.firstOrNull { avail.containsKey(it) }
        if (target != null) select(target)
    }

    /** Route button: cycle through the outputs that are currently available. */
    fun cycle() {
        val avail = cycleOrder.filter { available().containsKey(it) }
        if (avail.size < 2) return
        val next = avail[(avail.indexOf(current) + 1) % avail.size]
        userChoice = next
        app.getSharedPreferences("lexi", Context.MODE_PRIVATE).edit().putString("route", next.name).apply()
        select(next)
    }

    private fun select(route: Route) {
        val device = available()[route] ?: return
        if (am.setCommunicationDevice(device)) {
            current = route
            NativeBridge.onAudioRouteChanged()
        }
        CallModel.notifyChanged()
    }
}
