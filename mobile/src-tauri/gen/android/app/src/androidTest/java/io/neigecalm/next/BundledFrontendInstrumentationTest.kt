package io.neigecalm.next

import android.Manifest
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.WebView
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import androidx.webkit.WebViewCompat
import org.json.JSONObject
import org.json.JSONTokener
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class BundledFrontendInstrumentationTest {
  @get:Rule val camera: GrantPermissionRule = GrantPermissionRule.grant(Manifest.permission.CAMERA)
  private lateinit var activity: ActivityScenario<MainActivity>
  private lateinit var webView: WebView
  private lateinit var origin: String
  private lateinit var otherOrigin: String
  private lateinit var badOrigin: String
  private lateinit var password: String

  private fun findWebView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (index in 0 until view.childCount) findWebView(view.getChildAt(index))?.let { return it }
    return null
  }

  private fun evaluate(script: String): Any? {
    val latch = CountDownLatch(1)
    val result = AtomicReference<String>()
    activity.onActivity { webView.evaluateJavascript(script) { value -> result.set(value); latch.countDown() } }
    assertTrue("JavaScript evaluation timed out", latch.await(10, TimeUnit.SECONDS))
    return JSONTokener(result.get() ?: "null").nextValue()
  }

  private fun waitFor(message: String, script: String) {
    val deadline = SystemClock.elapsedRealtime() + 30000
    while (SystemClock.elapsedRealtime() < deadline) {
      if (evaluate(script) == true) return
      SystemClock.sleep(100)
    }
    fail(message + ": " + evaluate("document.body.innerText.slice(0,1000)"))
  }

  private fun asyncValue(expression: String): JSONObject {
    evaluate("window.__nativeProbe=null; Promise.resolve().then(function(){return " + expression + ";}).then(function(value){window.__nativeProbe={ok:true,value:value};},function(error){window.__nativeProbe={ok:false,error:String(error)};});")
    val deadline = SystemClock.elapsedRealtime() + 30000
    while (SystemClock.elapsedRealtime() < deadline) {
      val encoded = evaluate("JSON.stringify(window.__nativeProbe && typeof window.__nativeProbe.ok==='boolean' ? window.__nativeProbe : undefined)")
      if (encoded is String) {
        val result = JSONObject(encoded)
        if (result.has("ok")) return result
      }
      SystemClock.sleep(100)
    }
    throw AssertionError("Asynchronous browser operation timed out at " + evaluate("location.href"))
  }

  private fun transition(action: (WebView) -> Unit) {
    val marker = JSONObject.quote(UUID.randomUUID().toString())
    evaluate("window.__previousDocument=" + marker)
    activity.onActivity { action(webView) }
    waitFor("Navigation did not finish in a new document",
      "window.__previousDocument!==" + marker + " && document.readyState==='complete'")
  }

  private fun navigate(url: String) = transition { it.loadUrl(url) }
  private fun api(path: String): JSONObject {
    val result = asyncValue("fetch(" + JSONObject.quote(origin + path) + ").then(function(r){return r.json()})")
    assertTrue(result.toString(), result.getBoolean("ok"))
    return result.getJSONObject("value")
  }

  @Before fun launch() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    Assume.assumeTrue(BundledWebViewSupport.available(instrumentation.targetContext))
    val args = InstrumentationRegistry.getArguments()
    origin = requireNotNull(args.getString("server_origin"))
    otherOrigin = requireNotNull(args.getString("other_origin"))
    badOrigin = requireNotNull(args.getString("bad_origin"))
    password = requireNotNull(args.getString("test_password"))
    activity = ActivityScenario.launch(MainActivity::class.java)
    val cookies = CountDownLatch(1)
    val viewDeadline = SystemClock.elapsedRealtime() + 15000
    while (!::webView.isInitialized && SystemClock.elapsedRealtime() < viewDeadline) {
      activity.onActivity { findWebView(it.findViewById(android.R.id.content))?.let { found -> webView = found } }
      if (!::webView.isInitialized) SystemClock.sleep(100)
    }
    assertTrue("Native WebView did not attach", ::webView.isInitialized)
    activity.onActivity {
      CookieManager.getInstance().removeAllCookies { cookies.countDown() }
    }
    assertTrue(cookies.await(10, TimeUnit.SECONDS))
    waitFor("Launcher/native bridge did not load", "location.host==='tauri.localhost' && typeof window.__TAURI__?.core?.invoke==='function'")
    val binding = asyncValue("window.__TAURI__.core.invoke('plugin:bundled-frontend|bind_server',{origin:" + JSONObject.quote(origin) + "})")
    assertTrue(binding.toString(), binding.getBoolean("ok"))
    activity.onActivity { assertEquals("BundledWebViewClient", WebViewCompat.getWebViewClient(webView).javaClass.simpleName) }
    navigate(origin + "/next/")
    waitFor("Bundled connection UI did not render", "document.body.innerText.includes('扫码连接你的工作区')")
    api("/_test/reset")
  }

  // Android Test Orchestrator owns each test process and its Activity lifecycle.

  private fun login() {
    evaluate("Array.from(document.querySelectorAll('button')).find(function(b){return b.textContent==='使用账号登录'}).click()")
    waitFor("Manual login did not render", "!!document.querySelector('input[name=password]')")
    evaluate("(function(){var set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;var u=document.querySelector('input[name=username]');var p=document.querySelector('input[name=password]');set.call(u,'owner');u.dispatchEvent(new Event('input',{bubbles:true}));set.call(p," + JSONObject.quote(password) + ");p.dispatchEvent(new Event('input',{bubbles:true}));})()")
    evaluate("document.querySelector('form').requestSubmit()")
    waitFor("Real Next workspace did not render", "document.body.innerText.includes('Today')")
  }

  @Test fun localFrontendUsesRealBackendAndWebsocketWithoutAssetDownloads() {
    login()
    val identity = api("/api/auth/whoami")
    assertEquals("owner", identity.getString("role"))
    waitFor("WebSocket connection did not complete", "typeof window.__TAURI__.core.invoke==='function'")
    var stats = api("/_test/stats")
    val deadline = SystemClock.elapsedRealtime() + 15000
    while (stats.getInt("websocketAccepted") == 0 && SystemClock.elapsedRealtime() < deadline) { SystemClock.sleep(200); stats = api("/_test/stats") }
    assertTrue("No authenticated WebSocket upgrade: " + stats, stats.getInt("websocketAccepted") > 0)
    assertTrue(stats.getInt("api") > 0)
    assertEquals("Frontend resources escaped the APK", 0, stats.getInt("assets"))
    assertEquals(404, asyncValue("fetch('/next/assets/not-in-the-apk.js').then(function(r){return r.status})").getInt("value"))
    assertEquals(400, asyncValue("fetch('/next/assets/invalid%2fpath.js').then(function(r){return r.status})").getInt("value"))
    assertEquals(0, api("/_test/stats").getInt("assets"))
    navigate(origin + "/next/settings/network")
    waitFor("Bundled deep link did not render", "document.body.innerText.includes('Network')")
    assertEquals(0, api("/_test/stats").getInt("assets"))
    api("/_test/offline")
    transition { it.reload() }
    waitFor("Offline session probe became blank", "document.body.innerText.includes('暂时无法连接服务器')")
    api("/_test/online")
  }

  private fun assertNativeDenied() {
    waitFor("Native bridge must exist for an actual ACL check", "typeof window.__TAURI__?.core?.invoke==='function'")
    for (command in listOf("plugin:bundled-frontend|bind_server", "plugin:barcode-scanner|request_permissions")) {
      val result = asyncValue("window.__TAURI__.core.invoke(" + JSONObject.quote(command) + ",{origin:" + JSONObject.quote(otherOrigin) + "})")
      assertFalse("Remote page invoked " + command, result.getBoolean("ok"))
      assertTrue("Expected the permission fence, not a missing bridge or handler: " + result, result.getString("error").contains("not allowed"))
    }
  }

  @Test fun remotePagesCannotRebindOrUseCamera() {
    assertNativeDenied()
    transition { it.reload() }
    waitFor("Reload did not render bundled UI", "document.body.innerText.includes('扫码连接你的工作区')")
    assertNativeDenied()
    navigate(origin + "/_test/redirect")
    waitFor("Cross-origin redirect did not load", "location.origin===" + JSONObject.quote(otherOrigin) + " && document.body.innerText.includes('Other origin')")
    assertNativeDenied()
    transition { it.goBack() }
    waitFor("Back navigation did not restore the paired origin", "location.origin===" + JSONObject.quote(origin))
    assertNativeDenied()
    transition { it.goBack() }
    waitFor("Launcher did not return", "location.host==='tauri.localhost' && !!document.querySelector('#server')")
    assertTrue(asyncValue("window.__TAURI__.core.invoke('plugin:bundled-frontend|bind_server',{origin:" + JSONObject.quote(origin) + "})").getBoolean("ok"))
  }

  @Test fun untrustedTlsEndpointCannotExecuteItsDocument() {
    activity.onActivity { webView.loadUrl(badOrigin + "/_test/untrusted") }
    SystemClock.sleep(4000)
    assertNotEquals(true, evaluate("window.untrustedCertificateAccepted===true"))
    navigate(origin + "/next/")
    waitFor("Could not return after rejected TLS", "document.body.innerText.includes('扫码连接你的工作区')")
    val stats = api("/_test/stats")
    assertTrue("The untrusted TLS endpoint was never contacted", stats.getInt("badTlsConnections") > 0)
    assertEquals("Untrusted TLS reached HTTP", 0, stats.getInt("badTlsHttp"))
  }
}
