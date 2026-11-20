package dev.relayproxy.android

import android.content.Intent
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.ParcelFileDescriptor

/**
 * Captures the traffic of selected apps with a local VPN and hands every TCP connection to the
 * Rust engine. Only the chosen apps are routed into the VPN — never Relay itself, so the engine's
 * own upstream sockets cannot loop back.
 */
class RelayVpnService : VpnService() {

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopCapture()
            stopSelf()
            return START_NOT_STICKY
        }
        val targets = intent?.getStringArrayExtra(EXTRA_TARGETS).orEmpty().filter { it.isNotBlank() }
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
        RelayCore.log("capturing ${targets.joinToString()} · QUIC blocked so apps fall back to TCP")
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
        stopCapture()
        super.onDestroy()
    }

    companion object {
        const val ACTION_STOP = "dev.relayproxy.android.STOP"
        const val EXTRA_TARGETS = "targets"
        const val MTU = 1500
    }
}
