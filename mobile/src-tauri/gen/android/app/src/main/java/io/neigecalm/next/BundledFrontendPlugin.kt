package io.neigecalm.next

import android.app.Activity
import android.security.NetworkSecurityPolicy
import android.webkit.WebView
import androidx.webkit.WebViewCompat
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

  override fun load(webView: WebView) { view = webView }

  @Command
  fun bindServer(invoke: Invoke) {
    val args = try { invoke.parseArgs(BindFrontendArgs::class.java) }
      catch (_: Exception) { invoke.reject("缺少服务器地址"); return }
    host.runOnUiThread {
      try {
        val webView = checkNotNull(view) { "网页组件尚未准备好，请重试" }
        val current = URI(webView.url ?: "")
        check(current.host == "tauri.localhost" && current.rawUserInfo == null
          && ((current.scheme == "http" && current.port in listOf(-1, 80))
            || (current.scheme == "https" && current.port in listOf(-1, 443)))) { "只能从 App 连接页修改服务器" }
        check(BundledWebViewSupport.available(host)) {
          "系统网页组件版本过旧，请更新 Android System WebView 后重试"
        }
        val origin = BundledOrigin.parse(args.origin) { NetworkSecurityPolicy.getInstance().isCleartextTrafficPermitted(it) }
        // Binding is invoked from the loaded launcher, after Wry has installed
        // the client retained by Ipc. Wrapping during onWebViewCreate is too early.
        val installed = WebViewCompat.getWebViewClient(webView)
        if (client == null) {
          check(installed is RustWebViewClient) { "网页组件初始化尚未完成，请重试" }
          client = BundledWebViewClient(installed, BundledFrontendAssets(host.assets, selectedOrigin))
          webView.webViewClient = client!!
        } else {
          check(installed === client) { "网页组件已改变，请重新打开 App" }
        }
        // The existing launcher optionally remembers the address. Native
        // authority is established afresh for this WebView, never by a cookie.
        selectedOrigin.set(origin)
        val result = JSObject()
        result.put("origin", origin.value)
        invoke.resolve(result)
      } catch (error: Exception) { invoke.reject(error.message ?: "无法准备本地界面") }
    }
  }
}
