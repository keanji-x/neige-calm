import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 36
    namespace = "io.neigecalm.next"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "io.neigecalm.next"
        minSdk = 26
        targetSdk = 36
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        testInstrumentationRunnerArguments["clearPackageData"] = "true"
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    buildTypes {
        getByName("debug") {
            applicationIdSuffix = ".p2ptrial"
            manifestPlaceholders["usesCleartextTraffic"] = "false"
            isDebuggable = true
            isJniDebuggable = false
            isMinifyEnabled = false
            // Keep the installable test APK small; native debug symbols stay in target/.
        }
        getByName("release") {
            if (providers.gradleProperty("neige.releaseSmoke").orNull == "true") {
                signingConfig = signingConfigs.getByName("debug")
            }
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
        create("instrumented") {
            initWith(getByName("debug"))
            applicationIdSuffix = ".instrumented"
            matchingFallbacks += listOf("debug")
        }
    }
    testBuildType = when {
        providers.gradleProperty("neige.releaseSmoke").orNull == "true" -> "release"
        providers.gradleProperty("neige.launcherSmoke").orNull == "true" -> "debug"
        else -> "instrumented"
    }
    testOptions { execution = "ANDROIDX_TEST_ORCHESTRATOR" }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
    sourceSets.getByName("main").assets.srcDir("../../../../bundled-frontend")
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
    androidTestImplementation("androidx.test:runner:1.5.2")
    androidTestImplementation("androidx.test:rules:1.5.0")
    androidTestImplementation("androidx.test.uiautomator:uiautomator:2.3.0")
    androidTestUtil("androidx.test:orchestrator:1.4.2")
}

apply(from = "tauri.build.gradle.kts")

// Every Android packaging entry point must include the userspace network library.
val p2pAbis = providers.gradleProperty("abiList").orElse("arm64-v8a,armeabi-v7a,x86,x86_64").get().split(',')
val p2pTasks = p2pAbis.map { abi ->
    tasks.register<Exec>("buildP2p" + abi.replace("-", "").replace("_", "")) {
        workingDir = file("../../../..")
        commandLine("bash", "scripts/build-p2p-native.sh", abi)
        inputs.dir(file("../../../../p2p-native"))
        inputs.file(file("../../../../scripts/build-p2p-native.sh"))
        outputs.file(file("src/main/jniLibs/$abi/libneige_p2p.so"))
    }
}
tasks.configureEach {
    if (name.endsWith("JniLibFolders")) dependsOn(p2pTasks)
}
