package io.neigecalm.next

import android.webkit.CookieManager
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.Assert.*
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(AndroidJUnit4::class)
class RememberedSessionInstrumentationTest {
  @Test fun retainsTheCookieAcrossARealProcessRestart() {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val direct = "http://203.0.113.9:4140"
    val profiles = ConnectionProfiles(instrumentation.targetContext)
    val phase = InstrumentationRegistry.getArguments().getString("phase")
    if (phase == "seed") {
      profiles.save("ip", direct, true)
      val done = CountDownLatch(1)
      instrumentation.runOnMainSync {
        CookieManager.getInstance().setCookie(P2PConnection.ORIGIN,
          "calm-session=phone_test_session_1234567890; Path=/; Secure; HttpOnly; SameSite=Strict") { saved ->
          assertTrue(saved)
          RememberedSession.persist { persisted ->
            assertTrue(persisted)
            CookieManager.getInstance().setCookie(direct, "calm-session=ip_test_session_1234567890; Path=/; HttpOnly; SameSite=Strict") {
              RememberedSession.persist(direct) { savedIp -> assertTrue(savedIp); done.countDown() }
            }
          }
        }
      }
      assertTrue(done.await(10, TimeUnit.SECONDS))
    } else {
      assertEquals("check", phase)
      assertEquals(ConnectionSettings("ip", direct, true), profiles.read())
      instrumentation.runOnMainSync {
        assertTrue("Saved authorization was lost after force-stop", RememberedSession.hasCookie())
        assertTrue("IP authorization was lost after force-stop", RememberedSession.hasCookie(direct))
        assertEquals("calm-session=phone_test_session_1234567890", CookieManager.getInstance().getCookie(P2PConnection.ORIGIN))
        assertEquals("calm-session=ip_test_session_1234567890", CookieManager.getInstance().getCookie(direct))
      }
    }
  }
}
