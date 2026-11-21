package app.lexi

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import android.view.Gravity
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.GestureDetector
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/** The only screen: full-screen SurfaceView (rendered by Rust) + four custom-drawn buttons + a status line. */
class MainActivity : Activity(), SurfaceHolder.Callback {
    private companion object {
        const val REQ_PERMISSIONS = 1
        val PERMISSIONS = arrayOf(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.POST_NOTIFICATIONS,
            Manifest.permission.BLUETOOTH_CONNECT, // only needed to list Bluetooth headsets
        )
    }

    private lateinit var surface: SurfaceView
    private lateinit var status: TextView
    private lateinit var controls: LinearLayout
    private lateinit var callBtn: IconButton
    private lateinit var muteBtn: IconButton
    private lateinit var routeBtn: IconButton
    private lateinit var modeBtn: IconButton

    private var visualMode = NativeBridge.MODE_FLUID
    private var notice: Int? = null // transient status line message (resource id)
    private var micBlocked = false // permission permanently denied -> send the user to Settings
    private val refresh = Runnable { render() }

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.isNavigationBarContrastEnforced = false

        val root = FrameLayout(this)
        surface = SurfaceView(this).also { it.holder.addCallback(this) }
        root.addView(surface, ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)

        status = TextView(this).apply {
            setTextColor(0xFFFFFFFF.toInt()); textSize = 16f; gravity = Gravity.CENTER
            setShadowLayer(6f, 0f, 0f, 0xFF000000.toInt())
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }
        root.addView(status, FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP
        ).apply { topMargin = dp(16) })

        muteBtn = IconButton(this).also { it.setOnClickListener { toggleMute() } }
        callBtn = IconButton(this).also { it.setOnClickListener { onCallTapped() } }
        routeBtn = IconButton(this).also { it.setOnClickListener { AudioRouter.cycle() } }
        modeBtn = IconButton(this).also { it.setOnClickListener { toggleMode() } }

        controls = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER }
        fun add(b: IconButton, size: Int) = controls.addView(b, LinearLayout.LayoutParams(dp(size), dp(size)).apply {
            marginStart = dp(10); marginEnd = dp(10)
        })
        add(muteBtn, 60); add(callBtn, 80); add(routeBtn, 60); add(modeBtn, 60)
        root.addView(controls, FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM
        ))

        // Edge-to-edge: keep the controls and status line clear of system bars and cutouts.
        root.setOnApplyWindowInsetsListener { _, insets ->
            val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            controls.setPadding(i.left, 0, i.right, i.bottom + dp(24))
            status.setPadding(i.left + dp(16), i.top, i.right + dp(16), 0)
            insets
        }
        setContentView(root)
        installGestures()
    }

    private fun installGestures() {
        val scale = ScaleGestureDetector(this, object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
            override fun onScale(d: ScaleGestureDetector): Boolean {
                NativeBridge.pinch(d.scaleFactor, d.focusX, d.focusY)
                return true
            }
        })
        val taps = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onDown(e: MotionEvent) = true
            override fun onDoubleTap(e: MotionEvent): Boolean {
                NativeBridge.touch(NativeBridge.ACTION_DOUBLE_TAP, 0, e.x, e.y)
                return true
            }
        })
        surface.setOnTouchListener { v, e ->
            scale.onTouchEvent(e)
            taps.onTouchEvent(e)
            when (e.actionMasked) {
                MotionEvent.ACTION_MOVE -> for (i in 0 until e.pointerCount)
                    NativeBridge.touch(MotionEvent.ACTION_MOVE, e.getPointerId(i), e.getX(i), e.getY(i))
                else -> e.actionIndex.let {
                    NativeBridge.touch(e.actionMasked, e.getPointerId(it), e.getX(it), e.getY(it))
                }
            }
            if (e.actionMasked == MotionEvent.ACTION_UP) v.performClick()
            true
        }
    }

    override fun onStart() {
        super.onStart()
        CallModel.addListener(refresh)
        render()
    }

    override fun onStop() {
        CallModel.removeListener(refresh)
        super.onStop()
    }

    // Surface lifecycle -> native renderer (it must stop drawing when the surface is gone).
    override fun surfaceCreated(holder: SurfaceHolder) = NativeBridge.surfaceCreated(holder.surface)
    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) = NativeBridge.surfaceChanged(w, h)
    override fun surfaceDestroyed(holder: SurfaceHolder) = NativeBridge.surfaceDestroyed()

    private fun toggleMode() {
        visualMode = if (visualMode == NativeBridge.MODE_FLUID) NativeBridge.MODE_FRACTAL else NativeBridge.MODE_FLUID
        NativeBridge.setVisualMode(visualMode)
        render()
    }

    private fun toggleMute() {
        if (CallModel.active) CallModel.setMuted(!CallModel.muted)
    }

    private fun onCallTapped() {
        if (CallModel.active) {
            startService(Intent(this, CallService::class.java).setAction(CallService.ACTION_HANGUP))
            return
        }
        if (micBlocked) {
            startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null)))
            return
        }
        val missing = PERMISSIONS.filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isEmpty()) beginCall() else requestPermissions(missing.toTypedArray(), REQ_PERMISSIONS)
    }

    override fun onRequestPermissionsResult(code: Int, permissions: Array<out String>, results: IntArray) {
        super.onRequestPermissionsResult(code, permissions, results)
        if (code != REQ_PERMISSIONS) return
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            micBlocked = false
            beginCall() // notification / Bluetooth permissions are optional
        } else {
            micBlocked = !shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO)
            notice = if (micBlocked) R.string.st_need_mic_settings else R.string.st_need_mic
            render()
        }
    }

    private fun beginCall() {
        val cm = getSystemService(ConnectivityManager::class.java)
        val online = cm.getNetworkCapabilities(cm.activeNetwork)?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true
        if (!online) {
            notice = R.string.st_no_network
            render()
            return
        }
        notice = null
        // Started from the visible Activity: Android 14+ forbids starting a microphone FGS from the background.
        startForegroundService(Intent(this, CallService::class.java).setAction(CallService.ACTION_START))
    }

    private fun render() {
        val active = CallModel.active
        val connecting = CallModel.state == CallModel.CONNECTING || CallModel.state == CallModel.RECONNECTING
        if (active) notice = null

        callBtn.kind = if (active) IconButton.Kind.HANGUP else IconButton.Kind.CALL
        callBtn.busy = connecting
        callBtn.contentDescription = getString(if (active) R.string.cd_hangup else R.string.cd_call)

        muteBtn.kind = if (CallModel.muted) IconButton.Kind.MIC_OFF else IconButton.Kind.MIC
        muteBtn.contentDescription = getString(if (CallModel.muted) R.string.cd_unmute else R.string.cd_mute)
        muteBtn.alpha = if (active) 1f else 0.4f

        val (routeKind, routeCd) = when (AudioRouter.current) {
            AudioRouter.Route.EARPIECE -> IconButton.Kind.EARPIECE to R.string.cd_route_earpiece
            AudioRouter.Route.SPEAKER -> IconButton.Kind.SPEAKER to R.string.cd_route_speaker
            AudioRouter.Route.WIRED -> IconButton.Kind.WIRED to R.string.cd_route_wired
            AudioRouter.Route.BLUETOOTH -> IconButton.Kind.BLUETOOTH to R.string.cd_route_bluetooth
        }
        routeBtn.kind = routeKind
        routeBtn.contentDescription = getString(routeCd)
        routeBtn.alpha = if (active) 1f else 0.4f

        val fluid = visualMode == NativeBridge.MODE_FLUID
        modeBtn.kind = if (fluid) IconButton.Kind.FLUID else IconButton.Kind.FRACTAL
        modeBtn.contentDescription = getString(if (fluid) R.string.cd_mode_fluid else R.string.cd_mode_fractal)

        status.text = notice?.let { getString(it) } ?: when (CallModel.state) {
            CallModel.CONNECTING -> getString(R.string.st_connecting)
            CallModel.RECONNECTING -> getString(R.string.st_reconnecting)
            CallModel.LIVE -> getString(R.string.st_live)
            CallModel.ENDED -> if (CallModel.endReason == "interrupted") getString(R.string.st_interrupted) else ""
            CallModel.ERROR -> getString(R.string.st_error)
            else -> ""
        }
    }
}
