plugins {
    id("com.android.application")
}

// SPEC §8: cargo-ndk builds liblexi_core.so into jniLibs before the Android build.
// arm64-v8a only by default; pass -PwithArmv7 to also build armeabi-v7a
// (needs `rustup target add armv7-linux-androideabi`).
val withArmv7 = providers.gradleProperty("withArmv7").isPresent
val repoRoot = rootProject.projectDir.parentFile

// Build-time settings (never committed): LEXI_SERVICE_URL and LEXI_ATTEST_KEY from the environment or
// -PlexiServiceUrl / -PlexiAttestKey. The attestation key ships inside the APK, so it is a weak
// MVP measure only (SPEC §5.4).
fun setting(env: String, prop: String, default: String): String =
    providers.environmentVariable(env).orElse(providers.gradleProperty(prop)).getOrElse(default)
fun quoted(v: String) = "\"" + v.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
val attestKey = setting("LEXI_ATTEST_KEY", "lexiAttestKey", "")

val buildRust = tasks.register<Exec>("buildRust") {
    group = "build"
    description = "Builds lexi-core for Android with cargo-ndk"
    workingDir = repoRoot
    val abis = buildList {
        add("arm64-v8a")
        if (withArmv7) add("armeabi-v7a")
    }
    commandLine(
        buildList {
            add("cargo"); add("ndk")
            abis.forEach { add("-t"); add(it) }
            addAll(listOf("--platform", "31", "-o", "android/app/src/main/jniLibs",
                "build", "-p", "lexi-core", "--release"))
        }
    )
    inputs.files(fileTree(repoRoot.resolve("core")) { exclude("target") }, repoRoot.resolve("Cargo.toml"), repoRoot.resolve("Cargo.lock"))
    outputs.dir(layout.projectDirectory.dir("src/main/jniLibs"))
}
tasks.named("preBuild") { dependsOn(buildRust) }
// A release build has no sensible default address: refuse to build an app that points nowhere.
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    doFirst {
        if (setting("LEXI_SERVICE_URL", "lexiServiceUrl", "").isEmpty()) {
            throw GradleException("LEXI_SERVICE_URL is not set: a release build needs the service address (see .envrc.example)")
        }
    }
}

android {
    namespace = "app.lexi"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.lexi"
        minSdk = 31
        targetSdk = 36 // Google Play requirement since 2026-08-31 (verified 2026-11-21)
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += if (withArmv7) listOf("arm64-v8a", "armeabi-v7a") else listOf("arm64-v8a") }
    }

    buildFeatures { buildConfig = true }

    buildTypes {
        debug {
            // A local `cargo run -p lexi` reached over `adb reverse tcp:8080 tcp:8080`
            // (cleartext to localhost is allowed in debug only).
            buildConfigField("String", "SERVICE_URL", quoted(setting("LEXI_SERVICE_URL", "lexiServiceUrl", "http://localhost:8080")))
            buildConfigField("String", "ATTEST_KEY", quoted(attestKey))
        }
        release {
            buildConfigField("String", "SERVICE_URL", quoted(setting("LEXI_SERVICE_URL", "lexiServiceUrl", "")))
            buildConfigField("String", "ATTEST_KEY", quoted(attestKey))
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    packaging {
        jniLibs { useLegacyPackaging = false }
    }
}

// No dependencies on purpose (SPEC N1): minSdk 31 platform APIs are enough; androidx.core is allowed but unused.
