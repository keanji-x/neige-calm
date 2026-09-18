package io.neigecalm.next

import org.junit.Test
import org.junit.Assert.*

class ConnectionAttemptTest {
  private val settings = ConnectionSettings("tailscale", "http://192.168.1.8:4140", true, P2PConnection.ORIGIN, false)

  @Test fun ipIsPreferredEvenWhenTheEditorShowsTailscale() {
    val attempted = mutableListOf<String>()
    val result = ConnectionAttempt.firstAvailable(settings) { attempted.add(it.mode) }
    assertEquals(listOf("ip"), attempted)
    assertEquals("ip", result.route?.mode)
  }
  @Test fun aFailedIpFallsBackOnceToConfiguredTailscale() {
    val attempted = mutableListOf<String>()
    val result = ConnectionAttempt.firstAvailable(settings) {
      attempted.add(it.mode)
      if (it.mode == "ip") throw java.net.SocketTimeoutException("timeout")
    }
    assertEquals(listOf("ip", "tailscale"), attempted)
    assertEquals("tailscale", result.route?.mode)
    assertEquals(1, result.failures.size)
  }
  @Test fun explicitTailnetSelectionNeverProbesReachableDirect() {
    val attempted = mutableListOf<ConnectionRoute>()
    val result = ConnectionAttempt.firstAvailable(settings, settings.tailnetOrigin) { attempted.add(it) }
    assertEquals(listOf(ConnectionRoute("tailscale", settings.tailnetOrigin)), attempted)
    assertEquals(ConnectionRoute("tailscale", settings.tailnetOrigin), result.route)
  }
  @Test fun unreachableExplicitTailnetNeverFallsBackToReachableDirect() {
    val attempted = mutableListOf<ConnectionRoute>()
    val result = ConnectionAttempt.firstAvailable(settings, settings.tailnetOrigin) {
      attempted.add(it)
      if (it.mode == "tailscale") throw java.net.SocketTimeoutException("chosen unavailable")
    }
    assertNull(result.route)
    assertEquals(listOf(ConnectionRoute("tailscale", settings.tailnetOrigin)), attempted)
    assertEquals(listOf(ConnectionFailure("tailscale", "chosen unavailable")), result.failures)
  }
  @Test fun explicitSelectionRejectsUnselectedOrDisabledTargetsBeforeProbe() {
    for ((configured, requested) in listOf(settings to "https://other.tail.example", settings to "",
      settings.copy(tailscaleEnabled = false) to settings.tailnetOrigin)) {
      try {
        ConnectionAttempt.firstAvailable(configured, requested) { fail("Invalid choice cannot probe any route") }
        fail("Invalid choice must be rejected")
      } catch (_: IllegalArgumentException) {}
    }
  }
  @Test fun explicitSelectionCancellationCannotAdvanceToAnotherRoute() {
    val cancellation = ConnectionAttempt.Cancellation()
    val attempted = mutableListOf<ConnectionRoute>()
    try {
      ConnectionAttempt.firstAvailable(settings, settings.tailnetOrigin) {
        attempted.add(it); cancellation.cancel(); cancellation.check()
      }
      fail("Cancelled choice must not complete")
    } catch (_: java.util.concurrent.CancellationException) {}
    assertEquals(listOf(ConnectionRoute("tailscale", settings.tailnetOrigin)), attempted)
  }
  @Test fun exhaustionStopsAndMissingOptionsAreNotTried() {
    var attempts = 0
    val result = ConnectionAttempt.firstAvailable(settings) { attempts++; throw java.net.SocketTimeoutException("timeout") }
    assertNull(result.route); assertEquals(2, attempts); assertEquals(2, result.failures.size)
    val empty = ConnectionAttempt.firstAvailable(ConnectionSettings("ip", "", false, "", false)) { fail("No configured route") }
    assertNull(empty.route); assertTrue(empty.failures.isEmpty())
  }
  @Test fun cancellationStopsBeforeStartingAnotherRoute() {
    val cancellation = ConnectionAttempt.Cancellation()
    cancellation.cancel()
    var checks = 0
    try {
      ConnectionAttempt.firstAvailable(settings) { cancellation.check(); checks++ }
      fail("Cancelled work must not continue")
    } catch (_: java.util.concurrent.CancellationException) {}
    assertEquals(0, checks)
  }
  @Test fun addressesAreExplicitAndCannotTargetTheLauncherOrMetadata() {
    assertEquals("http://192.168.1.8:4140", ConnectionProfiles.parseDirect("192.168.1.8:4140/next/").value)
    assertEquals("http://203.0.113.5:4140", ConnectionProfiles.parseDirect("http://203.0.113.5:4140").value)
    assertEquals("https://calm.example.com", ConnectionProfiles.parseDirect("https://calm.example.com").value)
    for (value in listOf("http://127.0.0.1:4140", "http://tauri.localhost", "http://169.254.169.254", "http://example.com", "http://192.168.1.8@evil.example", "http://192.168.1.8:4140/?token=secret")) {
      try { ConnectionProfiles.parseDirect(value); fail(value) } catch (_: IllegalArgumentException) {} catch (_: java.net.URISyntaxException) {}
    }
  }
  @Test fun cleartextIsLimitedToTheSelectedOrigin() {
    val active = ConnectionProfiles.parseDirect("http://192.168.1.8:4140")
    assertTrue(HttpOriginFence.permits("http://192.168.1.8:4140/api/version", active))
    assertFalse(HttpOriginFence.permits("http://192.168.1.8:4141/api/version", active))
    assertFalse(HttpOriginFence.permits("http://203.0.113.5:4140/api/version", active))
    assertFalse(HttpOriginFence.permits("http://192.168.1.8:4140/api/version", null))
  }
}
