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

    /** Running cost of the current call in micro-USD; null until the first priced response. */
    var costMicro: Long? = null
        private set

    /** Why the call ended when it was our decision rather than the user's (e.g. "interrupted"). */
    var endReason: String? = null

    val active get() = state == CONNECTING || state == LIVE || state == RECONNECTING

    /** How long the call's cost stays on screen after the call ends. */
    private const val COST_LINGER_MS = 10_000L
    private val hideCost = Runnable {
        if (!active && costMicro != null) {
            costMicro = null
            notifyChanged()
        }
    }

    override fun onState(state: Int, detail: String?) {
        main.post { set(state, detail) }
    }

    override fun onCost(microUsd: Long) {
        main.post {
            costMicro = microUsd
            notifyChanged()
        }
    }

    fun begin() {
        main.removeCallbacks(hideCost)
        costMicro = null
        // `muted` is deliberately kept: muting before the call starts must hold from the first frame.
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
        val wasActive = active
        this.state = state
        this.detail = detail
        // The call just ended: keep its cost visible for a moment, then let it go.
        if (wasActive && !active && costMicro != null) main.postDelayed(hideCost, COST_LINGER_MS)
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
