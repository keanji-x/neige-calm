package io.neigecalm.next

import org.junit.Assert.*
import org.junit.Test

class BundledOriginTest {
  private val origin = BundledOrigin.parse("https://calm.example.com:8443") { false }
  private val policy = BundledSelectionPolicy(setOf("index.html", "assets/main.js", "assets/main.css"))

  @Test fun originRequiresAnExplicitSafeAuthority() {
    assertEquals("https://calm.example.com", BundledOrigin.parse("https://CALM.example.com:443/") { false }.value)
    assertEquals("https://[2001:db8::1]:8443", BundledOrigin.parse("https://[2001:db8::1]:8443") { false }.value)
    for (input in listOf("javascript:alert(1)", "https://user:secret@calm.example.com", "https://calm.example.com:0",
      "https://calm.example.com:70000", "https://calm.example.com/next/", "https://calm.example.com?x=1",
      "https://calm.example.com#ticket", "http://calm.example.com")) {
      assertThrows(IllegalArgumentException::class.java) { BundledOrigin.parse(input) { false } }
    }
    assertEquals("http://192.0.2.1:4140", BundledOrigin.parse("http://192.0.2.1:4140") { it == "192.0.2.1" }.value)
    assertEquals("http://127.0.0.1:4140", BundledOrigin.parse("http://127.0.0.1:4140") { it == "127.0.0.1" }.value)
    assertThrows(IllegalArgumentException::class.java) { BundledOrigin.parse("http://tauri.localhost") { true } }
  }

  @Test fun serverBindingCannotTargetPrivilegedOrLoopbackOrigins() {
    for (value in listOf("https://tauri.localhost", "https://ipc.localhost", "https://asset.localhost", "https://localhost",
      "https://127.0.0.1", "https://0.0.0.0", "https://[::1]", "https://[0:0:0:0:0:0:0:1]", "https://[::ffff:127.0.0.1]",
      "https://0177.0.0.1", "https://0x7f.0.0.1", "https://2130706433")) {
      assertThrows("Accepted " + value, IllegalArgumentException::class.java) { BundledOrigin.parse(value) { true } }
    }
  }

  @Test fun assetsAndMainFrameRoutesUseTheBundledFiles() {
    assertEquals(BundledSelection.File("assets/main.js"), policy.select(origin, origin.value + "/next/assets/main.js?v=1", "GET", false))
    assertEquals(BundledSelection.File("index.html"), policy.select(origin, origin.value + "/next/track/abc", "GET", true))
    assertEquals(BundledSelection.File("index.html"), policy.select(origin, origin.value + "/next/", "GET", true))
    assertEquals(BundledSelection.Error(404), policy.select(origin, origin.value + "/next/track/abc", "GET", false))
  }

  @Test fun originAndBackendRequestsNeverSelectBundledFiles() {
    for (url in listOf("https://other.example.com:8443/next/assets/main.js", "https://calm.example.com/next/assets/main.js",
      "http://calm.example.com:8443/next/assets/main.js", origin.value + "/api/auth/whoami", origin.value + "/api/files/a")) {
      assertEquals(BundledSelection.Network, policy.select(origin, url, "GET", false))
    }
    assertEquals(BundledSelection.Network, policy.select(null, origin.value + "/next/", "GET", true))
  }

  @Test fun missingAssetsAndTraversalDoNotFallBackToNetworkOrSpaHtml() {
    for (path in listOf("/next/assets/missing.js", "/next/assets/main.js.map")) {
      assertEquals(BundledSelection.Error(404), policy.select(origin, origin.value + path, "GET", true))
    }
    for (path in listOf("/next/assets/../index.html", "/next/assets/%2e%2e/index.html", "/next/assets/main%2f.js", "/next/assets/main%5c.js")) {
      assertEquals(BundledSelection.Error(400), policy.select(origin, origin.value + path, "GET", false))
    }
    assertEquals(BundledSelection.Error(405), policy.select(origin, origin.value + "/next/assets/main.js", "POST", false))
    assertThrows(IllegalArgumentException::class.java) { BundledSelectionPolicy(setOf("index.html", "assets/../../secret")) }
  }
}
