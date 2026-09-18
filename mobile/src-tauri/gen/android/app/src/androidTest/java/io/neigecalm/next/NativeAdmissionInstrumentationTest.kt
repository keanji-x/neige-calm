package io.neigecalm.next

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.lang.ref.WeakReference
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.ExecutorService
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit

/** Real commands and JNI admission, no node startup, camera or external app.
 * Only dispatch is parked at the real pre-worker boundary to force the race.
 */
@RunWith(AndroidJUnit4::class)
class NativeAdmissionInstrumentationTest {
  private class Parked : AbstractExecutorService() {
    private var stopped = false
    override fun execute(command: Runnable) { check(!stopped) }
    override fun shutdown() { stopped = true }
    override fun shutdownNow(): MutableList<Runnable> { stopped = true; return mutableListOf() }
    override fun isShutdown() = stopped
    override fun isTerminated() = stopped
    override fun awaitTermination(timeout: Long, unit: TimeUnit) = stopped
  }
  private class Deadlines : ScheduledThreadPoolExecutor(1) {
    var pending: Runnable? = null
    override fun schedule(command: Runnable, delay: Long, unit: TimeUnit): java.util.concurrent.ScheduledFuture<*> {
      pending = command
      return super.schedule(command, 1, TimeUnit.DAYS)
    }
  }
  private fun field(owner: Any, name: String) = owner.javaClass.getDeclaredField(name).also { it.isAccessible = true }
  private fun plugin(): BundledFrontendPlugin {
    val field = BundledFrontendPlugin::class.java.getDeclaredField("active").also { it.isAccessible = true }
    return (field.get(null) as WeakReference<*>).get() as BundledFrontendPlugin
  }
  @Test fun everySupersedingPathRevokesQueuedNativeAdmissionBeforeWorkerDispatch() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    assertTrue(context.getSharedPreferences("connection-profiles",0).edit().clear().commit())
    assertTrue(context.getSharedPreferences("workspace-resume",0).edit().clear().commit())
    val helper = RecoveryInstrumentationTest()
    val scenario = ScanFixtureActivity.acquire("native-admission")
    val parked = Parked(); val deadlines = Deadlines()
    lateinit var plugin: BundledFrontendPlugin
    lateinit var originalNetwork: ExecutorService
    lateinit var originalDeadlines: ScheduledExecutorService
    lateinit var originalView: android.webkit.WebView
    var initialized = false
    var path = "setup"
    val passed = JSONArray()
    try {
      helper.await(scenario, "location.host==='tauri.localhost' && !document.querySelector('#connection-mode').disabled", "Launcher unavailable")
      scenario.onActivity { host ->
        // The fixture temporarily enables a saved profile to exercise bind
        // commands; unrelated platform callbacks must not wake a real node.
        val manager = field(host,"networks").get(host) as? android.net.ConnectivityManager
        val callback = field(host,"networkCallback").get(host) as android.net.ConnectivityManager.NetworkCallback
        manager?.unregisterNetworkCallback(callback)
        field(host,"networks").set(host,null)
        P2PConnection.pause()
        plugin = plugin()
        originalView = field(plugin,"view").get(plugin) as android.webkit.WebView
        originalNetwork = field(plugin,"network").get(plugin) as ExecutorService
        originalDeadlines = field(plugin,"deadlines").get(plugin) as ScheduledExecutorService
        field(plugin,"network").set(plugin,parked)
        field(plugin,"deadlines").set(plugin,deadlines)
        initialized = true
        ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("admission.parked",host))
      }
      for (nextPath in listOf("save", "select", "attempt", "bind", "cancel", "pause", "timeout", "new-scan", "reset", "legacy", "attach", "destroy")) {
        path = nextPath
        scenario.onActivity { host ->
          val profiles=ConnectionProfiles(context)
          profiles.selectTailnet(if (path=="legacy") P2PConnection.ORIGIN else "https://saved.tail.example")
          if (path=="select") profiles.selectTailnet("https://other-saved.tail.example")
          ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("admission.$path.before-reserve",host).put("passed",passed))
        }
        helper.evaluate(scenario, "window.s4Pending=null; window.__TAURI__.core.invoke('plugin:bundled-frontend|enroll_from_scan',{payload:'neige-enroll:v2:parked'}).then(()=>s4Pending='ok',()=>s4Pending='cancelled'); true")
        var token = ""
        lateinit var oldPending: Any
        ScanFixtureActivity.awaitUi(scenario,"admission.$path.reserved") {
          val pending=field(plugin,"pending").get(plugin)
          pending!=null && field(pending,"nativeOperation").get(pending)!=null
        }
        scenario.onActivity {
          val pending = checkNotNull(field(plugin,"pending").get(plugin))
          oldPending = pending
          val operation = checkNotNull(field(pending,"nativeOperation").get(pending))
          token = field(operation,"token").get(operation) as String
        }
        val command = when (path) {
          "save" -> "save_connection',{mode:'ip',ipOrigin:'https://direct.invalid',tailscaleEnabled:false}"
          "select" -> "select_saved_tailnet',{origin:'https://saved.tail.example'}"
          "attempt" -> "attempt_connection',{}"
          "bind" -> "bind_server',{origin:'https://saved.tail.example'}"
          "cancel" -> "cancel_enrollment',{}"
          "new-scan" -> "enroll_from_scan',{payload:'neige-enroll:v2:replacement'}"
          "reset" -> "reset_enrollment',{}"
          "legacy" -> "confirm_legacy_tailnet',{origin:'${P2PConnection.ORIGIN}'}"
          else -> null
        }
        if (command != null) helper.evaluate(scenario, "window.__TAURI__.core.invoke('plugin:bundled-frontend|$command).catch(()=>{}); true")
        else scenario.onActivity { host ->
          when(path) {
            "pause" -> plugin.onPause()
            "timeout" -> checkNotNull(deadlines.pending).run()
            "attach" -> {
              val original = field(plugin,"view").get(plugin) as android.webkit.WebView
              val replacement = android.webkit.WebView(host)
              BundledFrontendPlugin.attachActivity(host,replacement)
              BundledFrontendPlugin.attachActivity(host,original)
              replacement.destroy()
            }
            "destroy" -> plugin.onDestroy(host)
          }
        }
        if (path=="reset") {
          scenario.onActivity { host ->
            assertSame("Reset must not bypass native confirmation",oldPending,field(plugin,"pending").get(plugin))
            ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("reset.before-confirmation-wait",host))
          }
          ScanFixtureActivity.confirmOwnedResetDialog(scenario)
        }
        ScanFixtureActivity.awaitUi(scenario,"admission.$path.dispatched") {
          val settled=(field(oldPending,"settled").get(oldPending) as java.util.concurrent.atomic.AtomicBoolean).get()
          val current=field(plugin,"pending").get(plugin)
          settled && when(path) {
            "save" -> current==null && ConnectionProfiles(context).read().let { settings -> settings.mode=="ip" && settings.ipOrigin=="https://direct.invalid" && !settings.tailscaleEnabled }
            "select" -> current==null && ConnectionProfiles(context).read().tailnetOrigin=="https://saved.tail.example"
            "new-scan", "reset", "legacy" -> current!=null && current!==oldPending && field(current,"nativeOperation").get(current)?.let { operation -> field(operation,"token").get(operation)!=token }==true
            "attempt", "bind" -> current!=null && current!==oldPending
            else -> current==null
          }
        }
        scenario.onActivity {
          // Real JNI checks the old ticket before current()/node startup or QR decoding.
          val result = JSONObject(NativeP2P.enroll(token,"unused"))
          assertFalse("$path admitted old JNI", result.getBoolean("ok"))
          assertEquals("$path did not revoke native admission", "原生操作已取消", result.getString("error"))
        }
        passed.put(path)
        scenario.onActivity { host -> ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("admission.$path.passed",host).put("passed",passed)) }
        if (path!="destroy") {
          helper.evaluate(scenario,"window.s4Cleanup=false; window.__TAURI__.core.invoke('plugin:bundled-frontend|cancel_enrollment').then(()=>s4Cleanup=true,()=>s4Cleanup=false); true")
          helper.await(scenario,"window.s4Cleanup===true","Cleanup command did not acknowledge: $path")
          ScanFixtureActivity.awaitUi(scenario,"admission.$path.cleaned") { field(plugin,"pending").get(plugin)==null }
        }
      }
      assertEquals("Every superseding path must reach its own JNI assertion",12,passed.length())
    } catch (error: Throwable) {
      scenario.onActivity { host -> ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("admission.$path.failed",host).put("passed",passed).put("error",error.javaClass.simpleName)) }
      throw error
    } finally {
      scenario.onActivity { host ->
        // Disable wake before lifecycle cleanup; this test never starts tsnet.
        ConnectionProfiles(context).disableTailnet()
        ScanFixtureActivity.cancelOwnedResetDialog(host)
        if (initialized) {
          field(plugin,"network").set(plugin,originalNetwork)
          field(plugin,"deadlines").set(plugin,originalDeadlines)
          BundledFrontendPlugin.attachActivity(host,originalView)
        }
        NativeP2P.cancelEnrollment()
      }
      parked.shutdownNow(); deadlines.shutdownNow()
      ScanFixtureActivity.pause(scenario,"native-admission")
    }
  }
}
