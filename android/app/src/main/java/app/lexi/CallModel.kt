package app.lexi

import android.os.Handler
import android.os.Looper

/** Single source of UI truth for the call. Mutated and observed on the main thread only. */
object CallModel : NativeCallback {
    const val IDLE = 0
    const val CONNECTING = 1
    const val LIVE = 2
    const val RECONNECTING = 3
    const val ENDED = 4
    const val ERROR = 5

    private val main = Handler(Looper.getMainLooper())
    private val listeners = ArrayList<Runnable>()

    var state = IDLE
        private set
    var detail: String? = null
        private set
    var muted = false
        private set

    /** Why the call ended when it was our decision rather than the user's (e.g. "interrupted"). */
    var endReason: String? = null

    val active get() = state == CONNECTING || state == LIVE || state == RECONNECTING

    override fun onState(state: Int, detail: String?) {
        main.post { set(state, detail) }
    }

    fun begin() {
        muted = false
        endReason = null
        set(CONNECTING, null)
    }

    fun fail(detail: String?) = set(ERROR, detail)

    fun setMuted(value: Boolean) {
        muted = value
        NativeBridge.setMuted(value)
        notifyChanged()
    }

    /** Audio focus was taken away for good (e.g. a phone call). */
    fun interrupt() {
        if (!active) return
        endReason = "interrupted"
        NativeBridge.endCall()
    }

    private fun set(state: Int, detail: String?) {
        this.state = state
        this.detail = detail
        notifyChanged()
    }

    fun addListener(r: Runnable) {
        if (!listeners.contains(r)) listeners.add(r)
    }

    fun removeListener(r: Runnable) {
        listeners.remove(r)
    }

    fun notifyChanged() {
        for (l in ArrayList(listeners)) l.run()
    }
}
