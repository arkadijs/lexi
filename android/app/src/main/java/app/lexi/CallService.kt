package app.lexi

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.net.wifi.WifiManager
import android.os.IBinder
import android.os.PowerManager

/** Foreground service (type microphone) that keeps the call alive with the screen off. */
class CallService : Service() {
    companion object {
        const val ACTION_START = "app.lexi.START"
        const val ACTION_HANGUP = "app.lexi.HANGUP"
        const val ACTION_TOGGLE_MUTE = "app.lexi.TOGGLE_MUTE"
        private const val CHANNEL = "call"
        private const val NOTIFICATION_ID = 1
        private const val LOCK_TIMEOUT_MS = 40L * 60 * 1000 // safety net beyond the 30 min session guard
    }

    private var started = false
    private var sawActive = false
    private var wake: PowerManager.WakeLock? = null
    private var wifi: WifiManager.WifiLock? = null
    private var network: NetworkWatcher? = null
    private val listener = Runnable { onModelChanged() }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> if (!started) startCall()
            ACTION_HANGUP -> hangUp()
            ACTION_TOGGLE_MUTE -> if (started) CallModel.setMuted(!CallModel.muted)
            else -> if (!started) stopSelf()
        }
        return START_NOT_STICKY
    }

    private fun startCall() {
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.notif_channel), NotificationManager.IMPORTANCE_LOW)
        )
        try {
            startForeground(NOTIFICATION_ID, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        } catch (e: RuntimeException) {
            // Not allowed (background start, or missing microphone permission): fail visibly, never crash (N5).
            CallModel.fail(e.javaClass.simpleName)
            stopSelf()
            return
        }
        started = true
        sawActive = false

        wake = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "lexi:call").apply {
                setReferenceCounted(false)
                acquire(LOCK_TIMEOUT_MS)
            }
        wifi = getSystemService(WifiManager::class.java)
            .createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "lexi:call").apply {
                setReferenceCounted(false)
                acquire()
            }
        AudioRouter.start()
        network = NetworkWatcher(this).also { it.start() }

        CallModel.addListener(listener)
        CallModel.begin()
        NativeBridge.setMuted(CallModel.muted) // honour "muted before the call"
        NativeBridge.startCall(
            BuildConfig.MINTER_URL, InstallId.hash(this), BuildConfig.ATTEST_KEY, Voices.current(this).id,
            Models.current(this).id
        )
    }

    private fun hangUp() {
        if (started && CallModel.active) NativeBridge.endCall() else cleanup()
    }

    private fun onModelChanged() {
        if (!started) return
        if (CallModel.active) sawActive = true
        val terminal = CallModel.state == CallModel.ENDED || CallModel.state == CallModel.ERROR ||
            (CallModel.state == CallModel.IDLE && sawActive)
        if (terminal) cleanup() else getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification())
    }

    private fun cleanup() {
        if (started) {
            started = false
            CallModel.removeListener(listener)
            network?.stop()
            network = null
            AudioRouter.stop()
            wake?.takeIf { it.isHeld }?.release()
            wifi?.takeIf { it.isHeld }?.release()
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        if (started) {
            NativeBridge.endCall()
            cleanup()
        }
        super.onDestroy()
    }

    private fun action(act: String, title: Int, request: Int): Notification.Action {
        val pi = PendingIntent.getService(
            this, request, Intent(this, CallService::class.java).setAction(act), PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Action.Builder(Icon.createWithResource(this, R.drawable.ic_stat), getString(title), pi).build()
    }

    private fun notification(): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE
        )
        val text = when (CallModel.state) {
            CallModel.CONNECTING -> R.string.st_connecting
            CallModel.RECONNECTING -> R.string.st_reconnecting
            else -> R.string.st_live
        }
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat)
            .setContentTitle(getString(R.string.notif_title))
            .setContentText(getString(text))
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_CALL)
            .setContentIntent(open)
            .setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
            .addAction(action(ACTION_TOGGLE_MUTE, if (CallModel.muted) R.string.notif_unmute else R.string.notif_mute, 1))
            .addAction(action(ACTION_HANGUP, R.string.notif_hangup, 2))
            .build()
    }
}
