package dev.relayproxy.android

import android.content.Context
import android.util.Log
import java.io.File
import uniffi.relay_ffi.FfiState
import uniffi.relay_ffi.FfiTransaction
import uniffi.relay_ffi.RelayEngine
import uniffi.relay_ffi.TrafficListener

/** Process-wide holder for the Rust engine and a small in-memory log for the prototype UI. */
object RelayCore {
    const val TAG = "RelayTraffic"
    private const val MAX_LINES = 300

    private var engine: RelayEngine? = null
    var proxyPort: Int = 0
        private set
    var caPath: String = ""
        private set

    private val lines = ArrayDeque<String>()
    @Volatile var onUpdate: (() -> Unit)? = null

    @Synchronized
    fun engine(context: Context): RelayEngine {
        engine?.let { return it }
        val dir = File(context.filesDir, "relay").apply { mkdirs() }
        val e = RelayEngine(dir.path)
        // The proxy listener is internal (loopback only); the VPN feeds connections in directly.
        proxyPort = e.start("127.0.0.1", 0u, Listener).toInt()
        caPath = e.caCertificatePath()
        engine = e
        log("engine started · CA ${caPath}")
        return e
    }

    fun currentEngine(): RelayEngine? = engine

    fun snapshot(): List<String> = synchronized(lines) { lines.toList() }

    fun log(line: String) {
        Log.i(TAG, line)
        synchronized(lines) {
            lines.addLast(line)
            while (lines.size > MAX_LINES) lines.removeFirst()
        }
        onUpdate?.invoke()
    }

    private object Listener : TrafficListener {
        override fun onTransaction(transaction: FfiTransaction) = log(format(transaction))
    }

    private fun format(t: FfiTransaction): String {
        val status = when (t.state) {
            FfiState.TUNNELED -> "TUNNEL"
            FfiState.FAILED -> "FAILED"
            FfiState.COMPLETE -> t.status?.toString() ?: "-"
        }
        val lock = if (t.decrypted) "decrypted" else if (t.scheme == "https") "encrypted" else "plain"
        val err = t.error?.let { " — $it" } ?: ""
        return "$status ${t.method} ${t.url} · ${t.durationMs}ms · ${t.responseBodySize}B · $lock$err"
    }
}
