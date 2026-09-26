// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

import javax.inject.Inject
import org.gradle.process.ExecOperations

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose.compiler)
}

android {
    namespace = "dev.itsvic.softmodem"
    compileSdk = 37
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "dev.itsvic.softmodem"
        minSdk = 27
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "armeabi-v7a" }
    }

    buildFeatures { compose = true }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // Extracted to the native library directory, so that su can run it.
    packaging { jniLibs { useLegacyPackaging = true } }
}

/** Builds the softmodem binary for the phone, named as a library so that the APK carries it. */
abstract class CargoBuild : DefaultTask() {
    @get:Input abstract val target: Property<String>
    @get:Internal abstract val workspace: DirectoryProperty
    @get:Internal abstract val ndk: DirectoryProperty
    @get:OutputDirectory abstract val outputDir: DirectoryProperty
    @get:Inject abstract val execOperations: ExecOperations

    init {
        // Cargo knows best whether the binary is current.
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun build() {
        val host = if (System.getProperty("os.name").startsWith("Mac")) "darwin-x86_64" else "linux-x86_64"
        val bin = ndk.get().dir("toolchains/llvm/prebuilt/$host/bin").asFile
        val clang = bin.resolve("armv7a-linux-androideabi27-clang").path
        val key = target.get().replace('-', '_')
        execOperations.exec {
            workingDir = workspace.get().asFile
            commandLine("rustup", "run", "stable", "cargo", "build", "--release", "--target", target.get(), "-p", "softmodem")
            environment("CARGO_TARGET_${key.uppercase()}_LINKER", clang)
            environment("CC_$key", clang)
            environment("AR_$key", bin.resolve("llvm-ar").path)
        }
        val binary = workspace.get().file("target/${target.get()}/release/softmodem").asFile
        binary.copyTo(outputDir.get().file("armeabi-v7a/libsoftmodem.so").asFile, overwrite = true)
    }
}

val cargoBuild = tasks.register<CargoBuild>("cargoBuild") {
    target.set("armv7-linux-androideabi")
    workspace.set(rootProject.layout.projectDirectory.dir(".."))
    ndk.set(androidComponents.sdkComponents.ndkDirectory)
    outputDir.set(layout.buildDirectory.dir("rust/jniLibs"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargoBuild, CargoBuild::outputDir)
    }
}

dependencies {
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.activity.compose)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.coroutines.android)
}
