package io.neigecalm.next

import android.net.Uri
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.webkit.WebViewCompat
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayInputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/** Real WebView, production workspace/assets and the shipped FE. API failures
 * are a local fixture, not evidence of real Tailnet or host pairing success. */
@RunWith(AndroidJUnit4::class)
class ScanWorkspaceInstrumentationTest {
  private val origin = "https://scan-native.invalid"
  private fun find(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (i in 0 until view.childCount) find(view.getChildAt(i))?.let { return it }
    return null
  }
  private fun evaluate(scenario: ActivityScenario<MainActivity>, view: WebView, script: String): String {
    val result = AtomicReference(""); val done = CountDownLatch(1)
    scenario.onActivity { view.evaluateJavascript(script) { result.set(it); done.countDown() } }
    assertTrue(done.await(3, TimeUnit.SECONDS)); return result.get()
  }
  private fun await(scenario: ActivityScenario<MainActivity>, view: WebView, script: String) {
    val until = SystemClock.elapsedRealtime() + 15000
    while (SystemClock.elapsedRealtime() < until) {
      if (evaluate(scenario, view, script) == "true") return
      SystemClock.sleep(100)
    }
    fail("WebView condition did not hold: $script")
  }
  private fun request(url: String, main: Boolean) = object : WebResourceRequest {
    override fun getUrl() = Uri.parse(url)
    override fun isForMainFrame() = main
    override fun isRedirect() = false
    override fun hasGesture() = false
    override fun getMethod() = "GET"
    override fun getRequestHeaders() = emptyMap<String, String>()
  }
  @Test fun bundledScanDocumentExecutesOnceWithoutBridgeAndRejectsRemoteCode() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    assertTrue(context.getSharedPreferences("connection-profiles", 0).edit().clear().commit())
    assertTrue(context.getSharedPreferences("workspace-resume", 0).edit().clear().commit())
    val scenario = ActivityScenario.launch(MainActivity::class.java)
    lateinit var workspace: ScanWorkspace
    lateinit var launcher: WebView
    lateinit var client: WebViewClient
    var visits = 0
    try {
      scenario.onActivity { host ->
        launcher = checkNotNull(find(host.findViewById(android.R.id.content)))
        val bootstrap = JSONObject().put("generation", 7).put("origin", origin).put("enrollmentId", "native-fixture")
          .put("attemptId", "attempt").put("attemptSecret", "a".repeat(64)).put("pairTicket", "b".repeat(64))
          .put("deadline", System.currentTimeMillis() + 120000)
        workspace = ScanWorkspace(host, launcher, BundledOrigin.parse(origin) { false }, ScanDocument(bootstrap), { visits++ }, {})
        client = WebViewCompat.getWebViewClient(workspace.view)!!
        // Hold the network boundary locally; never make a real account request.
        workspace.view.webViewClient = object : WebViewClient() {
          override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
            if (request.url.path?.startsWith("/api/") == true) return WebResourceResponse(
              if (request.url.path == "/api/remote.js") "application/javascript" else "application/json", "UTF-8",
              if (request.url.path == "/api/remote.js") 200 else 503, "Fixture", mapOf("Cache-Control" to "no-store"),
              ByteArrayInputStream(if (request.url.path == "/api/remote.js") "window.remoteCodeExecuted=true; export default 1;".toByteArray() else "{}".toByteArray()))
            return client.shouldInterceptRequest(view, request)
          }
          override fun doUpdateVisitedHistory(view: WebView, url: String, reload: Boolean) = client.doUpdateVisitedHistory(view, url, reload)
        }
        workspace.open("/next/")
        assertEquals(View.GONE, launcher.visibility)
      }
      await(scenario, workspace.view, "document.querySelector('[data-nc-recovery-status]') !== null && !Object.hasOwn(window,'__NEIGE_SCAN__')")
      assertEquals("true", evaluate(scenario, workspace.view,
        "typeof window.__TAURI__==='undefined' && typeof window.__TAURI_INTERNALS__==='undefined' && typeof window.ipc==='undefined'"))
      evaluate(scenario, workspace.view, "window.policyViolations=[]; document.addEventListener('securitypolicyviolation',e=>policyViolations.push(e.blockedURI)); " +
        "let s=document.createElement('script'); s.src='/api/remote.js'; document.head.append(s); " +
        "import('/api/remote.js').catch(()=>{}); let i=document.createElement('script'); i.textContent='window.inlineCodeExecuted=true'; document.head.append(i); true")
      await(scenario, workspace.view, "policyViolations.some(v=>v.includes('/api/remote.js')) && policyViolations.includes('inline')")
      assertEquals("true", evaluate(scenario, workspace.view, "!window.remoteCodeExecuted && !window.inlineCodeExecuted"))
      scenario.onActivity {
        val missing = client.shouldInterceptRequest(workspace.view, request("$origin/next/assets/not-in-apk.js", false))
        assertNotNull("Missing assets must not fall through to network", missing)
        assertEquals(404, missing!!.statusCode)
        val reload = client.shouldInterceptRequest(workspace.view, request("$origin/next/", true))!!
        assertFalse("Reload must not receive the scan capability again", reload.data.bufferedReader().use { it.readText() }.contains("__NEIGE_SCAN__"))
        workspace.destroy()
        val before = visits
        client.doUpdateVisitedHistory(workspace.view, "$origin/next/track/retired", false)
        assertEquals("Destroyed document cannot update resume", before, visits)
        assertNull(workspace.view.parent)
        assertEquals(View.VISIBLE, launcher.visibility)
      }
    } finally {
      // The parent driver force-stops only after instrumentation reports;
      // closing Tauri's last Activity would terminate its in-process runner.
      scenario.moveToState(Lifecycle.State.CREATED)
    }
  }
}
