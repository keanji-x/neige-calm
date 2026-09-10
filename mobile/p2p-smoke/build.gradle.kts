plugins {
  id("com.android.application") version "8.11.0"
  id("org.jetbrains.kotlin.android") version "1.9.25"
}
android {
  namespace = "io.neigecalm.next"
  compileSdk = 36
  defaultConfig {
    applicationId = "io.neigecalm.next.loginsmoke"
    minSdk = 26
    targetSdk = 36
    testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
  }
  sourceSets.getByName("main") {
    java.srcDir("../src-tauri/gen/android/app/src/main/java")
    java.include("**/NativeP2P.kt", "**/ConnectionTrialActivity.kt", "**/AndroidNetworkSnapshot.kt")
    jniLibs.srcDir("../src-tauri/gen/android/app/src/main/jniLibs")
  }
  kotlinOptions { jvmTarget = "1.8" }
}
dependencies {
  implementation("androidx.appcompat:appcompat:1.7.1")
  implementation("androidx.webkit:webkit:1.14.0")
  androidTestImplementation("androidx.test.ext:junit:1.1.4")
  androidTestImplementation("androidx.test:runner:1.5.2")
  androidTestImplementation("androidx.test.uiautomator:uiautomator:2.3.0")
}
