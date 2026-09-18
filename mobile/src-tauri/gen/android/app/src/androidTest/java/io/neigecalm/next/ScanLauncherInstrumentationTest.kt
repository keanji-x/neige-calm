package io.neigecalm.next

import android.content.Intent
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import org.junit.Test
import org.junit.Assert.*
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ScanLauncherInstrumentationTest {
  @Test fun packagedLauncherOffersScanningWithoutInteractiveLogin() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    check(context.packageName.endsWith(".instrumented"))
    val device = UiDevice.getInstance(instrumentation)
    context.startActivity(Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    assertNotNull("Fresh launcher must offer scanning", device.wait(Until.findObject(By.textContains("扫码授权").enabled(true)), 20000))
    assertFalse("Interactive Tailscale login must not be offered", device.hasObject(By.textContains("登录 Tailscale")))
  }
}
