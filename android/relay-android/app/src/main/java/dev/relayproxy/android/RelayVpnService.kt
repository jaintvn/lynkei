package dev.relayproxy.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor

/**
 * Captures the traffic of selected apps with a local VPN and hands every TCP connection to the
 * Rust engine. Only the chosen apps are routed into the VPN — never Relay itself, so the engine's
 * own upstream sockets cannot loop back.
 */
class RelayVpnService : VpnService() {

    private val main = Handler(Looper.getMainLooper())
    private var targetsLabel = ""
    private val refresh = object : Runnable {
        override fun run() {
            notificationManager().notify(NOTIFICATION_ID, buildNotification())
            main.postDelayed(this, 2_000)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopCapture()
            main.removeCallbacks(refresh)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        val targets = intent?.getStringArrayExtra(EXTRA_TARGETS).orEmpty().filter { it.isNotBlank() }
        // Foreground first (within 5 s of start): keeps the VPN alive under memory pressure and
        // gives the user a visible, stoppable capture.
        targetsLabel = targets.joinToString()
        goForeground()
        try {
            startCapture(targets)
        } catch (e: Exception) {
            RelayCore.log("VPN failed to start: ${e.message}")
            stopSelf()
        }
        return START_STICKY
    }

    private fun startCapture(targets: List<String>) {
        stopCapture()
        val engine = RelayCore.engine(this)
        val builder = Builder()
            .setSession("Relay")
            .setMtu(MTU)
            .addAddress("10.111.0.2", 32)
            .addRoute("0.0.0.0", 0)
            .addAddress("fd00:7265:6c61::2", 128)
            .addRoute("::", 0)
            .addDnsServer("8.8.8.8")

        var added = 0
        for (pkg in targets) {
            try {
                builder.addAllowedApplication(pkg)
                added++
            } catch (e: PackageManager.NameNotFoundException) {
                RelayCore.log("not installed: $pkg")
            }
        }
        // With no allowed apps Android would route EVERY app — including Relay — into the VPN.
        check(added > 0) { "choose at least one installed app to capture" }

        val pfd: ParcelFileDescriptor = builder.establish() ?: error("VPN permission not granted")
        engine.startVpn(pfd.detachFd(), MTU.toUShort(), true)
        RelayCore.completed.set(0)
        RelayCore.errors.set(0)
        RelayCore.log("capturing ${targets.joinToString()} · QUIC blocked so apps fall back to TCP")
        main.removeCallbacks(refresh)
        main.post(refresh)
    }

    private fun notificationManager() = getSystemService(NotificationManager::class.java)

    private fun goForeground() {
        val channel = NotificationChannel(CHANNEL_ID, "Capture", NotificationManager.IMPORTANCE_LOW).apply {
            description = "Shown while Relay is capturing traffic"
            setShowBadge(false)
        }
        notificationManager().createNotificationChannel(channel)
        val n = buildNotification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SYSTEM_EXEMPTED)
        } else {
            startForeground(NOTIFICATION_ID, n)
        }
    }

    private fun buildNotification(): Notification {
        val stats = RelayCore.currentEngine()?.vpnStats()
        val done = RelayCore.completed.get()
        val errs = RelayCore.errors.get()
        val text = buildString {
            append("$done requests")
            if (errs > 0) append(" · $errs errors")
            if (stats != null) append(" · ${stats.tcpConnections} connections")
        }
        val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), flags)
        val stop = PendingIntent.getService(this, 1, Intent(this, RelayVpnService::class.java).setAction(ACTION_STOP), flags)
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle(if (targetsLabel.isEmpty()) "Relay is capturing" else "Capturing $targetsLabel")
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .build()
    }

    private fun stopCapture() {
        RelayCore.currentEngine()?.let {
            it.stopVpn()
            RelayCore.log("capture stopped")
        }
    }

    override fun onRevoke() {
        stopCapture()
        super.onRevoke()
    }

    override fun onDestroy() {
        main.removeCallbacks(refresh)
        stopCapture()
        super.onDestroy()
    }

    companion object {
        const val ACTION_STOP = "dev.relayproxy.android.STOP"
        const val EXTRA_TARGETS = "targets"
        const val MTU = 1500
        private const val CHANNEL_ID = "capture"
        private const val NOTIFICATION_ID = 1
    }
}
