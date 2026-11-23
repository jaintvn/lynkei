package dev.acme.shop

import android.app.Activity
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/**
 * "Acme Shop" — a tiny stand-in for a real app. It makes a realistic mix of API calls so you can
 * watch Relay track them: list, detail, add to cart (JSON POST), a failing checkout (500),
 * an expired token (401), a slow call and an image. Nothing here knows about Relay.
 *
 * `adb shell am start -n dev.acme.shop/.MainActivity --ez autorun true` runs every call once.
 */
class MainActivity : Activity() {

    private class Call(val label: String, val method: String, val url: String, val body: String? = null)

    private val calls = listOf(
        Call("Browse shoes", "GET", "https://httpbin.org/get?category=shoes&page=2"),
        Call("Product list", "GET", "https://httpbin.org/json"),
        Call("Add to cart", "POST", "https://httpbin.org/post", """{"sku":"p_8231","qty":1,"price":129.0}"""),
        Call("Checkout (fails)", "POST", "https://httpbin.org/status/500", """{"cart":{"items":3,"total":248.9}}"""),
        Call("Refresh token (401)", "POST", "https://httpbin.org/status/401", """{"refresh":"expired"}"""),
        Call("Recommendations (slow)", "GET", "https://httpbin.org/delay/2"),
        Call("Product image", "GET", "https://httpbin.org/image/png"),
        Call("Tip of the day", "GET", "https://api.github.com/zen"),
    )

    private lateinit var log: TextView
    private lateinit var scroll: ScrollView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildUi())
        if (intent.getBooleanExtra("autorun", false)) runAll()
    }

    private fun runAll() {
        thread {
            calls.forEach { run(it); Thread.sleep(400) }
            line("— done —")
        }
    }

    private fun run(c: Call) {
        val t0 = System.currentTimeMillis()
        try {
            val conn = URL(c.url).openConnection() as HttpURLConnection
            conn.requestMethod = c.method
            conn.connectTimeout = 15_000
            conn.readTimeout = 30_000
            conn.setRequestProperty("User-Agent", "AcmeShop/3.4.1 (Android)")
            conn.setRequestProperty("X-App-Version", "3.4.1")
            conn.setRequestProperty("Accept", "application/json")
            if (c.body != null) {
                conn.doOutput = true
                conn.setRequestProperty("Content-Type", "application/json")
                conn.outputStream.use { it.write(c.body.toByteArray()) }
            }
            val code = conn.responseCode
            val size = (if (code in 200..299) conn.inputStream else conn.errorStream)?.use { it.readBytes().size } ?: 0
            line("${if (code in 200..299) "✓" else "✗"} $code  ${c.label}  ·  $size B  ·  ${System.currentTimeMillis() - t0} ms")
        } catch (e: Exception) {
            line("✗ ERR  ${c.label}  ·  ${e.javaClass.simpleName}: ${e.message}")
        }
    }

    private fun line(text: String) = runOnUiThread {
        log.append(text + "\n")
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }

    private fun buildUi(): LinearLayout {
        val dp = resources.displayMetrics.density
        val bg = Color.parseColor("#FFF8F3")
        val ink = Color.parseColor("#2A1B14")
        val brand = Color.parseColor("#FF5277")
        fun rounded(color: Int) = GradientDrawable().apply { setColor(color); cornerRadius = 14 * dp }

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(bg)
            setPadding((20 * dp).toInt(), (44 * dp).toInt(), (20 * dp).toInt(), (16 * dp).toInt())
        }
        root.addView(TextView(this).apply {
            text = "Acme Shop"
            textSize = 28f
            setTypeface(typeface, Typeface.BOLD)
            setTextColor(brand)
        })
        root.addView(TextView(this).apply { text = "v3.4.1 · demo app for Relay"; setTextColor(ink); alpha = .6f })

        val grid = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        calls.chunked(2).forEach { pair ->
            val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            pair.forEach { c ->
                row.addView(Button(this).apply {
                    text = c.label
                    isAllCaps = false
                    textSize = 12f
                    setTextColor(Color.WHITE)
                    background = rounded(brand)
                    setOnClickListener { thread { run(c) } }
                }, LinearLayout.LayoutParams(0, (44 * dp).toInt(), 1f).apply { setMargins(0, (6 * dp).toInt(), (6 * dp).toInt(), 0) })
            }
            grid.addView(row)
        }
        root.addView(grid, LinearLayout.LayoutParams(-1, -2).apply { topMargin = (14 * dp).toInt() })
        root.addView(Button(this).apply {
            text = "Run every call"
            isAllCaps = false
            setTextColor(Color.WHITE)
            background = rounded(ink)
            setOnClickListener { runAll() }
        }, LinearLayout.LayoutParams(-1, (48 * dp).toInt()).apply { topMargin = (10 * dp).toInt() })

        log = TextView(this).apply { setTextColor(ink); typeface = Typeface.MONOSPACE; textSize = 11f }
        scroll = ScrollView(this).apply {
            background = rounded(Color.parseColor("#FFFFFF"))
            setPadding((12 * dp).toInt(), (12 * dp).toInt(), (12 * dp).toInt(), (12 * dp).toInt())
            addView(log)
        }
        root.addView(scroll, LinearLayout.LayoutParams(-1, 0, 1f).apply { topMargin = (12 * dp).toInt() })
        return root
    }
}
