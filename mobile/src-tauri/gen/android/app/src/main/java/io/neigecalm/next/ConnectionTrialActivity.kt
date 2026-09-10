package io.neigecalm.next

import android.content.Intent
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.appcompat.app.AlertDialog
import androidx.webkit.ProxyConfig
import androidx.webkit.ProxyController
import androidx.webkit.WebViewFeature
import org.json.JSONObject
import java.io.File
import java.net.URI
import java.util.concurrent.Executors

/** Phone-test harness only. Deliberately does not use Android VpnService. */
class ConnectionTrialActivity : AppCompatActivity() {
  private val worker = Executors.newSingleThreadExecutor()
  private val handler = Handler(Looper.getMainLooper())
  private lateinit var connection: TextView
  private lateinit var measurement: TextView
  private lateinit var authorize: Button
  private lateinit var test: Button
  private lateinit var enter: Button
  private var authURL = ""
  private var busy = false
  private var resumed = false
  private var requestingLogin = false
  private val poll = object : Runnable {
    override fun run() {
      if (!resumed || busy || requestingLogin) { if (resumed) handler.postDelayed(this, 1000); return }
      busy = true
      worker.execute {
        val status = runCatching { JSONObject(NativeP2P.status()) }
        runOnUiThread {
          busy = false
          status.onSuccess { render(it) }.onFailure { connection.text = it.message ?: "读取连接状态失败" }
          if (resumed) handler.postDelayed(this, 1500)
        }
      }
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val layout = LinearLayout(this).apply {
      orientation = LinearLayout.VERTICAL
      val inset = (24 * resources.displayMetrics.density).toInt()
      setPadding(inset, inset * 2, inset, inset)
    }
    fun label(text: String, size: Float = 16f): TextView = TextView(this).also {
      it.text = text; it.textSize = size; it.setPadding(0, 12, 0, 16); layout.addView(it)
    }
    fun button(text: String, action: () -> Unit): Button = Button(this).also {
      it.text = text; it.setOnClickListener { action() }; layout.addView(it)
    }
    label("Neige 直连试验", 26f)
    label("保留你原来的 VPN。本试验只连接你的工作区，不接管手机网络。")
    connection = label("正在启动内置连接…")
    authorize = button("登录 Tailscale / 重新获取授权") { requestLogin() }
    label("首次授权请选择电脑所在的同一个 Tailscale 账户。完成后返回这里，不需要打开 Tailscale App。")
    test = button("测试接口耗时 / 直连状态") { measure() }.apply { isEnabled = false }
    measurement = label("连接后可测试三次：第一次包含建连耗时，后两次观察稳定延迟。")
    enter = button("进入工作区试用") { openWorkspace() }.apply { isEnabled = false }
    button("退出试验") { finishAffinity() }
    label("试验版 · 手机实测后再做正式 review\n如连接仍走中转，可在现有 VPN 中尝试将本试验 App 设为绕过。")
    setContentView(ScrollView(this).apply { addView(layout) })
    busy = true
    worker.execute {
      val result = runCatching {
        val configured = JSONObject(NativeP2P.configure(AndroidNetworkSnapshot.read()))
        check(configured.optBoolean("ok")) { configured.optString("error") }
        JSONObject(NativeP2P.start(File(noBackupFilesDir, "p2p-node").absolutePath))
      }
      runOnUiThread {
        busy = false
        result.onSuccess { if (!it.optBoolean("ok")) showFailure("连接启动失败", it.optString("error")) }
          .onFailure { showFailure("连接启动失败", it.message ?: "未知错误") }
      }
    }
  }

  private fun showFailure(title: String, message: String) {
    connection.text = "$title：$message"
    measurement.text = message
    if (!isDestroyed && !isFinishing) AlertDialog.Builder(this).setTitle(title)
      .setMessage(message).setPositiveButton("知道了", null).show()
  }

  private fun requestLogin() {
    if (requestingLogin) return
    requestingLogin = true
    authorize.isEnabled = false
    measurement.text = "正在获取登录链接，最多约 20 秒…"
    worker.execute {
      val result = runCatching { JSONObject(NativeP2P.login()) }
      runOnUiThread {
        requestingLogin = false
        authorize.isEnabled = true
        if (isDestroyed) return@runOnUiThread
        result.onSuccess { value ->
          if (!value.optBoolean("ok")) {
            showFailure("登录失败", value.optString("error"))
          } else {
            render(value)
            val uri = runCatching { URI(authURL) }.getOrNull()
            if (value.optString("state") == "Running") {
              measurement.text = "已登录，可以点击测试接口耗时。"
            } else if (uri?.scheme == "https" && uri.host == "login.tailscale.com" && uri.rawUserInfo == null) {
              measurement.text = "请在浏览器中完成登录，然后返回这里。"
              if (resumed) runCatching { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(authURL))) }
                .onFailure { measurement.text = "无法打开浏览器：${it.message}" }
            } else { measurement.text = "未获得有效登录链接，请重试。" }
          }
        }.onFailure { showFailure("登录失败", it.message ?: "未知错误") }
      }
    }
  }

  private fun render(value: JSONObject) {
    if (!value.optBoolean("ok")) { connection.text = value.optString("error"); return }
    val state = value.optString("state")
    authURL = value.optString("authURL")
    authorize.text = if (state == "Running") "已登录 · 刷新连接状态" else "登录 Tailscale / 重新获取授权"
    val ready = state == "Running"
    test.isEnabled = ready
    enter.isEnabled = ready
    val manager = getSystemService(ConnectivityManager::class.java)
    val vpn = manager.getNetworkCapabilities(manager.activeNetwork)?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true
    val networkText = if (vpn) "当前网络仍由你的 VPN 提供" else "当前网络未报告 VPN"
    val description = when(state) {
      "Running" -> "已加入网络 · 点击测试确认直连或中转"
      "NeedsLogin" -> "等待首次账户授权"
      "NeedsMachineAuth" -> "等待 Tailscale 管理端批准新设备"
      else -> "连接中：$state"
    }
    val health = value.optJSONArray("health")
    val detail = if (health != null && health.length() > 0) "\n诊断：${health.optString(0)}" else ""
    connection.text = "$description\n$networkText\n本 App 未申请系统 VPN 权限$detail"
  }

  private fun pathLabel(path: String) = when(path) {
    "direct" -> "直连"
    "relay" -> "中转（未直连）"
    else -> "尚未确认路径，点击测试"
  }

  private fun measure() {
    if (busy) return
    busy = true; test.isEnabled = false; enter.isEnabled = false
    measurement.text = "正在请求真实 /api/version…"
    worker.execute {
      val result = runCatching { JSONObject(NativeP2P.probe()) }
      runOnUiThread {
        busy = false
        result.onSuccess {
          measurement.text = if (it.optBoolean("ok")) {
            "${pathLabel(it.optString("path"))}\n接口耗时：${it.optLong("requestMs")} ms\n返回：${it.optInt("bytes")} 字节 · 兼容版本 ${it.optInt("webCompatVersion")}\n再点一次可比较连接复用后的耗时。"
          } else "请求失败：${it.optString("error")}"
        }.onFailure { measurement.text = "测试失败：${it.message}" }
      }
    }
  }

  private fun openWorkspace() {
    if (!WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)) {
      measurement.text = "请更新 Android System WebView 后再试。"; return
    }
    val proxy = NativeP2P.proxy()
    if (!proxy.startsWith("http://127.0.0.1:")) { measurement.text = "内置连接未准备好"; return }
    enter.isEnabled = false
    // Applied only to this app's WebViews; the external authorization browser
    // and other apps retain their own network/VPN configuration.
    val config = ProxyConfig.Builder().addProxyRule(proxy)
      .addBypassRule("tauri.localhost").build()
    ProxyController.getInstance().setProxyOverride(config, java.util.concurrent.Executor { runOnUiThread(it) }) { finish() }
  }

  override fun onResume() { super.onResume(); resumed = true; handler.post(poll) }
  override fun onPause() { resumed = false; handler.removeCallbacks(poll); super.onPause() }
  override fun onDestroy() { handler.removeCallbacks(poll); worker.shutdown(); super.onDestroy() }
}
