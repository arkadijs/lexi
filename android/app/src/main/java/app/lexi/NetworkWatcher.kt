package app.lexi

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network

/** Tells the core when the default network changes (Wi-Fi <-> cellular, loss and return). */
class NetworkWatcher(context: Context) {
    private val cm = context.getSystemService(ConnectivityManager::class.java)
    private var seenFirst = false
    private var last: Network? = null

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            val changed = seenFirst && network != last
            seenFirst = true
            last = network
            if (changed) NativeBridge.onNetworkChanged()
        }

        override fun onLost(network: Network) {
            if (network == last) last = null
        }
    }

    fun start() = cm.registerDefaultNetworkCallback(callback)

    fun stop() {
        try {
            cm.unregisterNetworkCallback(callback)
        } catch (_: IllegalArgumentException) {
            // not registered
        }
    }
}
