pluginManagement {
    val flutterSdkPath =
        run {
            val properties = java.util.Properties()
            file("local.properties").inputStream().use { properties.load(it) }
            val flutterSdkPath = properties.getProperty("flutter.sdk")
            require(flutterSdkPath != null) { "flutter.sdk not set in local.properties" }
            flutterSdkPath
        }

    includeBuild("$flutterSdkPath/packages/flutter_tools/gradle")

    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

plugins {
    id("dev.flutter.flutter-plugin-loader") version "1.0.0"
    id("com.android.application") version "9.1.0" apply false
    // Declared `apply false` only so the KGP version is pinned on the classpath
    // above AGP 9's built-in Kotlin default (2.2.10), which is below Flutter's
    // minimum (2.2.20). The app itself uses built-in Kotlin (android.builtInKotlin=true).
    id("org.jetbrains.kotlin.android") version "2.4.0" apply false
}

include(":app")
