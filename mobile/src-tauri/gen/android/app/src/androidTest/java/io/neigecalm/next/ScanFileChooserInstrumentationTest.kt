package io.neigecalm.next

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.webkit.WebViewCompat
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayInputStream

@RunWith(AndroidJUnit4::class)
class ScanFileChooserInstrumentationTest {
  @Test fun chooserUsesDocumentIntentAndCancelsOnDisposalWithoutLaunchingOtherApps() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    assertTrue(context.getSharedPreferences("connection-profiles",0).edit().clear().commit())
    assertTrue(context.getSharedPreferences("workspace-resume",0).edit().clear().commit())
    val scenario = ScanFixtureActivity.acquire("chooser")
    lateinit var workspace: ScanWorkspace
    lateinit var chrome: WebChromeClient
    var intent: Intent? = null
    var result: ((Int,Intent?)->Unit)? = null
    var releases = 0
    var dispose: () -> Unit = {}
    try {
      scenario.onActivity { host ->
        val origin = "https://scan-file.invalid"
        val bootstrap = JSONObject().put("generation",1).put("origin",origin).put("enrollmentId","fixture")
          .put("attemptId","attempt").put("attemptSecret","a".repeat(64)).put("pairTicket","b".repeat(64)).put("deadline",System.currentTimeMillis()+120000)
        val launcher = checkNotNull(ScanFixtureActivity.findWebView(host.findViewById(android.R.id.content)))
        workspace = ScanWorkspace(host, launcher, BundledOrigin.parse(origin){false}, ScanDocument(bootstrap),{}, {},
          FileChooserLauncher { request, callback -> intent=request; result=callback; { releases++ } })
        dispose = { workspace.destroy() }
        val client = checkNotNull(WebViewCompat.getWebViewClient(workspace.view))
        workspace.view.webViewClient = object : WebViewClient() {
          override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
            if (request.url.path?.startsWith("/api/") == true) return WebResourceResponse("application/json", "UTF-8", 503, "Fixture",
              mapOf("Cache-Control" to "no-store"), ByteArrayInputStream("{}".toByteArray()))
            return client.shouldInterceptRequest(view, request)
          }
          override fun onPageStarted(view: WebView, url: String, favicon: android.graphics.Bitmap?) {
            client.onPageStarted(view, url, favicon)
            ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("chooser.page-started", host, view))
          }
          override fun onPageFinished(view: WebView, url: String) {
            client.onPageFinished(view, url)
            ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("chooser.page-finished", host, view))
          }
          override fun doUpdateVisitedHistory(view: WebView, url: String, reload: Boolean) = client.doUpdateVisitedHistory(view, url, reload)
        }
        ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("chooser.before-open",host,workspace.view))
        workspace.open("/next/")
        chrome = checkNotNull(WebViewCompat.getWebChromeClient(workspace.view))
      }
      val callbacks = mutableListOf<Array<Uri>?>()
      val params = object : WebChromeClient.FileChooserParams() {
        override fun getMode() = MODE_OPEN_MULTIPLE
        override fun getAcceptTypes() = arrayOf("text/plain")
        override fun isCaptureEnabled() = false
        override fun getTitle(): CharSequence? = null
        override fun getFilenameHint(): String? = null
        override fun createIntent() = Intent(Intent.ACTION_OPEN_DOCUMENT)
      }
      ScanFixtureActivity.awaitDocument(scenario,workspace.view,"https://scan-file.invalid/next/")
      scenario.onActivity {
        assertTrue(chrome.onShowFileChooser(workspace.view, ValueCallback { callbacks.add(it) }, params))
        assertEquals(Intent.ACTION_OPEN_DOCUMENT,intent?.action)
        assertTrue(intent!!.hasCategory(Intent.CATEGORY_OPENABLE))
        assertEquals("text/plain",intent!!.type)
        assertTrue(intent!!.getBooleanExtra(Intent.EXTRA_ALLOW_MULTIPLE,false))
        // Reject arbitrary file:// input even if an external picker returns it.
        result!!(Activity.RESULT_OK,Intent().setData(Uri.parse("file:///data/private")).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION))
        assertEquals(1,callbacks.size); assertNull(callbacks[0])
        chrome.onShowFileChooser(workspace.view,ValueCallback { callbacks.add(it) },params)
        val late=result!!
        workspace.destroy()
        assertEquals(2,callbacks.size); assertNull(callbacks[1])
        late(Activity.RESULT_CANCELED,null)
        assertEquals(2,callbacks.size); assertEquals(2,releases)
        ScanFixtureActivity.emit(JSONObject().put("stage","chooser.assertions-passed").put("callbacks",callbacks.size).put("releases",releases))
      }
    } finally {
      scenario.onActivity { dispose() }
      ScanFixtureActivity.pause(scenario,"chooser")
    }
  }
}
