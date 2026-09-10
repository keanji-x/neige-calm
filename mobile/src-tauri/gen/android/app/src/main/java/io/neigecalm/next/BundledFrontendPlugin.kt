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
import java.util.concurrent.atomic.AtomicReference

@InvokeArg
class BindFrontendArgs { lateinit var origin: String }

@TauriPlugin
class BundledFrontendPlugin(private val host: Activity) : Plugin(host) {
  private var view: WebView? = null
  private var client: BundledWebViewClient? = null
  private val selectedOrigin = AtomicReference<BundledOrigin?>(null)
  private var resumeConsumed = false

  override fun load(webView: WebView) {
    view = webView
    P2PConnection.start(host)
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

  @Command fun connectionStatus(invoke: Invoke) {
    host.runOnUiThread {
      try { launcher() } catch (error: Exception) { invoke.reject(error.message); return@runOnUiThread }
      P2PConnection.execute({ P2PConnection.checked(NativeP2P.status()) }) { result ->
        host.runOnUiThread {
          try {
            launcher()
            val status = result.getOrThrow()
            val response = JSObject()
            response.put("state", status.getString("state"))
            response.put("origin", P2PConnection.ORIGIN)
            response.put("resumeAvailable", !resumeConsumed && RememberedSession.hasCookie())
            invoke.resolve(response)
          } catch (error: Throwable) { invoke.reject(error.message ?: "无法读取连接状态") }
        }
      }
    }
  }

  @Command fun loginTailscale(invoke: Invoke) {
    host.runOnUiThread {
      try { launcher() } catch (error: Exception) { invoke.reject(error.message); return@runOnUiThread }
      P2PConnection.execute({ P2PConnection.checked(NativeP2P.login()) }) { result ->
        host.runOnUiThread {
          try {
            launcher()
            val status = result.getOrThrow()
            if (status.getString("state") != "Running") {
              val uri = URI(status.getString("authURL"))
              check(uri.scheme == "https" && uri.host == "login.tailscale.com" && uri.rawUserInfo == null) { "未获得有效登录链接，请重试" }
              host.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri.toString())))
            }
            val response = JSObject()
            response.put("state", status.getString("state"))
            invoke.resolve(response)
          } catch (error: Throwable) { invoke.reject(error.message ?: "无法打开登录页面") }
        }
      }
    }
  }

  @Command fun bindServer(invoke: Invoke) {
    val args = try { invoke.parseArgs(BindFrontendArgs::class.java) }
      catch (_: Exception) { invoke.reject("缺少服务器地址"); return }
    host.runOnUiThread {
      try {
        launcher()
        check(args.origin == P2PConnection.ORIGIN) { "请扫描当前工作区的二维码" }
      } catch (error: Exception) { invoke.reject(error.message); return@runOnUiThread }
      P2PConnection.execute({ P2PConnection.checked(NativeP2P.status()).also {
        check(it.getString("state") == "Running") { "请先登录 Tailscale，等待连接恢复" }
      } }) { state ->
        host.runOnUiThread {
          try {
            state.getOrThrow()
            val webView = launcher()
            check(BundledWebViewSupport.available(host) && WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE)) { "请更新 Android System WebView 后重试" }
            val origin = BundledOrigin.parse(args.origin) { NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(it) }
            val installed = WebViewCompat.getWebViewClient(webView)
            if (client == null) {
              check(installed is RustWebViewClient) { "网页组件初始化尚未完成，请重试" }
              client = BundledWebViewClient(installed, BundledFrontendAssets(host.assets, selectedOrigin))
              webView.webViewClient = client!!
            } else { check(installed === client) { "网页组件已改变，请重新打开 App" } }
            val proxy = NativeP2P.proxy()
            check(proxy.startsWith("http://127.0.0.1:")) { "内置连接尚未准备好" }
            val config = ProxyConfig.Builder().addProxyRule(proxy).addBypassRule("tauri.localhost").build()
            ProxyController.getInstance().setProxyOverride(config, java.util.concurrent.Executor { host.runOnUiThread(it) }) {
              try {
                launcher()
                selectedOrigin.set(origin)
                resumeConsumed = true
                RememberedSession.persist()
                val response = JSObject()
                response.put("origin", origin.value)
                invoke.resolve(response)
              } catch (error: Throwable) { invoke.reject(error.message ?: "无法恢复工作区") }
            }
          } catch (error: Throwable) { invoke.reject(error.message ?: "无法准备本地界面") }
        }
      }
    }
  }
}
