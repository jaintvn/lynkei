plugins {
    id("com.android.application")
}

android {
    namespace = "dev.relayproxy.android"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.relayproxy.android"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-prototype"
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    buildTypes {
        release { isMinifyEnabled = false }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    // UniFFI's Kotlin bindings call into librelay_ffi.so through JNA.
    implementation("net.java.dev.jna:jna:5.17.0@aar")
}
