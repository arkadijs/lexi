package app.lexi

import android.content.Context
import android.view.Surface

/** Delivered from a native dispatcher thread; implementations must hop to the main thread. */
interface NativeCallback {
    /** state: 0 Idle, 1 Connecting, 2 Live, 3 Reconnecting, 4 Ended, 5 Error. */
    fun onState(state: Int, detail: String?)

    /** Running cost of the current call in micro-USD (USD x 1e-6); only sent for models with a known price. */
    fun onCost(microUsd: Long)
}

/** JNI surface, SPEC §4.2. Rust side: core/src/ffi. Names must match `Java_app_lexi_NativeBridge_*`. */
object NativeBridge {
    init {
        System.loadLibrary("lexi_core")
    }

    /** `touch` action code for a double tap (MotionEvent actions are passed through unchanged). */
    const val ACTION_DOUBLE_TAP = 1000
    const val MODE_FLUID = 0
    const val MODE_FRACTAL = 1

    @JvmStatic external fun init(context: Context, callback: NativeCallback)
    @JvmStatic external fun surfaceCreated(surface: Surface)
    @JvmStatic external fun surfaceChanged(width: Int, height: Int)
    @JvmStatic external fun surfaceDestroyed()
    @JvmStatic external fun setVisualMode(mode: Int)
    @JvmStatic external fun touch(action: Int, pointerId: Int, x: Float, y: Float)
    @JvmStatic external fun pinch(scale: Float, focusX: Float, focusY: Float)
    /**
     * [attestKey]: build-time shared key for the minter's `shared-key` attestation; empty = none.
     * [voice]: one of the minter's voices; empty = the minter's default. Fixed for the whole call.
     */
    @JvmStatic external fun startCall(minterUrl: String, installIdHash: String, attestKey: String, voice: String)
    @JvmStatic external fun endCall()
    @JvmStatic external fun setMuted(muted: Boolean)
    @JvmStatic external fun onAudioRouteChanged()
    @JvmStatic external fun onNetworkChanged()
}
