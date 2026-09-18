package io.neigecalm.next

import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.json.JSONObject
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/** Device-independent: no emulator gateway, external server, VPN or network toggle. */
@RunWith(AndroidJUnit4::class)
class RecoveryInstrumentationTest {
  internal val origin = "https://recovery.invalid"
  private fun webView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (i in 0 until view.childCount) webView(view.getChildAt(i))?.let { return it }
    return null
  }
  internal fun prepare(route: String) {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented")) { "Recovery tests must use the isolated instrumented APK" }
    val profiles = ConnectionProfiles(context)
    profiles.save("ip", origin, false)
    ResumeEntry(context).remember(profiles, ConnectionProfiles.parseDirect(origin), origin + route)
  }
  internal fun evaluate(activity: ActivityScenario<MainActivity>, script: String): String {
    val value = AtomicReference<String>("")
    val done = CountDownLatch(1)
    activity.onActivity { host ->
      val view = webView(host.findViewById(android.R.id.content))
      if (view == null) done.countDown() else view.evaluateJavascript(script) { value.set(it); done.countDown() }
    }
    assertTrue(done.await(3, TimeUnit.SECONDS))
    return value.get()
  }
  internal fun await(activity: ActivityScenario<MainActivity>, script: String, message: String) {
    val until = SystemClock.elapsedRealtime() + 10000
    while (SystemClock.elapsedRealtime() < until) {
      if (evaluate(activity, script) == "true") return
      SystemClock.sleep(100)
    }
    fail(message + "; observed=" + evaluate(activity,
      "JSON.stringify({path:location.pathname,page:document.querySelector('[data-nc-recovery-page]')?.getAttribute('data-nc-recovery-page'),status:document.querySelector('[data-nc-recovery-status]')?.getAttribute('data-nc-recovery-status')})"))
  }
  internal fun assertNativeDenied(activity: ActivityScenario<MainActivity>) {
    await(activity, "typeof window.__TAURI__?.core?.invoke === 'function'", "Missing real native bridge for ACL validation")
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    val profiles = ConnectionProfiles(context)
    val before = profiles.read()
    val revision = profiles.revision()
    val commands = listOf(
      "plugin:bundled-frontend|save_connection" to "{mode:'ip',ipOrigin:'https://denied.invalid',tailscaleEnabled:false}",
      "plugin:barcode-scanner|request_permissions" to "{}",
      "plugin:barcode-scanner|scan" to "{formats:['QR_CODE'],cameraDirection:'back',windowed:true}"
    )
    for ((command, arguments) in commands) {
      evaluate(activity, "window.nativeAclProbe = null; window.__TAURI__.core.invoke(" + JSONObject.quote(command) + "," + arguments + ")" +
        ".then(() => {window.nativeAclProbe={ok:true}}, error => {window.nativeAclProbe={ok:false,error:String(error)}}); true")
      await(activity, "window.nativeAclProbe !== null", "Native ACL request did not settle")
      val result = JSONObject(evaluate(activity, "window.nativeAclProbe"))
      assertFalse("Remote bundled page invoked $command", result.getBoolean("ok"))
      assertTrue("Expected Tauri permission denial, not missing bridge or malformed arguments: $result", result.getString("error").contains("not allowed"))
      assertEquals(before, profiles.read())
      assertEquals(revision, profiles.revision())
    }
  }
  internal fun withLiveActivity(test: (ActivityScenario<MainActivity>) -> Unit) {
    val activity = ActivityScenario.launch(MainActivity::class.java)
    try { test(activity) }
    finally {
      // Closing Tauri's last Activity exits its host process, including this
      // in-process runner. The 5f2ff75c6 baseline reproduces the same FORTIFY.
      // run-recovery-process.py owns final force-stop after the runner reports.
      activity.moveToState(Lifecycle.State.CREATED)
    }
  }
  @Test fun offlineColdEntryShowsSavedTrackAndCornerStatusBeforeNetworkIsReady() {
    prepare("/next/track/saved-track?panel=cards")
    withLiveActivity { activity ->
      await(activity, "location.pathname === '/next/track/saved-track' && document.querySelector('[data-nc-recovery-page=track]') !== null && document.querySelector('[data-nc-recovery-status]') !== null",
        "Offline cold start waited for network instead of painting the saved local Track")
      assertEquals("true", evaluate(activity, "document.body.innerText.includes('等待恢复终端') && !document.body.innerText.includes('已连接')"))
      assertEquals("true", evaluate(activity, "document.querySelector('[data-nc-recovery-status]').getBoundingClientRect().right <= innerWidth"))
      assertNativeDenied(activity)
    }
  }
  @Test fun historySurvivesColdRecreationAndHotResumeKeepsTheSameWebView() {
    prepare("/next/track/saved-track")
    withLiveActivity { activity ->
      await(activity, "document.querySelector('[data-nc-recovery-page=track]') !== null", "Missing saved local page")
      assertNativeDenied(activity)
      val initial = AtomicReference<WebView>()
      val retiredClient = AtomicReference<android.webkit.WebViewClient>()
      activity.onActivity {
        val view = webView(it.findViewById(android.R.id.content))!!
        initial.set(view); retiredClient.set(androidx.webkit.WebViewCompat.getWebViewClient(view))
      }
      evaluate(activity, "window.retainedRecoveryNode = document.querySelector('[data-nc-recovery-page]'); history.pushState({}, '', '/next/settings/network'); true")
      val context = InstrumentationRegistry.getInstrumentation().targetContext
      val until = SystemClock.elapsedRealtime() + 5000
      while (ResumeEntry(context).read(ConnectionProfiles(context))?.route != "/next/settings/network" && SystemClock.elapsedRealtime() < until) SystemClock.sleep(100)
      assertEquals("/next/settings/network", ResumeEntry(context).read(ConnectionProfiles(context))?.route)
      activity.moveToState(Lifecycle.State.CREATED)
      activity.moveToState(Lifecycle.State.RESUMED)
      activity.onActivity { assertSame(initial.get(), webView(it.findViewById(android.R.id.content))) }
      assertEquals("true", evaluate(activity, "window.retainedRecoveryNode === document.querySelector('[data-nc-recovery-page]')"))
      activity.recreate()
      await(activity, "location.pathname === '/next/settings/network' && document.querySelector('[data-nc-recovery-page=settings]') !== null",
        "Activity recreation did not reopen the history-observed settings route")
      assertNativeDenied(activity)
      activity.onActivity { retiredClient.get().doUpdateVisitedHistory(initial.get(), "$origin/next/track/retired-callback", false) }
      assertEquals("Retired WebView must not overwrite the resumed route", "/next/settings/network", ResumeEntry(context).read(ConnectionProfiles(context))?.route)
    }
  }
}
