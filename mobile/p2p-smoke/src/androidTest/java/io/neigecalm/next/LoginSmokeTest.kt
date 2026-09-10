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
class LoginSmokeTest {
  @Test fun actualLoginButtonOpensBrowserUsingRealNativeEnrollment() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    val device = UiDevice.getInstance(instrumentation)
    context.startActivity(Intent(context, ConnectionTrialActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    val button = device.wait(Until.findObject(By.textContains("重新获取授权")), 15000)
    assertNotNull("Login button absent", button)
    button.click()
    val browser = device.wait(Until.hasObject(By.pkg("com.android.chrome")), 60000)
    if (!browser) {
      // UI text only: never print successful enrollment URLs or private keys.
      val ui = device.findObjects(By.clazz("android.widget.TextView")).joinToString(" | ") { it.text }
      fail("Browser did not open after the actual tap. UI: $ui")
    }
    assertTrue(browser)
  }
}
