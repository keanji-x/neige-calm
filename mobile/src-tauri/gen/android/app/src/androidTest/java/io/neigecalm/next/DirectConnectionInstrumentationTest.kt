package io.neigecalm.next

import android.content.Intent
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.test.core.app.ActivityScenario
import org.json.JSONObject
import java.net.URL
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import org.junit.Test
import org.junit.Assert.*
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class DirectConnectionInstrumentationTest {
  @Test fun directIpLoadsTheBundledAppWithoutATailscaleIdentity() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    ConnectionProfiles(context).save("ip", "http://10.0.2.2:5413", false)
    val device = UiDevice.getInstance(instrumentation)
    context.startActivity(Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    assertTrue("IP route did not reach the bundled workspace login", device.wait(Until.hasObject(By.textContains("扫码连接你的工作区")), 25000))
    val back = device.wait(Until.findObject(By.textContains("返回连接页")), 10000)
    assertNotNull(back); back.click()
    assertTrue(device.wait(Until.hasObject(By.textContains("保存并连接 IP")), 10000))
    SystemClock.sleep(2000)
    assertTrue("Returning to setup must not immediately reopen IP", device.hasObject(By.textContains("保存并连接 IP")))
  }

  private fun webView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (index in 0 until view.childCount) webView(view.getChildAt(index))?.let { return it }
    return null
  }

  @Test fun subresourceRedirectCannotEscapeTheConfiguredHttpOrigin() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    ConnectionProfiles(context).save("ip", "http://10.0.2.2:5413", false)
    URL("http://10.0.2.2:5413/reset").readText()
    val activity = ActivityScenario.launch(MainActivity::class.java)
    val device = UiDevice.getInstance(InstrumentationRegistry.getInstrumentation())
    assertTrue(device.wait(Until.hasObject(By.textContains("扫码连接你的工作区")), 25000))
    activity.onActivity { webView(it.findViewById(android.R.id.content))!!.loadUrl("http://10.0.2.2:5413/redirect-document") }
    assertTrue(device.wait(Until.hasObject(By.text("Redirect test")), 10000))
    var stats = JSONObject(URL("http://10.0.2.2:5413/stats").readText())
    val deadline = SystemClock.elapsedRealtime() + 8000
    while (stats.getInt("initial") == 0 && SystemClock.elapsedRealtime() < deadline) {
      SystemClock.sleep(100); stats = JSONObject(URL("http://10.0.2.2:5413/stats").readText())
    }
    assertTrue("Initial image request must reach the allowed origin", stats.getInt("initial") > 0)
    SystemClock.sleep(1000)
    stats = JSONObject(URL("http://10.0.2.2:5413/stats").readText())
    assertEquals("A redirect reached an unconfigured HTTP origin", 0, stats.getInt("sink"))
  }

  @Test fun obsoleteRequestsDoNotQueueAheadOfTheLatestConfiguration() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    ConnectionProfiles(instrumentation.targetContext).save("ip", "", false)
    val activity = ActivityScenario.launch(MainActivity::class.java)
    val device = UiDevice.getInstance(instrumentation)
    assertTrue(device.wait(Until.hasObject(By.textContains("保存并连接 IP")), 15000))
    fun evaluate(script: String): String {
      val result = AtomicReference<String>()
      val done = CountDownLatch(1)
      activity.onActivity { webView(it.findViewById(android.R.id.content))!!.evaluateJavascript(script) { value -> result.set(value); done.countDown() } }
      assertTrue(done.await(5, TimeUnit.SECONDS)); return result.get()
    }
    val start = SystemClock.elapsedRealtime()
    evaluate("""window.routeResult=null;(async()=>{
      const call=(name,args)=>window.__TAURI__.core.invoke('plugin:bundled-frontend|'+name,args);
      await call('save_connection',{mode:'ip',ipOrigin:'http://10.0.2.2:5414',tailscaleEnabled:false});
      for(let n=0;n<7;n++){call('attempt_connection').catch(()=>{});await new Promise(r=>setTimeout(r,60));}
      await call('save_connection',{mode:'ip',ipOrigin:'http://10.0.2.2:5413',tailscaleEnabled:false});
      window.routeResult=await call('attempt_connection');
    })().catch(e=>{window.routeResult={error:String(e)}});""")
    val deadline = start + 7000
    while (evaluate("window.routeResult!==null") != "true" && SystemClock.elapsedRealtime() < deadline) SystemClock.sleep(100)
    assertEquals("Latest request waited behind obsolete timeouts", "true", evaluate("window.routeResult?.connected===true && window.routeResult?.origin==='http://10.0.2.2:5413'"))
    assertTrue(SystemClock.elapsedRealtime() - start < 7000)
  }

  @Test fun directProbesRespectTimeoutAndDoNotFollowRedirects() {
    ConnectionAttempt.checkDirect("http://10.0.2.2:5413")
    val start = SystemClock.elapsedRealtime()
    try { ConnectionAttempt.checkDirect("http://10.0.2.2:5414"); fail("Silent peer must time out") }
    catch (_: java.io.IOException) {} catch (_: IllegalStateException) {}
    assertTrue("Direct timeout exceeded its budget", SystemClock.elapsedRealtime() - start < 6500)
    try { ConnectionAttempt.checkDirect("http://10.0.2.2:5415"); fail("Redirect must be rejected") }
    catch (_: IllegalStateException) {}
  }
}
