package io.neigecalm.next

import androidx.test.core.app.ActivityScenario
import androidx.test.espresso.Espresso.onView
import androidx.test.espresso.assertion.ViewAssertions.matches
import androidx.test.espresso.matcher.ViewMatchers.isDisplayed
import androidx.test.espresso.matcher.ViewMatchers.withText
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assume
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class OldWebViewInstrumentationTest {
  @Test fun unsupportedWebViewShowsANativeUpgradeMessage() {
    Assume.assumeFalse(BundledWebViewSupport.available(InstrumentationRegistry.getInstrumentation().targetContext))
    // Android Test Orchestrator owns the isolated process and Activity lifecycle.
    ActivityScenario.launch(MainActivity::class.java)
    onView(withText("请更新系统网页组件")).check(matches(isDisplayed()))
  }
}
