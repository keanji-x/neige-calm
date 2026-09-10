plugins {
  id("com.android.application") version "8.11.0"
  id("org.jetbrains.kotlin.android") version "1.9.25"
}
val stageLoginSources by tasks.registering(Sync::class) {
  from("../src-tauri/gen/android/app/src/main/java/io/neigecalm/next") {
    include("NativeP2P.kt", "ConnectionTrialActivity.kt", "AndroidNetworkSnapshot.kt")
  }
  into(layout.buildDirectory.dir("generated/login"))
}
tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile>().configureEach { dependsOn(stageLoginSources) }
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
    java.srcDir(layout.buildDirectory.dir("generated/login"))
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
