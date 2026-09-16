package io.neigecalm.next
import org.junit.Test
import org.junit.Assert.*
import java.net.URI
class ResumeEntryTest {
  @Test fun keepsOnlyNonSensitiveExistingPages() {
    assertEquals("/next/track/t-1?panel=cards", ResumeEntry.safeRoute(URI("https://example.test/next/track/t-1?panel=cards")))
    assertEquals("/next/settings/network", ResumeEntry.safeRoute(URI("https://example.test/next/settings/network")))
    assertEquals("/next/", ResumeEntry.safeRoute(URI("https://example.test/next/area/a1/new")))
    assertEquals("/next/track/t-1", ResumeEntry.safeRoute(URI("https://example.test/next/track/t-1?token=secret")))
    for (path in listOf("/mobile/pair#secret", "/api/auth/logout", "/next/track/%2e%2e", "/next/../api/version", "/next/settings/secret"))
      assertNull(path, ResumeEntry.safeRoute(URI("https://example.test$path")))
  }
}
