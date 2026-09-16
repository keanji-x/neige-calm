package io.neigecalm.next

import android.content.Context
import android.os.Bundle
import android.os.Process
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** Two explicit stages driven across a real force-stop; the check stage never reseeds state. */
@RunWith(AndroidJUnit4::class)
class RecoveryProcessInstrumentationTest {
  private val harness = RecoveryInstrumentationTest()
  private fun reportPid() {
    InstrumentationRegistry.getInstrumentation().sendStatus(2, Bundle().also {
      it.putInt("recoveryPid", Process.myPid())
    })
  }
  @Test fun seedSavedRoute() {
    harness.prepare("/next/track/process-seed")
    harness.withLiveActivity { activity ->
      harness.await(activity, "document.querySelector('[data-nc-recovery-page=track]') !== null", "Seed did not open local Track")
      harness.evaluate(activity, "history.pushState({}, '', '/next/settings/network'); true")
      val context = InstrumentationRegistry.getInstrumentation().targetContext
      val profiles = ConnectionProfiles(context)
      val until = SystemClock.elapsedRealtime() + 5000
      while (ResumeEntry(context).read(profiles)?.route != "/next/settings/network" && SystemClock.elapsedRealtime() < until) SystemClock.sleep(100)
      assertEquals("/next/settings/network", ResumeEntry(context).read(profiles)?.route)
      // Flush the real observer's queued preference write before the driver
      // kills the process. Do not manufacture or replace the resume record.
      assertTrue(context.getSharedPreferences("workspace-resume", Context.MODE_PRIVATE).edit().commit())
      assertTrue(context.getSharedPreferences("recovery-process-test", Context.MODE_PRIVATE).edit()
        .putInt("seed-pid", Process.myPid()).putString("profile-id", profiles.profileId())
        .putLong("revision", profiles.revision()).commit())
      reportPid()
    }
  }
  @Test fun checkSavedRouteAfterProcessDeath() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    val evidence = context.getSharedPreferences("recovery-process-test", Context.MODE_PRIVATE)
    val seedPid = evidence.getInt("seed-pid", -1)
    assertTrue("Run seedSavedRoute through the external driver first", seedPid > 0)
    assertNotEquals("The external driver must kill the seed process", seedPid, Process.myPid())
    val profiles = ConnectionProfiles(context)
    assertEquals(evidence.getString("profile-id", null), profiles.profileId())
    assertEquals(evidence.getLong("revision", -1), profiles.revision())
    assertEquals("/next/settings/network", ResumeEntry(context).read(profiles)?.route)
    harness.withLiveActivity { activity ->
      harness.await(activity, "location.pathname === '/next/settings/network' && document.querySelector('[data-nc-recovery-page=settings]') !== null && document.querySelector('[data-nc-recovery-status]') !== null",
        "A new process did not restore the history-observed Settings page offline")
      assertEquals("true", harness.evaluate(activity, "!document.body.innerText.includes('已连接')"))
      harness.assertNativeDenied(activity)
      reportPid()
    }
  }
}
