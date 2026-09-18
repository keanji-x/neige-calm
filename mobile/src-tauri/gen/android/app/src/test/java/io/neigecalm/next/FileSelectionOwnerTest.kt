package io.neigecalm.next

import org.junit.Assert.*
import org.junit.Test

class FileSelectionOwnerTest {
  @Test fun selectsOnceAndReleasesLauncher() {
    val owner = FileSelectionOwner<String> { true }
    val values = mutableListOf<String?>(); var release = 0
    lateinit var result: (String?) -> Unit
    owner.begin({ values.add(it) }) { result = it; { release++ } }
    result("selected"); result("late")
    assertEquals(listOf("selected"), values); assertEquals(1,release)
  }
  @Test fun disposeCancelsCallbackAndRejectsLateResultsAndNewRequests() {
    val owner = FileSelectionOwner<String> { true }
    val values = mutableListOf<String?>(); var release = 0
    lateinit var result: (String?) -> Unit
    owner.begin({ values.add(it) }) { result = it; { release++ } }
    owner.dispose()
    assertEquals("Disposal must settle before any Android result",listOf<String?>(null),values)
    assertEquals(1,release)
    result("private-file")
    owner.begin({ values.add(it) }) { error("closed document launched chooser") }
    assertEquals(listOf(null,null),values); assertEquals(1,release)
  }
  @Test fun replacementAndDocumentChangeCannotAdoptOldResults() {
    var current = true
    val owner = FileSelectionOwner<String> { current }
    val values = mutableListOf<String?>()
    lateinit var old: (String?) -> Unit; lateinit var fresh: (String?) -> Unit
    owner.begin({ values.add(it) }) { old = it; {} }
    owner.begin({ values.add(it) }) { fresh = it; {} }
    old("stale"); current = false; fresh("wrong-origin")
    assertEquals(listOf(null,null),values)
    current = true
    owner.begin({ values.add(it) }) { fresh = it; {} }
    owner.cancel(); fresh("retired-document")
    assertEquals(listOf(null,null,null),values)
  }
  @Test fun synchronousResultAndLaunchFailureStillSettleOnce() {
    val owner = FileSelectionOwner<String> { true }
    val values = mutableListOf<String?>(); var released = 0
    owner.begin({ values.add(it) }) { it("immediate"); { released++ } }
    owner.begin({ values.add(it) }) { throw IllegalStateException("no chooser") }
    assertEquals(listOf("immediate",null),values); assertEquals(1,released)
  }
}
