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
class LauncherConnectionInstrumentationTest {
  @Test fun packagedLoginOpensBrowserThroughRealTauriAndNativeNode() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    val device = UiDevice.getInstance(instrumentation)
    context.startActivity(Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    val button = device.wait(Until.findObject(By.textContains("登录 Tailscale")), 20000)
    assertNotNull("Packaged login button must be visible", button)
    button.click()
    assertTrue("Real packaged login did not open Chrome", device.wait(Until.hasObject(By.pkg("com.android.chrome")), 60000))
  }
}
