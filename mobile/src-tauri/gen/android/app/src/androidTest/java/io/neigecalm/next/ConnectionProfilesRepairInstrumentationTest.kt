package io.neigecalm.next

import android.content.Context
import android.content.SharedPreferences
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ConnectionProfilesRepairInstrumentationTest {
  private fun repair(corrupt: (SharedPreferences.Editor) -> Unit) {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    val preferences = context.getSharedPreferences("connection-profiles", Context.MODE_PRIVATE)
    assertTrue(preferences.edit().clear().commit())
    val profiles = ConnectionProfiles(context)
    val origin = "https://repair.invalid"
    profiles.save("ip", origin, false)
    val oldId = profiles.profileId()
    val resume = ResumeEntry(context)
    resume.remember(profiles, ConnectionProfiles.parseDirect(origin), "$origin/next/track/old-profile")
    assertNotNull(resume.read(profiles))
    val edit = preferences.edit(); corrupt(edit); assertTrue(edit.commit())
    assertTrue("Corrupt metadata must not be accepted as an existing profile", runCatching { profiles.read() }.isFailure)
    profiles.save("ip", origin, false)
    assertEquals(ConnectionSettings("ip", origin, false, ""), profiles.read())
    assertTrue(profiles.revision() > 0)
    java.util.UUID.fromString(profiles.profileId())
    assertNotEquals("Repair must retire the old profile namespace", oldId, profiles.profileId())
    assertNull("Repair must not revive the old resume pointer", resume.read(profiles))
  }
  @Test fun repairsWrongTypeRevision() = repair { it.putString("config-revision", "broken") }
  @Test fun repairsWrongTypeProfileId() = repair { it.putInt("profile-id", 7) }
  @Test fun repairsNonPositiveRevision() = repair { it.putLong("config-revision", -2) }
  @Test fun repairsMalformedTailnetFieldsThroughSaveAndVerifiedScan() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    check(context.packageName.endsWith(".instrumented"))
    val preferences = context.getSharedPreferences("connection-profiles", Context.MODE_PRIVATE)
    for (scan in listOf(false, true)) for (field in listOf("tailnet-origin", "tailnet-origins", "tailscale-enabled", "ip-origin")) {
      assertTrue(preferences.edit().clear().commit())
      val profiles = ConnectionProfiles(context)
      profiles.selectTailnet("https://old.tail.example")
      val old = profiles.read().tailnetOrigin
      val resume = ResumeEntry(context)
      resume.remember(profiles, BundledOrigin.parse(old) { false }, "$old/next/settings/network")
      assertNotNull(resume.read(profiles))
      assertTrue(preferences.edit().putInt(field, 17).commit())
      assertTrue(runCatching { profiles.read() }.isFailure)
      if (scan) {
        profiles.selectTailnet("https://next.tail.example")
        assertEquals("https://next.tail.example", profiles.read().tailnetOrigin)
      } else {
        profiles.save("ip", "https://direct.example", false)
        assertEquals("https://direct.example", profiles.read().ipOrigin)
      }
      assertNull("$field/$scan revived the old resume entry", resume.read(profiles))
    }
  }
}
