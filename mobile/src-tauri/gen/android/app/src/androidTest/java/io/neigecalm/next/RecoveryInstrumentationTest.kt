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
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/** Device-independent: no emulator gateway, external server, VPN or network toggle. */
@RunWith(AndroidJUnit4::class)
class RecoveryInstrumentationTest {
  private val origin = "https://recovery.invalid"
  private fun webView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (i in 0 until view.childCount) webView(view.getChildAt(i))?.let { return it }
    return null
  }
  private fun prepare(route: String) {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented")) { "Recovery tests must use the isolated instrumented APK" }
    val profiles = ConnectionProfiles(context)
    profiles.save("ip", origin, false)
    ResumeEntry(context).remember(profiles, ConnectionProfiles.parseDirect(origin), origin + route)
  }
  private fun evaluate(activity: ActivityScenario<MainActivity>, script: String): String {
    val value = AtomicReference<String>("")
    val done = CountDownLatch(1)
    activity.onActivity { host ->
      val view = webView(host.findViewById(android.R.id.content))
      if (view == null) done.countDown() else view.evaluateJavascript(script) { value.set(it); done.countDown() }
    }
    assertTrue(done.await(3, TimeUnit.SECONDS))
    return value.get()
  }
  private fun await(activity: ActivityScenario<MainActivity>, script: String, message: String) {
    val until = SystemClock.elapsedRealtime() + 10000
    while (SystemClock.elapsedRealtime() < until) {
      if (evaluate(activity, script) == "true") return
      SystemClock.sleep(100)
    }
    fail(message)
  }
  @Test fun offlineColdEntryShowsSavedTrackAndCornerStatusBeforeNetworkIsReady() {
    prepare("/next/track/saved-track?panel=cards")
    ActivityScenario.launch(MainActivity::class.java).use { activity ->
      await(activity, "location.pathname === '/next/track/saved-track' && document.querySelector('[data-nc-recovery-page=track]') !== null && document.querySelector('[data-nc-recovery-status]') !== null",
        "Offline cold start waited for network instead of painting the saved local Track")
      assertEquals("true", evaluate(activity, "document.body.innerText.includes('等待恢复终端') && !document.body.innerText.includes('已连接')"))
      assertEquals("true", evaluate(activity, "document.querySelector('[data-nc-recovery-status]').getBoundingClientRect().right <= innerWidth"))
    }
  }
  @Test fun historySurvivesColdRecreationAndHotResumeKeepsTheSameWebView() {
    prepare("/next/track/saved-track")
    ActivityScenario.launch(MainActivity::class.java).use { activity ->
      await(activity, "document.querySelector('[data-nc-recovery-page=track]') !== null", "Missing saved local page")
      val initial = AtomicReference<WebView>()
      activity.onActivity { initial.set(webView(it.findViewById(android.R.id.content))) }
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
    }
  }
}
