plugins {
    id("com.android.application")
}

android {
    namespace = "dev.acme.shop"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.acme.shop"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "3.4.1"
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
}
