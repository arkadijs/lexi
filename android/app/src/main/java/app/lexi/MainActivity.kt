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
import android.graphics.drawable.GradientDrawable
import android.view.Menu
import android.widget.FrameLayout
import android.widget.PopupMenu
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
    private lateinit var topBar: LinearLayout
    private lateinit var providerPill: TextView
    private lateinit var voicePill: TextView
    private lateinit var modelPill: TextView
    private lateinit var costRow: LinearLayout
    private lateinit var durView: TextView
    private lateinit var dotView: TextView
    private lateinit var costView: TextView
    private lateinit var controls: LinearLayout
    private lateinit var callBtn: IconButton
    private lateinit var muteBtn: IconButton
    private lateinit var routeBtn: IconButton
    private lateinit var modeBtn: IconButton
    private lateinit var muteLbl: TextView
    private lateinit var routeLbl: TextView
    private lateinit var modeLbl: TextView

    private var visualMode = NativeBridge.MODE_FLUID
    private var notice: Int? = null // transient status line message (resource id)
    private var micBlocked = false // permission permanently denied -> send the user to Settings
    private val refresh = Runnable { render() }
    private val tick = android.os.Handler(android.os.Looper.getMainLooper())

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.isNavigationBarContrastEnforced = false
        // Measurement aid: `adb shell am start -n app.lexi/.MainActivity --es visuals off` gives a baseline with no renderer.
        if (intent.getStringExtra("visuals") == "off") {
            visualMode = NativeBridge.MODE_OFF
            NativeBridge.setVisualMode(NativeBridge.MODE_OFF)
        }

        val root = FrameLayout(this)
        surface = SurfaceView(this).also { it.holder.addCallback(this) }
        root.addView(surface, ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)

        fun pill(onClick: () -> Unit) = TextView(this).apply {
            setTextColor(0xFFFFFFFF.toInt()); textSize = 14f; gravity = Gravity.CENTER
            setPadding(dp(16), dp(8), dp(16), dp(8))
            background = GradientDrawable().apply { cornerRadius = dp(20).toFloat(); setColor(0x88202A38.toInt()) }
            minHeight = dp(40)
            isClickable = true; isFocusable = true
            setOnClickListener { onClick() }
        }
        providerPill = pill { showProviderMenu() }
        voicePill = pill { showVoiceMenu() }
        modelPill = pill { showModelMenu() }
        // The three selectors side by side (provider, voice, model): the same width, the same gaps.
        val pills = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER
            val lp = { LinearLayout.LayoutParams(dp(104), ViewGroup.LayoutParams.WRAP_CONTENT) }
            addView(providerPill, lp().apply { marginEnd = dp(4) })
            addView(voicePill, lp().apply { marginStart = dp(4); marginEnd = dp(4) })
            addView(modelPill, lp().apply { marginStart = dp(4) })
        }
        status = TextView(this).apply {
            setTextColor(0xFFFFFFFF.toInt()); textSize = 16f; gravity = Gravity.CENTER
            setShadowLayer(6f, 0f, 0f, 0xFF000000.toInt())
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }
        // Duration | dot | cost: the two texts have equal weight on either side of a fixed dot, so the dot stays on the
        // centre line and nothing shifts when the digits change width.
        fun costText(g: Int) = TextView(this).apply {
            setTextColor(0xCCFFFFFF.toInt()); textSize = 14f; gravity = g
            setShadowLayer(6f, 0f, 0f, 0xFF000000.toInt())
        }
        durView = costText(Gravity.END)
        dotView = costText(Gravity.CENTER).apply { text = "·"; setPadding(dp(8), 0, dp(8), 0) }
        costView = costText(Gravity.START)
        costRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            visibility = View.INVISIBLE // keeps the layout still before the call
            addView(durView, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(dotView, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            addView(costView, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        }
        // Top bar: the voice and model selectors, with the status line and the call cost under it (the bottom is busy).
        topBar = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER_HORIZONTAL }
        topBar.addView(pills, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        topBar.addView(status, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(8) })
        topBar.addView(costRow, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(2) })
        root.addView(topBar, FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP
        ))

        muteBtn = IconButton(this).also { it.setOnClickListener { toggleMute() } }
        callBtn = IconButton(this).also { it.setOnClickListener { onCallTapped() } }
        routeBtn = IconButton(this).also { it.setOnClickListener { AudioRouter.cycle() } }
        modeBtn = IconButton(this).also { it.setOnClickListener { toggleMode() } }

        controls = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_HORIZONTAL or Gravity.TOP }
        // A button with a caption underneath; the side buttons are smaller, so they get a top margin
        // that keeps all four circle centres on one line.
        fun column(b: IconButton, size: Int, labelled: Boolean = true): TextView? {
            val label = if (!labelled) null else TextView(this).apply {
                setTextColor(0xFFFFFFFF.toInt()); textSize = 12f; gravity = Gravity.CENTER
                setShadowLayer(4f, 0f, 0f, 0xFF000000.toInt())
                importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO // the button has the description
            }
            val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER_HORIZONTAL }
            col.addView(b, LinearLayout.LayoutParams(dp(size), dp(size)))
            if (label != null) {
                col.addView(label, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(4) })
            }
            controls.addView(col, LinearLayout.LayoutParams(dp(84), ViewGroup.LayoutParams.WRAP_CONTENT).apply {
                topMargin = dp((80 - size) / 2)
            })
            return label
        }
        // The call button needs no caption: its colour and icon say it.
        muteLbl = column(muteBtn, 60)!!; column(callBtn, 80, labelled = false)
        routeLbl = column(routeBtn, 60)!!; modeLbl = column(modeBtn, 60)!!
        root.addView(controls, FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM
        ))

        // Edge-to-edge: keep the controls and status line clear of system bars and cutouts.
        root.setOnApplyWindowInsetsListener { _, insets ->
            val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            controls.setPadding(i.left, 0, i.right, i.bottom + dp(24))
            topBar.setPadding(i.left + dp(16), i.top + dp(12), i.right + dp(16), 0)
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
        tick.removeCallbacks(refresh)
        super.onStop()
    }

    // Surface lifecycle -> native renderer (it must stop drawing when the surface is gone).
    override fun surfaceCreated(holder: SurfaceHolder) = NativeBridge.surfaceCreated(holder.surface)
    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) = NativeBridge.surfaceChanged(w, h)
    override fun surfaceDestroyed(holder: SurfaceHolder) = NativeBridge.surfaceDestroyed()

    private fun showProviderMenu() = showMenu(providerPill, Providers.all.map { it.label }, Providers.current(this).label) { label ->
        Providers.all.firstOrNull { it.label == label }?.let { Providers.set(this, it) }
    }

    private fun showVoiceMenu() {
        val voices = Voices.all(Providers.current(this))
        showMenu(voicePill, voices.map { it.label }, Voices.current(this).label) { label ->
            voices.firstOrNull { it.label == label }?.let { Voices.set(this, it) }
        }
    }

    private fun showModelMenu() = showMenu(modelPill, Models.all.map { it.label }, Models.current(this).label) { label ->
        Models.all.firstOrNull { it.label == label }?.let { Models.set(this, it) }
    }

    private fun showMenu(anchor: View, labels: List<String>, current: String, onPick: (String) -> Unit) {
        if (CallModel.active) return // locked during a call
        val menu = PopupMenu(this, anchor)
        labels.forEachIndexed { i, l -> menu.menu.add(1, i, i, l) }
        menu.menu.setGroupCheckable(1, true, true)
        // Only the match may be touched: in an exclusive group even `isChecked = false` selects the item.
        for (n in 0 until menu.menu.size()) {
            val item = menu.menu.getItem(n)
            if (item.title == current) item.isChecked = true
        }
        menu.setOnMenuItemClickListener { item ->
            onPick(item.title.toString())
            render()
            true
        }
        menu.show()
    }

    /** US dollars rounded UP to the cent ($0.0011 shows as $0.01). An estimate priced from OpenAI's usage reports. */
    private fun formatCost(micro: Long): String =
        String.format(java.util.Locale.US, "$%.2f", (micro + 9_999) / 10_000 / 100.0)

    private fun formatDuration(sec: Long): String =
        if (sec >= 3600) String.format(java.util.Locale.US, "%d:%02d:%02d", sec / 3600, sec / 60 % 60, sec % 60)
        else String.format(java.util.Locale.US, "%d:%02d", sec / 60, sec % 60)

    private fun toggleMode() {
        visualMode = if (visualMode == NativeBridge.MODE_FLUID) NativeBridge.MODE_FRACTAL else NativeBridge.MODE_FLUID
        NativeBridge.setVisualMode(visualMode)
        render()
    }

    // Works before, during and after a call: muting first means the call starts muted.
    private fun toggleMute() = CallModel.setMuted(!CallModel.muted)

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
        muteLbl.setText(if (CallModel.muted) R.string.lb_muted else R.string.lb_mute)

        val (routeKind, routeCd, routeLb) = when (AudioRouter.current) {
            AudioRouter.Route.EARPIECE -> Triple(IconButton.Kind.EARPIECE, R.string.cd_route_earpiece, R.string.lb_earpiece)
            AudioRouter.Route.SPEAKER -> Triple(IconButton.Kind.SPEAKER, R.string.cd_route_speaker, R.string.lb_speaker)
            AudioRouter.Route.WIRED -> Triple(IconButton.Kind.WIRED, R.string.cd_route_wired, R.string.lb_wired)
            AudioRouter.Route.BLUETOOTH -> Triple(IconButton.Kind.BLUETOOTH, R.string.cd_route_bluetooth, R.string.lb_bluetooth)
        }
        routeBtn.kind = routeKind
        routeBtn.contentDescription = getString(routeCd)
        routeLbl.setText(routeLb)

        val fluid = visualMode == NativeBridge.MODE_FLUID
        modeBtn.kind = if (fluid) IconButton.Kind.FLUID else IconButton.Kind.FRACTAL
        modeBtn.contentDescription = getString(if (fluid) R.string.cd_mode_fluid else R.string.cd_mode_fractal)
        modeLbl.setText(if (fluid) R.string.lb_fluid else R.string.lb_fractal)

        val voice = Voices.current(this)
        voicePill.text = "${voice.label}  ▾"
        voicePill.contentDescription = getString(R.string.cd_voice, voice.label)
        val provider = Providers.current(this)
        providerPill.text = "${provider.label}  ▾"
        providerPill.contentDescription = getString(R.string.cd_provider, provider.label)
        val model = Models.current(this)
        val google = provider.id == "google"
        // Gemini has one Live model: the model pill shows it and cannot be changed.
        modelPill.text = if (google) "Live" else "${model.label}  ▾"
        modelPill.contentDescription = if (google) getString(R.string.cd_model_fixed) else getString(R.string.cd_model, model.label)
        // The choices are per call: locked (dimmed, not clickable) while a call runs.
        for (p in listOf(providerPill, voicePill, modelPill)) { p.isEnabled = !active; p.alpha = if (active) 0.4f else 1f }
        if (google && !active) { modelPill.isEnabled = false; modelPill.alpha = 0.4f }

        val cost = CallModel.costMicro
        val secs = CallModel.durationSec
        costRow.visibility = if (cost == null && secs == null) View.INVISIBLE else View.VISIBLE
        durView.text = secs?.let { formatDuration(it) } ?: ""
        costView.text = cost?.let { formatCost(it) } ?: ""
        dotView.visibility = if (cost != null && secs != null) View.VISIBLE else View.INVISIBLE
        durView.contentDescription = secs?.let { getString(R.string.cd_duration, formatDuration(it)) }
        costView.contentDescription = cost?.let { getString(R.string.cd_cost, formatCost(it)) }
        // Tick once a second while a call runs and the screen is on.
        tick.removeCallbacks(refresh)
        if (CallModel.active && secs != null) tick.postDelayed(refresh, 1000)

        status.text = notice?.let { getString(it) } ?: when (CallModel.state) {
            CallModel.CONNECTING -> getString(R.string.st_connecting)
            CallModel.RECONNECTING -> getString(R.string.st_reconnecting)
            CallModel.LIVE -> getString(R.string.st_live)
            CallModel.ENDED -> when {
                CallModel.endReason == "interrupted" -> getString(R.string.st_interrupted)
                CallModel.detail == "remote" -> getString(R.string.st_remote)
                CallModel.detail == "max-session" -> getString(R.string.st_max_session)
                else -> ""
            }
            CallModel.ERROR -> getString(when (CallModel.detail) {
                "network" -> R.string.st_no_network
                "auth" -> R.string.st_auth
                "clock" -> R.string.st_clock
                "provider" -> R.string.st_provider
                "rate" -> R.string.st_rate
                "service" -> R.string.st_service
                "audio" -> R.string.st_audio
                else -> R.string.st_error
            })
            else -> ""
        }
    }
}
