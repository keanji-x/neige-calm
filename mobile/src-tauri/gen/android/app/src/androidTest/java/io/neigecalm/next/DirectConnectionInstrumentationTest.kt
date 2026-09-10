package io.neigecalm.next

import android.content.Intent
import android.os.SystemClock
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
