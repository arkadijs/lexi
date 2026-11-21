plugins {
    id("com.android.application")
}

// SPEC §8: cargo-ndk builds liblexi_core.so into jniLibs before the Android build.
// arm64-v8a only by default; pass -PwithArmv7 to also build armeabi-v7a
// (needs `rustup target add armv7-linux-androideabi`).
val withArmv7 = providers.gradleProperty("withArmv7").isPresent
val repoRoot = rootProject.projectDir.parentFile

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
            // Android emulator -> host loopback; a local `cargo run -p lexi-minter`.
            buildConfigField("String", "MINTER_URL", "\"http://10.0.2.2:8080\"")
        }
        release {
            buildConfigField("String", "MINTER_URL", "\"https://lexi-minter.example.com\"")
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
