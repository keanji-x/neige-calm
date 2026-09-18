package io.neigecalm.next

import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.lang.ref.WeakReference
import java.net.Inet4Address
import java.net.NetworkInterface
import java.net.ServerSocket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Future
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Real launcher -> plugin -> admitted JNI -> HTTP proof -> profile owner.
 * Only the version peer's response is held. No manual Go revocation and no
 * replacement implementation of native persistence or dispatch is used.
 */
@RunWith(AndroidJUnit4::class)
class DirectConfirmationEditInstrumentationTest {
  private fun field(owner: Any, name: String) = owner.javaClass.getDeclaredField(name).also { it.isAccessible = true }
  private fun plugin(): BundledFrontendPlugin {
    val active = BundledFrontendPlugin::class.java.getDeclaredField("active").also { it.isAccessible = true }
    return (active.get(null) as WeakReference<*>).get() as BundledFrontendPlugin
  }
  private class HeldVersionPeer : AutoCloseable {
    private val address = NetworkInterface.getNetworkInterfaces().toList().flatMap { it.inetAddresses.toList() }
      .filterIsInstance<Inet4Address>().firstOrNull { !it.isLoopbackAddress && !it.isLinkLocalAddress && !it.isAnyLocalAddress }
      ?: error("Fixture requires the device's own ordinary LAN IPv4 address")
    private val server = ServerSocket(0, 1, address)
    val origin = "http://${address.hostAddress}:${server.localPort}"
    val entered = CountDownLatch(1)
    val release = CountDownLatch(1)
    val finished = CountDownLatch(1)
    @Volatile var enteredAt = 0L
    @Volatile var failure: Throwable? = null
    private val worker = Thread {
      try {
        server.soTimeout = 10000
        server.accept().use { peer ->
          peer.soTimeout = 5000
          val reader = peer.getInputStream().bufferedReader()
          check(reader.readLine() == "GET /api/version HTTP/1.1")
          val headers = mutableListOf<String>()
          while (true) { val line = reader.readLine() ?: error("Incomplete request"); if (line.isEmpty()) break; headers.add(line) }
          check(headers.none { it.startsWith("Cookie:", ignoreCase = true) })
          enteredAt = SystemClock.elapsedRealtime(); entered.countDown()
          check(release.await(10, TimeUnit.SECONDS)) { "Fixture response was not released" }
          val body = """{"webCompatVersion":30,"apiVersion":"9","kernelVersion":"held-proof-fixture"}"""
          // Edit cancellation may already have closed the real native socket.
          runCatching { peer.getOutputStream().write(("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${body.toByteArray().size}\r\nConnection: close\r\n\r\n$body").toByteArray()) }
        }
      } catch (error: Throwable) { failure = error; entered.countDown() }
      finally { finished.countDown() }
    }.also { it.start() }
    fun awaitEntered() { assertTrue("Native proof never reached the local version peer", entered.await(10, TimeUnit.SECONDS)); failure?.let { throw AssertionError("Version fixture failed", it) } }
    override fun close() { release.countDown(); server.close(); worker.join(11000) }
  }

  @Test fun editingHeldDirectProofRevokesOnlyItsNativeOwnerAndPreservesSavedState() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    val preferences = context.getSharedPreferences("connection-profiles",0)
    val resumePreferences = context.getSharedPreferences("workspace-resume",0)
    check(preferences.edit().clear().commit()); check(resumePreferences.edit().clear().commit())
    val scenario = ScanFixtureActivity.acquire("direct-edit")
    val helper = RecoveryInstrumentationTest()
    HeldVersionPeer().use { first -> HeldVersionPeer().use { newer ->
      try {
        helper.await(scenario,"location.host==='tauri.localhost' && !document.querySelector('#connection-mode').disabled","Launcher unavailable")
        helper.evaluate(scenario,"document.querySelector('#connection-mode').value='ip'; document.querySelector('#connection-mode').dispatchEvent(new Event('change')); true")
        ScanFixtureActivity.awaitUi(scenario,"direct-edit.ip-mode") { ConnectionProfiles(context).read().mode == "ip" }
        helper.evaluate(scenario,"window.directPrepared=false; window.__TAURI__.core.invoke('plugin:bundled-frontend|save_connection',{mode:'ip',ipOrigin:${JSONObject.quote(first.origin)},tailscaleEnabled:false}).then(()=>directPrepared=true); true")
        helper.await(scenario,"window.directPrepared===true","Saved fixture origin unavailable")
        var before = emptyMap<String, Any?>()
        var resumeBefore: String? = null
        scenario.onActivity {
          val profiles = ConnectionProfiles(context)
          ResumeEntry(context).remember(profiles, ConnectionProfiles.parseDirect(first.origin), first.origin+"/next/track/retained-fixture")
          before = preferences.all.toMap(); resumeBefore = resumePreferences.getString("entry",null)
          assertNotNull(ResumeEntry(context).read(profiles))
        }
        helper.evaluate(scenario,"document.querySelector('#ip-origin').value=${JSONObject.quote(first.origin)}; document.querySelector('#login').click(); true")
        first.awaitEntered()
        lateinit var old: Any
        lateinit var owner: BundledFrontendPlugin
        var oldIntent = ""
        scenario.onActivity {
          owner = plugin(); old = checkNotNull(field(owner,"pending").get(owner))
          oldIntent = checkNotNull(field(old,"connectionIntent").get(old)) as String
          assertFalse((field(old,"settled").get(old) as AtomicBoolean).get())
        }
        helper.evaluate(scenario,"document.querySelector('#ip-origin').value='https://unsaved-draft.example'; document.querySelector('#ip-origin').dispatchEvent(new Event('input')); window.editBarrier=false; window.__TAURI__.core.invoke('plugin:bundled-frontend|connection_settings').then(()=>editBarrier=true); true")
        helper.await(scenario,"window.editBarrier===true","Read-only native edit barrier did not complete")
        assertTrue("Proof expired before the edit/release race; this is not a cancellation pass", SystemClock.elapsedRealtime()-first.enteredAt < 3500)
        first.release.countDown()
        assertTrue(first.finished.await(5,TimeUnit.SECONDS))
        ScanFixtureActivity.awaitUi(scenario,"direct-edit.old-settled") {
          (field(old,"settled").get(old) as AtomicBoolean).get() && (field(old,"future").get(old) as Future<*>).isDone
        }
        scenario.onActivity { host ->
          assertEquals("Edited-away proof changed the saved binding/revision",before,preferences.all)
          assertEquals(resumeBefore,resumePreferences.getString("entry",null))
          assertNotNull(ResumeEntry(context).read(ConnectionProfiles(context)))
          ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("direct-edit.preserved",host))
        }
        assertEquals("https://unsaved-draft.example", org.json.JSONTokener(helper.evaluate(scenario,"document.querySelector('#ip-origin').value")).nextValue())

        // A stale cancellation must not revoke a newer native proof. The new
        // proof is also the positive control: it really persists when released.
        helper.evaluate(scenario,"window.newerProof=null; (async()=>{const call=window.__TAURI__.core.invoke; await call('plugin:bundled-frontend|save_connection',{mode:'ip',ipOrigin:${JSONObject.quote(newer.origin)},tailscaleEnabled:false}); window.newerProof=await call('plugin:bundled-frontend|attempt_connection',{confirmDirect:true,intentId:'newer-proof'});})().catch(e=>newerProof={error:String(e)}); true")
        newer.awaitEntered()
        lateinit var next: Any
        scenario.onActivity { next = checkNotNull(field(owner,"pending").get(owner)); assertNotSame(old,next) }
        helper.evaluate(scenario,"window.oldCancelAck=false; window.__TAURI__.core.invoke('plugin:bundled-frontend|cancel_connection',{intentId:${JSONObject.quote(oldIntent)}}).then(()=>oldCancelAck=true); true")
        helper.await(scenario,"window.oldCancelAck===true","Stale cancellation was not acknowledged")
        scenario.onActivity { assertSame(next,field(owner,"pending").get(owner)); assertFalse((field(next,"settled").get(next) as AtomicBoolean).get()) }
        assertTrue("New proof expired before release",SystemClock.elapsedRealtime()-newer.enteredAt < 3500)
        newer.release.countDown()
        helper.await(scenario,"window.newerProof?.connected===true","Uncancelled newer proof did not succeed")
        scenario.onActivity { host ->
          assertTrue(ConnectionProfiles(context).directBinding(newer.origin).isNotEmpty())
          ScanFixtureActivity.emit(ScanFixtureActivity.snapshot("direct-edit.newer-proof-persisted",host))
        }
      } finally {
        first.release.countDown(); newer.release.countDown()
        scenario.onActivity { ConnectionProfiles(context).disableTailnet(); plugin().onPause() }
        ScanFixtureActivity.pause(scenario,"direct-edit")
      }
    } }
  }
}
