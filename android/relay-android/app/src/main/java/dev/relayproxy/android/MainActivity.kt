package dev.relayproxy.android

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.VpnService
import android.os.Bundle
import android.view.Gravity
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * Prototype screen (the real Compose UI follows the Claude Design canvas in P7).
 * For automation: `am start -n dev.relayproxy.android/.MainActivity --es targets com.android.shell --ez autostart true`
 */
class MainActivity : Activity() {
    private lateinit var targets: EditText
    private lateinit var status: TextView
    private lateinit var logView: TextView
    private lateinit var scroll: ScrollView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildUi())
        intent.getStringExtra("targets")?.let { targets.setText(it) }
        if (intent.getBooleanExtra("autostart", false)) requestStart()
    }

    override fun onResume() {
        super.onResume()
        RelayCore.onUpdate = { runOnUiThread { refresh() } }
        refresh()
    }

    override fun onPause() {
        RelayCore.onUpdate = null
        super.onPause()
    }

    private fun requestStart() {
        // Android 13+: the capture notification needs permission to be shown.
        if (android.os.Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFY)
        }
        val consent = VpnService.prepare(this)
        if (consent != null) startActivityForResult(consent, REQ_VPN) else startVpn()
    }

    @Deprecated("Prototype uses the platform Activity API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_VPN && resultCode == RESULT_OK) startVpn()
    }

    private fun startVpn() {
        val list = targets.text.split(',', ' ', '\n').map { it.trim() }.filter { it.isNotEmpty() }
        startService(
            Intent(this, RelayVpnService::class.java).putExtra(RelayVpnService.EXTRA_TARGETS, list.toTypedArray())
        )
    }

    private fun stopVpn() {
        startService(Intent(this, RelayVpnService::class.java).setAction(RelayVpnService.ACTION_STOP))
    }

    private fun refresh() {
        val stats = RelayCore.currentEngine()?.vpnStats()
        status.text = if (stats == null) {
            "Not capturing"
        } else {
            "Capturing · ${stats.tcpConnections} TCP · ${stats.udpFlows} UDP · ${stats.quicBlocked} QUIC blocked"
        }
        logView.text = RelayCore.snapshot().takeLast(120).joinToString("\n")
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }

    // Graphite + Electric Indigo tokens from the chosen design.
    private fun buildUi(): LinearLayout {
        val bg = Color.parseColor("#0D0E11")
        val panel = Color.parseColor("#16181D")
        val fg = Color.parseColor("#E9EBF0")
        val muted = Color.parseColor("#A1A7B4")
        val accent = Color.parseColor("#7C8CFF")
        val dp = resources.displayMetrics.density

        fun rounded(color: Int) = GradientDrawable().apply { setColor(color); cornerRadius = 14 * dp }

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(bg)
            setPadding((20 * dp).toInt(), (40 * dp).toInt(), (20 * dp).toInt(), (16 * dp).toInt())
        }
        root.addView(TextView(this).apply {
            text = "Relay"
            textSize = 26f
            setTypeface(typeface, Typeface.BOLD)
            setTextColor(fg)
        })
        root.addView(TextView(this).apply {
            text = "On-device capture prototype · VPN → Rust engine"
            setTextColor(muted)
        })
        targets = EditText(this).apply {
            setText("com.android.shell, com.android.chrome")
            setTextColor(fg)
            setHintTextColor(muted)
            hint = "Package names to capture"
            background = rounded(panel)
            setPadding((14 * dp).toInt(), (12 * dp).toInt(), (14 * dp).toInt(), (12 * dp).toInt())
        }
        root.addView(targets, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = (16 * dp).toInt() })

        val buttons = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
        buttons.addView(Button(this).apply {
            text = "Start capture"
            setTextColor(bg)
            background = rounded(accent)
            setOnClickListener { requestStart() }
        }, LinearLayout.LayoutParams(0, (48 * dp).toInt(), 1f))
        buttons.addView(Button(this).apply {
            text = "Stop"
            setTextColor(fg)
            background = rounded(panel)
            setOnClickListener { stopVpn() }
        }, LinearLayout.LayoutParams(0, (48 * dp).toInt(), 1f).apply { marginStart = (10 * dp).toInt() })
        root.addView(buttons, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = (12 * dp).toInt() })

        status = TextView(this).apply { setTextColor(accent); setPadding(0, (12 * dp).toInt(), 0, (8 * dp).toInt()) }
        root.addView(status)

        logView = TextView(this).apply {
            setTextColor(fg)
            typeface = Typeface.MONOSPACE
            textSize = 11f
        }
        scroll = ScrollView(this).apply {
            background = rounded(panel)
            setPadding((12 * dp).toInt(), (12 * dp).toInt(), (12 * dp).toInt(), (12 * dp).toInt())
            addView(logView)
        }
        root.addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        return root
    }

    companion object {
        private const val REQ_VPN = 1
        private const val REQ_NOTIFY = 2
    }
}
