import java.util.Properties

plugins {
    id("com.android.application")
    id("kotlin-android")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// The release signing identity: `android/key.properties` on a developer's
// machine (never committed; see `.gitignore`), or the EASYNOTES_* variables in
// the release workflow. With neither, a release build comes out *unsigned* —
// never debug-signed, which is what it used to be. Unsigned is what F-Droid
// builds and then signs itself; a store upload needs the real key.
val keyProperties =
    Properties().apply {
        val file = rootProject.file("key.properties")
        if (file.isFile) file.inputStream().use { load(it) }
    }

fun signingValue(property: String, variable: String): String? =
    keyProperties.getProperty(property) ?: System.getenv(variable)?.takeIf { it.isNotEmpty() }

val releaseStoreFile = signingValue("storeFile", "EASYNOTES_KEYSTORE")

android {
    namespace = "eu.fraisl.easynotes"
    // At least what the pinned target needs, and whatever newer the plugins ask for.
    compileSdk = maxOf(flutter.compileSdkVersion, 36)
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
        isCoreLibraryDesugaringEnabled = true
    }

    defaultConfig {
        // Stable once published: a store treats a different ID as a different app.
        applicationId = "eu.fraisl.easynotes"
        minSdk = flutter.minSdkVersion
        // Pinned rather than inherited from the Flutter SDK, so that neither an
        // SDK upgrade changes runtime behaviour unannounced nor an older SDK
        // drops below what Google Play accepts (API 36 since 2026-08-31). The
        // release workflow checks the merged manifest says the same.
        targetSdk = 36
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        releaseStoreFile?.let { path ->
            create("release") {
                storeFile = file(path)
                storePassword = signingValue("storePassword", "EASYNOTES_KEYSTORE_PASSWORD")
                keyAlias = signingValue("keyAlias", "EASYNOTES_KEY_ALIAS")
                keyPassword = signingValue("keyPassword", "EASYNOTES_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("release")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_11
    }
}

flutter {
    source = "../.."
}

dependencies {
    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.5")
}
