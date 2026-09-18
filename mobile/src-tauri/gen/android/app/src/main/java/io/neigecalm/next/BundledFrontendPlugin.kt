package io.neigecalm.next

import android.app.Activity
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

@InvokeArg class EnrollFromScanArgs { lateinit var payload: String }
@InvokeArg class BindFrontendArgs { lateinit var origin: String; var intentId: String? = null }
@InvokeArg class AttemptConnectionArgs { var tailnetOrigin: String? = null; var confirmDirect: Boolean = false; var intentId: String? = null }
@InvokeArg class CancelConnectionArgs { lateinit var intentId: String }
@InvokeArg class SaveConnectionArgs { lateinit var mode: String; lateinit var ipOrigin: String; var tailscaleEnabled: Boolean by kotlin.properties.Delegates.notNull(); var clearSelection: Boolean = false }

@TauriPlugin
class BundledFrontendPlugin(private var host: Activity) : Plugin(host) {
  private var view: WebView? = null
  private var client: BundledWebViewClient? = null
  private var wryClient: RustWebViewClient? = null
  private val selectedOrigin = AtomicReference<BundledOrigin?>(null)
  private var network = Executors.newFixedThreadPool(2)
  private val profiles by lazy { ConnectionProfiles(host.applicationContext) }
  @Volatile private var generation = 0
  private var deadlines = Executors.newSingleThreadScheduledExecutor()
  private class Pending(val invoke: Invoke, val connectionIntent: String?) {
    val cancellation = ConnectionAttempt.Cancellation()
    val settled = java.util.concurrent.atomic.AtomicBoolean(false)
    var future: java.util.concurrent.Future<*>? = null
    var timeout: java.util.concurrent.Future<*>? = null
    var nativeOperation: NativeOperation? = null
  }
  private var pending: Pending? = null
  private val resume by lazy { ResumeEntry(host.applicationContext) }
  private var boundGeneration = -1
  private var resumeConsumed = false
  private var candidate: ConnectionRoute? = null
  private var checkedAt = 0L
  private var scanWorkspace: ScanWorkspace? = null
  private var enrollmentPending = false

  companion object {
    // Tauri keeps plugin instances for the process, but Wry replaces the Activity
    // and WebView on recreation. This is a native ownership handoff, not a bridge.
    private var active = java.lang.ref.WeakReference<BundledFrontendPlugin>(null)
    internal fun activeWorkspace(): WebView? = active.get()?.scanWorkspace?.view
    internal fun handleWorkspaceBack(): Boolean { val workspace = active.get()?.scanWorkspace ?: return false; workspace.back(); return true }
    internal fun attachActivity(activity: MainActivity, webView: WebView) {
      active.get()?.attach(activity, webView)
    }
  }
  init { active = java.lang.ref.WeakReference(this) }
  private fun attach(activity: Activity, webView: WebView) {
    if (host === activity && view === webView) return
    closeScanWorkspace()
    generation++; cancelPending(); selectedOrigin.set(null)
    host = activity; view = webView; client = null; wryClient = null; candidate = null; resumeConsumed = false
    if (network.isShutdown) network = Executors.newFixedThreadPool(2)
    if (deadlines.isShutdown) deadlines = Executors.newSingleThreadScheduledExecutor()
  }
  override fun load(webView: WebView) { attach(host, webView) }
  override fun onPause() { generation++; cancelPending(); scanWorkspace?.pause(); if (enrollmentPending) { NativeP2P.cancelEnrollment(); enrollmentPending = false } }
  override fun onResume() {
    scanWorkspace?.resume()
    val active = selectedOrigin.get()
    val webView = view
    if (active != null && webView != null) {
      observe(webView, active, generation)
      boundGeneration = generation
    }
  }
  override fun onDestroy(activity: androidx.appcompat.app.AppCompatActivity) {
    if (activity !== host) return
    closeScanWorkspace()
    generation++; cancelPending(); selectedOrigin.set(null); view = null
    network.shutdownNow(); deadlines.shutdownNow()
  }

  private fun cancelPending() {
    pending?.let {
      // Native revocation is synchronous and precedes interrupting the worker.
      // It also revokes a JNI call that has not entered Go yet.
      val revoked = runCatching { it.nativeOperation?.cancel() }
      it.cancellation.cancel(); it.future?.cancel(true); it.timeout?.cancel(false)
      if (it.settled.compareAndSet(false, true)) it.invoke.reject(revoked.exceptionOrNull()?.message ?: "连接已取消")
    }
    pending = null
  }

  private fun startPending(invoke: Invoke, seconds: Long = 15, native: Boolean = false, connectionIntent: String? = null): Pending {
    cancelPending()
    val job = Pending(invoke, connectionIntent)
    if (native) job.nativeOperation = NativeOperation.reserve()
    pending = job
    job.timeout = deadlines.schedule({ host.runOnUiThread {
      if (job.settled.compareAndSet(false, true)) {
        val revoked = runCatching { job.nativeOperation?.cancel() }
        generation++; job.cancellation.cancel(); job.future?.cancel(true)
        if (enrollmentPending) { NativeP2P.cancelEnrollment(); enrollmentPending = false }
        if (pending === job) pending = null
        invoke.reject(revoked.exceptionOrNull()?.message ?: "连接超时，请重新配置")
      }
    } }, seconds, java.util.concurrent.TimeUnit.SECONDS)
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
    (host as? MainActivity)?.installConnectionBackHandler()
    return webView
  }

  private fun settingsJson(settings: ConnectionSettings, known: List<String> = profiles.tailnetOrigins()) = JSObject().also {
    it.put("mode", settings.mode); it.put("ipOrigin", settings.ipOrigin); it.put("tailscaleEnabled", settings.tailscaleEnabled)
    it.put("tailnetOrigin", settings.tailnetOrigin); it.put("tailnetOrigins", org.json.JSONArray(known))
    it.put("explicitTailnet", settings.explicitTailnet)
    it.put("legacyTailnet", runCatching { profiles.needsLegacyConfirmation() }.getOrDefault(false))
  }

  @Command fun connectionSettings(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val read = runCatching { profiles.read() }
      val result = read.fold({ settingsJson(it) }, { settingsJson(ConnectionSettings("tailscale", "", false, "", false), emptyList()) })
      if (read.isFailure) result.put("configurationError", "已保存的连接配置无效，请重新填写并保存。")
      if (read.isSuccess && !resumeConsumed) resume.read(profiles)?.let { entry ->
        result.put("resumeEntry", JSObject().also { it.put("origin", entry.origin); it.put("route", entry.route) })
      }
      invoke.resolve(result)
    }
    catch (error: Exception) { invoke.reject(error.message ?: "读取配置失败") }
  }

  @Command fun selectSavedTailnet(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val args = invoke.parseArgs(BindFrontendArgs::class.java)
      generation++; cancelPending(); closeScanWorkspace()
      val saved = profiles.selectSavedTailnet(args.origin)
      candidate = null; selectedOrigin.set(null)
      invoke.resolve(settingsJson(saved))
    } catch (error: Exception) { invoke.reject(error.message ?: "无法选择工作区") }
  }

  @Command fun saveConnection(invoke: Invoke) {
    val args = try { invoke.parseArgs(SaveConnectionArgs::class.java) }
      catch (_: Exception) { invoke.reject("连接配置不完整"); return }
    host.runOnUiThread {
      try {
        launcher()
        generation++; cancelPending(); closeScanWorkspace()
        val saved = profiles.save(args.mode, args.ipOrigin, args.tailscaleEnabled, args.clearSelection)
        candidate = null; selectedOrigin.set(null)
        invoke.resolve(settingsJson(saved))
      } catch (error: Exception) { invoke.reject(error.message ?: "保存配置失败") }
    }
  }

  @Command fun cancelConnection(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val args = invoke.parseArgs(CancelConnectionArgs::class.java)
      require(args.intentId.matches(Regex("[A-Za-z0-9_-]{1,128}"))) { "连接操作标识无效" }
      if (pending?.connectionIntent == args.intentId) { generation++; cancelPending(); candidate = null }
      invoke.resolve()
    } catch (error: Exception) { invoke.reject(error.message ?: "无法取消连接") }
  }

  private fun checkRoute(route: ConnectionRoute, cancellation: ConnectionAttempt.Cancellation, operation: NativeOperation?, binding: String, confirmDirect: Boolean = false): String? {
    cancellation.check()
    if (route.mode == "ip") {
      val verified = checkNotNull(operation).run({}) { token -> P2PConnection.checked(NativeP2P.checkDirect(token, route.origin, binding, confirmDirect)) }
      cancellation.check()
      return verified.getJSONObject("binding").toString()
    }
    P2PConnection.start(host.applicationContext); P2PConnection.awaitReadyAndReachable(route.origin, cancellation)
    return null
  }

  @Command fun attemptConnection(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val args = invoke.parseArgs(AttemptConnectionArgs::class.java)
      require(!args.confirmDirect || args.tailnetOrigin == null) { "不能同时确认不同的连接目标" }
      require(args.intentId == null || args.intentId!!.matches(Regex("[A-Za-z0-9_-]{1,128}"))) { "连接操作标识无效" }
      require(!args.confirmDirect || args.intentId != null) { "确认连接需要操作标识" }
      closeScanWorkspace()
      val attempt = ++generation
      val settings = profiles.read()
      require(!args.confirmDirect || (settings.mode == "ip" && !settings.explicitTailnet)) { "请明确选择 IP 后确认连接" }
      val binding = profiles.directBinding(settings.ipOrigin)
      candidate = null
      val job = startPending(invoke, native = args.tailnetOrigin == null && !settings.explicitTailnet && settings.ipOrigin.isNotEmpty(), connectionIntent = args.intentId)
      job.future = network.submit {
        val checked = runCatching {
          var verifiedBinding: String? = null
          val outcome = ConnectionAttempt.firstAvailable(settings, args.tailnetOrigin) {
            val verified = checkRoute(it, job.cancellation, job.nativeOperation, binding, args.confirmDirect)
            if (it.mode == "ip") verifiedBinding = verified
          }
          Pair(outcome, verifiedBinding)
        }
        host.runOnUiThread { finish(job) {
          try {
            val (outcome, verifiedBinding) = checked.getOrThrow()
            launcher(); check(attempt == generation) { "配置已更改，已取消旧连接" }
            if (args.confirmDirect && outcome.route?.mode == "ip") profiles.confirmDirectBinding(outcome.route.origin, checkNotNull(verifiedBinding))
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

  private fun closeScanWorkspace() {
    if (scanWorkspace == null && !enrollmentPending) return
    NativeP2P.cancelEnrollment()
    enrollmentPending = false
    scanWorkspace?.destroy(); scanWorkspace = null
  }

  @Command fun cancelEnrollment(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher(); generation++; cancelPending()
      P2PConnection.checked(NativeP2P.cancelEnrollment())
      enrollmentPending = false
      scanWorkspace?.destroy(); scanWorkspace = null
      invoke.resolve()
    } catch (error: Exception) { invoke.reject(error.message ?: "取消失败，请重新打开 App") }
  }

  @Command fun resetEnrollment(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val intentGeneration = generation
      androidx.appcompat.app.AlertDialog.Builder(host).setTitle("重新开始手机入网？")
        .setMessage("这会退出 Neige 内置的手机网络连接并清除未完成的扫码操作。不会影响系统 Tailscale，也不会删除 Tailscale 后台的设备。确认后请重新扫码。")
        .setNegativeButton("取消") { _, _ -> invoke.reject("已取消重置") }
        .setOnCancelListener { invoke.reject("已取消重置") }
        .setPositiveButton("退出并重新扫码") { _, _ ->
          try {
            launcher(); check(intentGeneration == generation) { "配置已改变，请重新确认" }
            generation++; closeScanWorkspace(); cancelPending(); candidate = null; selectedOrigin.set(null)
            val attempt = generation
            val job = startPending(invoke, native = true)
            val operation = checkNotNull(job.nativeOperation)
            job.future = network.submit {
              val result = runCatching { operation.run({ P2PConnection.prepare(host.applicationContext) }) { token -> P2PConnection.checked(NativeP2P.resetEnrollment(token)) } }
              host.runOnUiThread { finish(job) {
                try {
                  result.getOrThrow(); launcher(); check(attempt == generation) { "重置结果已过期，请重新检查连接状态" }
                  val saved = profiles.disableTailnet(); resume.clear(); resumeConsumed = true
                  invoke.resolve(settingsJson(saved))
                } catch (error: Exception) { invoke.reject(error.message ?: "无法确认手机已退出网络，请重试") }
              } }
            }
          } catch (error: Exception) { invoke.reject(error.message ?: "无法重置手机入网") }
        }.show()
    } catch (error: Exception) { invoke.reject(error.message ?: "无法重置手机入网") }
  }

  @Command fun enrollFromScan(invoke: Invoke) {
    val args = try { invoke.parseArgs(EnrollFromScanArgs::class.java) }
      catch (_: Exception) { invoke.reject("无效的入网二维码"); return }
    host.runOnUiThread {
      try {
        val launcherView = launcher()
        check(args.payload.length <= 2048) { "入网二维码过长" }
        closeScanWorkspace()
        generation++; cancelPending(); candidate = null; selectedOrigin.set(null)
        val attempt = generation
        val job = startPending(invoke, 180, native = true)
        val operation = checkNotNull(job.nativeOperation)
        enrollmentPending = true
        job.future = network.submit {
          val result = runCatching {
            operation.run({ P2PConnection.prepare(host.applicationContext) }) { token -> P2PConnection.checked(NativeP2P.enroll(token, args.payload)) }
          }
          host.runOnUiThread {
            if (!job.settled.get()) try {
              val native = result.getOrThrow()
              launcher(); check(attempt == generation) { "旧扫码操作已取消" }
              val origin = BundledOrigin.parse(native.getString("origin")) { false }
              val document = ScanDocument(native.getJSONObject("bootstrap"))
              val proxy = native.getString("proxy")
              check(proxy.startsWith("http://127.0.0.1:")) { "连接尚未准备好" }
              val config = ProxyConfig.Builder().addProxyRule(proxy).removeImplicitRules()
                .addBypassRule("http://tauri.localhost:80").addBypassRule("https://tauri.localhost:443").build()
              NativeP2P.stopDirect()
              ProxyController.getInstance().setProxyOverride(config, java.util.concurrent.Executor { host.runOnUiThread(it) }, Runnable {
                finish(job) {
                  try {
                    launcher(); check(attempt == generation) { "旧扫码操作已取消" }
                    // Target selection proves transport only. No cookie is copied
                    // and the FE's same-document session gate stays closed.
                    profiles.selectTailnet(origin.value)
                    val route = resume.read(profiles)?.takeIf { it.origin == origin.value }?.route ?: "/next/"
                    lateinit var workspace: ScanWorkspace
                    workspace = ScanWorkspace(host as MainActivity, launcherView, origin, document, { url ->
                      if (scanWorkspace === workspace) resume.remember(profiles, origin, url)
                    }, {
                      generation++; closeScanWorkspace()
                      launcherView.loadUrl("http://tauri.localhost/")
                    })
                    scanWorkspace = workspace; enrollmentPending = false; resumeConsumed = true
                    invoke.resolve(JSObject().also { it.put("origin", origin.value) })
                    workspace.open(route)
                  } catch (error: Exception) { document.clear(); closeScanWorkspace(); invoke.reject(error.message ?: "无法打开工作区") }
                }
              })
            } catch (error: Exception) { finish(job) { closeScanWorkspace(); invoke.reject(error.message ?: "入网未完成，请重新扫码") } }
          }
        }
      } catch (error: Exception) { invoke.reject(error.message ?: "无法开始扫码入网") }
    }
  }

  @Command fun bindServer(invoke: Invoke) {
    val args = try { invoke.parseArgs(BindFrontendArgs::class.java) }
      catch (_: Exception) { invoke.reject("缺少服务器地址"); return }
    host.runOnUiThread {
      try {
        launcher()
        require(args.intentId == null || args.intentId!!.matches(Regex("[A-Za-z0-9_-]{1,128}"))) { "连接操作标识无效" }
        closeScanWorkspace()
        val settings = profiles.read()
        val allowed = settings.candidates()
        val route = candidate?.takeIf { it.origin == args.origin && it in allowed }
          ?: allowed.firstOrNull { it.origin == args.origin }
          ?: throw IllegalArgumentException("请先配置这个服务器地址")
        val attempt = generation
        val localResume = !resumeConsumed && resume.read(profiles)?.origin == route.origin
        val fresh = candidate == route && android.os.SystemClock.elapsedRealtime() - checkedAt < 10000
        val binding = profiles.directBinding(route.origin)
        val job = startPending(invoke, native = route.mode == "ip" && !fresh && !localResume, connectionIntent = args.intentId)
        job.future = network.submit {
          val checked = runCatching {
            job.cancellation.check()
            if (!fresh && !localResume) checkRoute(route, job.cancellation, job.nativeOperation, binding)
            // Binding the loopback proxy does not wait for DNS or Tailnet readiness.
            if (route.mode == "tailscale") P2PConnection.prepare(host.applicationContext)
          }
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

  @Command fun confirmLegacyTailnet(invoke: Invoke) = host.runOnUiThread {
    try {
      launcher()
      val args = invoke.parseArgs(BindFrontendArgs::class.java)
      val settings = profiles.read()
      check(args.origin == P2PConnection.ORIGIN && settings.tailnetOrigin == args.origin && settings.tailscaleEnabled) { "请先扫描这个工作区的二维码" }
      generation++; cancelPending(); closeScanWorkspace(); candidate = null; selectedOrigin.set(null)
      val attempt = generation
      val job = startPending(invoke, native = true)
      val operation = checkNotNull(job.nativeOperation)
      job.future = network.submit {
        val result = runCatching { operation.run({ P2PConnection.prepare(host.applicationContext) }) { token ->
          P2PConnection.checked(NativeP2P.confirmLegacy(token, args.origin))
        } }
        host.runOnUiThread { finish(job) {
          try {
            result.getOrThrow(); launcher(); check(attempt == generation) { "旧工作区确认已取消" }
            invoke.resolve(settingsJson(profiles.selectTailnet(args.origin)))
          } catch (error: Exception) { operation.cancel(); invoke.reject(error.message ?: "工作区确认失败") }
        } }
      }
    } catch (error: Exception) { invoke.reject(error.message ?: "无法确认旧工作区") }
  }

  private fun observe(webView: WebView, origin: BundledOrigin, ownerGeneration: Int) {
    val installed = WebViewCompat.getWebViewClient(webView)
    val original = if (client == null) {
      check(installed is RustWebViewClient) { "网页组件初始化尚未完成，请重试" }
      wryClient = installed
      installed
    } else {
      check(installed === client) { "网页组件已改变，请重新打开 App" }
      checkNotNull(wryClient)
    }
    lateinit var observer: BundledWebViewClient
    observer = BundledWebViewClient(original, BundledFrontendAssets(host.assets, selectedOrigin)) visited@{ url ->
      // Sign ownership when installing the observer. Reading the current epoch
      // inside a retired callback would accidentally authorize that old view.
      if (view !== webView || client !== observer || ownerGeneration != generation || boundGeneration != ownerGeneration || selectedOrigin.get() != origin) return@visited
      resume.remember(profiles, origin, url)
      if (runCatching { URI(url).host == "tauri.localhost" }.getOrDefault(false)) {
        selectedOrigin.set(null); generation++; cancelPending()
      }
    }
    client = observer
    webView.webViewClient = observer
  }

  private fun install(route: ConnectionRoute, invoke: Invoke, attempt: Int, job: Pending) {
    val webView = launcher()
    check(BundledWebViewSupport.available(host) && WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)) { "请更新 Android System WebView 后重试" }
    val origin = if (route.mode == "ip") ConnectionProfiles.parseDirect(route.origin)
      else BundledOrigin.parse(route.origin) { NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(it) }
    observe(webView, origin, attempt)
    val executor = java.util.concurrent.Executor { host.runOnUiThread(it) }
    val done = Runnable { finish(job) {
      try {
        launcher(); check(attempt == generation) { "配置已更改，已取消旧连接" }
        selectedOrigin.set(origin); boundGeneration = attempt; resumeConsumed = true
        resume.remember(profiles, origin, origin.value + (resume.read(profiles)?.route ?: "/next/"))
        if (route.mode == "tailscale") P2PConnection.wake(host.applicationContext)
        RememberedSession.persist(origin.value)
        invoke.resolve(JSObject().also { it.put("origin", origin.value) })
      } catch (error: Throwable) { invoke.reject(error.message ?: "无法打开工作区") }
    } }
    check(attempt == generation && !job.settled.get()) { "旧连接已取消" }
    val proxy = if (route.mode == "ip") P2PConnection.checked(NativeP2P.direct(route.origin, profiles.directBinding(route.origin))).getString("proxy")
      else { NativeP2P.stopDirect(); P2PConnection.checked(NativeP2P.tailnet(route.origin)).getString("proxy") }
    check(proxy.startsWith("http://127.0.0.1:")) { "连接尚未准备好" }
    val config = ProxyConfig.Builder().addProxyRule(proxy).removeImplicitRules().addBypassRule("http://tauri.localhost:80").addBypassRule("https://tauri.localhost:443").build()
    ProxyController.getInstance().setProxyOverride(config, executor, done)
  }
}
