package io.neigecalm.next

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.webkit.WebViewCompat
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ScanFileChooserInstrumentationTest {
  @Test fun chooserUsesDocumentIntentAndCancelsOnDisposalWithoutLaunchingOtherApps() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    assertTrue(context.getSharedPreferences("connection-profiles",0).edit().clear().commit())
    assertTrue(context.getSharedPreferences("workspace-resume",0).edit().clear().commit())
    val scenario = ActivityScenario.launch(MainActivity::class.java)
    lateinit var workspace: ScanWorkspace
    lateinit var chrome: WebChromeClient
    var intent: Intent? = null
    var result: ((Int,Intent?)->Unit)? = null
    var releases = 0
    try {
      scenario.onActivity { host ->
        val origin = "https://scan-file.invalid"
        val bootstrap = JSONObject().put("generation",1).put("origin",origin).put("enrollmentId","fixture")
          .put("attemptId","attempt").put("attemptSecret","a".repeat(64)).put("pairTicket","b".repeat(64)).put("deadline",System.currentTimeMillis()+120000)
        workspace = ScanWorkspace(host, WebView(host), BundledOrigin.parse(origin){false}, ScanDocument(bootstrap),{}, {},
          FileChooserLauncher { request, callback -> intent=request; result=callback; { releases++ } })
        workspace.view.loadDataWithBaseURL("$origin/next/", "<input type=file multiple accept='text/plain'>", "text/html", "UTF-8", null)
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
      // Wait for the real WebView document before entering its production chrome callback.
      val until = android.os.SystemClock.elapsedRealtime()+10000
      var loaded=false
      while (!loaded && android.os.SystemClock.elapsedRealtime()<until) {
        scenario.onActivity { loaded=workspace.view.url=="https://scan-file.invalid/next/" }
        if (!loaded) android.os.SystemClock.sleep(50)
      }
      assertTrue(loaded)
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
      }
    } finally { scenario.moveToState(Lifecycle.State.CREATED) }
  }
}
