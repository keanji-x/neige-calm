package io.neigecalm.next

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.security.NetworkSecurityPolicy
import android.webkit.WebView
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import androidx.webkit.ProxyConfig
import androidx.webkit.ProxyController
import app.tauri.plugin.JSObject
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.net.URI
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicReference

@InvokeArg class BindFrontendArgs { lateinit var origin: String }
@InvokeArg class SaveConnectionArgs { lateinit var mode: String; lateinit var ipOrigin: String; var tailscaleEnabled: Boolean by kotlin.properties.Delegates.notNull() }

@TauriPlugin
class BundledFrontendPlugin(private val host: Activity) : Plugin(host) {
  private var view: WebView? = null
  private var client: BundledWebViewClient? = null
  private val selectedOrigin = AtomicReference<BundledOrigin?>(null)
  private val network = Executors.newFixedThreadPool(2)
  private val profiles by lazy { ConnectionProfiles(host.applicationContext) }
  @Volatile private var generation = 0
  private val deadlines = Executors.newSingleThreadScheduledExecutor()
  private class Pending(val invoke: Invoke) {
    val cancellation = ConnectionAttempt.Cancellation()
    val settled = java.util.concurrent.atomic.AtomicBoolean(false)
    var future: java.util.concurrent.Future<*>? = null
    var timeout: java.util.concurrent.Future<*>? = null
  }
  private var pending: Pending? = null
  private var resumeConsumed = false
  private var candidate: ConnectionRoute? = null
  private var checkedAt = 0L

  override fun load(webView: WebView) { view = webView }

  private fun cancelPending() {
    pending?.let {
      it.cancellation.cancel(); it.future?.cancel(true); it.timeout?.cancel(false)
      if (it.settled.compareAndSet(false, true)) it.invoke.reject("连接已取消")
    }
    pending = null
  }

  private fun startPending(invoke: Invoke): Pending {
    cancelPending()
    val job = Pending(invoke)
    pending = job
    job.timeout = deadlines.schedule({ host.runOnUiThread {
      if (job.settled.compareAndSet(false, true)) {
        generation++; job.cancellation.cancel(); job.future?.cancel(true)
        if (pending === job) pending = null
        invoke.reject("连接超时，请重新配置")
      }
    } }, 15, java.util.concurrent.TimeUnit.SECONDS)
    return job
  }

  private fun finish(job: Pending, action: () -> Unit) {
    if (!job.settled.compareAndSet(false, true)) return
    job.timeout?.cancel(false)
    if (pending === job) pending = null
    action()
  }

  private fun launcher(): WebView {
    check(!host.isDestroyed && !host.isFinishing) { "请重新打开 App" }
    val webView = checkNotNull(view) { "网页组件尚未准备好，请重试" }
    val current = URI(webView.url ?: "")
    check(current.host == "tauri.localhost" && current.rawUserInfo == null
      && ((current.scheme == "http" && current.port in listOf(-1, 80))
        || (current.scheme == "https" && current.port in listOf(-1, 443)))) { "只能从 App 连接页操作" }
    return webView
  }

  private fun settingsJson(settings: ConnectionSettings) = JSObject().also {
    it.put("mode", settings.mode); it.put("ipOrigin", settings.ipOrigin); it.put("tailscaleEnabled", settings.tailscaleEnabled)
  }

  @Command fun connectionSettings(invoke: Invoke) = host.runOnUiThread {
    try { launcher(); invoke.resolve(settingsJson(profiles.read())) }
    catch (error: Exception) { invoke.reject(error.message ?: "读取配置失败") }
  }

  @Command fun saveConnection(invoke: Invoke) {
    val args = try { invoke.parseArgs(SaveConnectionArgs::class.java) }
      catch (_: Exception) { invoke.reject("连接配置不完整"); return }
    host.runOnUiThread {
      try {
        launcher()
        val saved = profiles.save(args.mode, args.ipOrigin, args.tailscaleEnabled)
        generation++; cancelPending(); candidate = null
        invoke.resolve(settingsJson(saved))
      } catch (error: Exception) { invoke.reject(error.message ?: "保存配置失败") }
    }
  }

  private fun checkRoute(route: ConnectionRoute, cancellation: ConnectionAttempt.Cancellation) {
    cancellation.check()
    if (route.mode == "ip") ConnectionAttempt.checkDirect(route.origin, cancellation)
    else { P2PConnection.start(host.applicationContext); P2PConnection.awaitReadyAndReachable(cancellation) }
  }

  @Command fun attemptConnection(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val attempt = ++generation
      val settings = profiles.read()
      candidate = null
      val job = startPending(invoke)
      job.future = network.submit {
        val checked = runCatching { ConnectionAttempt.firstAvailable(settings) { checkRoute(it, job.cancellation) } }
        host.runOnUiThread { finish(job) {
          try {
            val outcome = checked.getOrThrow()
            launcher(); check(attempt == generation) { "配置已更改，已取消旧连接" }
            candidate = outcome.route; checkedAt = android.os.SystemClock.elapsedRealtime()
            val response = JSObject()
            response.put("connected", outcome.route != null)
            response.put("failures", org.json.JSONArray().also { failures -> outcome.failures.forEach {
              failures.put(JSObject().also { item -> item.put("mode", it.mode); item.put("message", it.message) })
            } })
            outcome.route?.let {
              response.put("mode", it.mode); response.put("origin", it.origin)
              response.put("entryAvailable", !resumeConsumed)
              response.put("resumeAvailable", !resumeConsumed && RememberedSession.hasCookie(it.origin))
            }
            invoke.resolve(response)
          } catch (error: Exception) { invoke.reject(error.message ?: "连接失败") }
        } }
      }
    } catch (error: Exception) { invoke.reject(error.message ?: "连接失败") }
  }

  @Command fun loginTailscale(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val settings = profiles.read()
      profiles.save("tailscale", settings.ipOrigin, true)
      val attempt = ++generation
      cancelPending()
      P2PConnection.start(host.applicationContext)
      P2PConnection.execute({ check(attempt == generation) { "登录已取消" }; P2PConnection.checked(NativeP2P.login()) }) { result ->
        host.runOnUiThread {
          try {
            launcher(); check(attempt == generation) { "配置已更改，已取消旧登录" }
            val status = result.getOrThrow()
            if (status.getString("state") != "Running") {
              val uri = URI(status.getString("authURL"))
              check(uri.scheme == "https" && uri.host == "login.tailscale.com" && uri.rawUserInfo == null) { "未获得有效登录链接，请重试" }
              host.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri.toString())))
            }
            invoke.resolve(JSObject().also { it.put("state", status.getString("state")) })
          } catch (error: Throwable) { invoke.reject(error.message ?: "无法打开登录页面") }
        }
      }
    } catch (error: Exception) { invoke.reject(error.message ?: "无法登录") }
  }

  @Command fun bindServer(invoke: Invoke) {
    val args = try { invoke.parseArgs(BindFrontendArgs::class.java) }
      catch (_: Exception) { invoke.reject("缺少服务器地址"); return }
    host.runOnUiThread {
      try {
        launcher()
        val settings = profiles.read()
        val allowed = settings.candidates()
        val route = candidate?.takeIf { it.origin == args.origin && it in allowed }
          ?: allowed.firstOrNull { it.origin == args.origin }
          ?: throw IllegalArgumentException("请先配置这个服务器地址")
        val attempt = generation
        val fresh = candidate == route && android.os.SystemClock.elapsedRealtime() - checkedAt < 10000
        val job = startPending(invoke)
        job.future = network.submit {
          val checked = runCatching { job.cancellation.check(); if (!fresh) checkRoute(route, job.cancellation) }
          host.runOnUiThread {
            if (!job.settled.get()) try {
              checked.getOrThrow(); launcher(); check(attempt == generation) { "配置已更改，已取消旧连接" }
              install(route, invoke, attempt, job)
            } catch (error: Throwable) { finish(job) { invoke.reject(error.message ?: "连接超时，请重试") } }
          }
        }
      } catch (error: Exception) { invoke.reject(error.message ?: "无法连接服务器") }
    }
  }

  private fun install(route: ConnectionRoute, invoke: Invoke, attempt: Int, job: Pending) {
    val webView = launcher()
    check(BundledWebViewSupport.available(host) && WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)) { "请更新 Android System WebView 后重试" }
    val origin = if (route.mode == "ip") ConnectionProfiles.parseDirect(route.origin)
      else BundledOrigin.parse(route.origin) { NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(it) }
    val installed = WebViewCompat.getWebViewClient(webView)
    if (client == null) {
      check(installed is RustWebViewClient) { "网页组件初始化尚未完成，请重试" }
      client = BundledWebViewClient(installed, BundledFrontendAssets(host.assets, selectedOrigin))
      webView.webViewClient = client!!
    } else { check(installed === client) { "网页组件已改变，请重新打开 App" } }
    val executor = java.util.concurrent.Executor { host.runOnUiThread(it) }
    val done = Runnable { finish(job) {
      try {
        launcher(); check(attempt == generation) { "配置已更改，已取消旧连接" }
        selectedOrigin.set(origin); resumeConsumed = true
        RememberedSession.persist(origin.value)
        invoke.resolve(JSObject().also { it.put("origin", origin.value) })
      } catch (error: Throwable) { invoke.reject(error.message ?: "无法打开工作区") }
    } }
    val proxy = if (route.mode == "ip") P2PConnection.checked(NativeP2P.direct(route.origin)).getString("proxy")
      else { NativeP2P.stopDirect(); NativeP2P.proxy() }
    check(proxy.startsWith("http://127.0.0.1:")) { "连接尚未准备好" }
    val config = ProxyConfig.Builder().addProxyRule(proxy).removeImplicitRules().addBypassRule("http://tauri.localhost:80").addBypassRule("https://tauri.localhost:443").build()
    ProxyController.getInstance().setProxyOverride(config, executor, done)
  }
}
