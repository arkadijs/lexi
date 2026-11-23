package app.lexi

import android.os.Handler
import android.os.Looper
import android.os.SystemClock

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

    private var liveSinceMs = 0L
    private var endedAtMs = 0L

    /** Seconds the call has been live (connected at least once); frozen when it ends; null before it connects. */
    val durationSec: Long?
        get() {
            if (liveSinceMs == 0L) return null
            val end = if (active) SystemClock.elapsedRealtime() else endedAtMs
            return ((end - liveSinceMs) / 1000).coerceAtLeast(0)
        }

    /** Why the call ended when it was our decision rather than the user's (e.g. "interrupted"). */
    var endReason: String? = null

    val active get() = state == CONNECTING || state == LIVE || state == RECONNECTING

    /** How long the call's cost stays on screen after the call ends. */
    private const val COST_LINGER_MS = 10_000L
    private val hideCost = Runnable {
        if (!active && (costMicro != null || liveSinceMs != 0L)) {
            costMicro = null
            liveSinceMs = 0L
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
        liveSinceMs = 0L
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
        if (state == LIVE && liveSinceMs == 0L) liveSinceMs = SystemClock.elapsedRealtime()
        this.state = state
        this.detail = detail
        // The call just ended: keep its duration and cost visible for a moment, then let them go.
        if (wasActive && !active) endedAtMs = SystemClock.elapsedRealtime()
        if (wasActive && !active && (costMicro != null || liveSinceMs != 0L)) main.postDelayed(hideCost, COST_LINGER_MS)
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
