import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.net.URI
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermission
import java.util.Properties

fun String.asBuildConfigString(): String =
    "\"${replace("\\", "\\\\").replace("\"", "\\\"")}\""

val dayWeaveApiBaseUrl = providers.gradleProperty("dayweaveApiBaseUrl")
    .orElse("")
    .get()
    .trim()

fun requirePrivateRegularFile(candidate: File, description: String) {
    require(candidate.isFile && !Files.isSymbolicLink(candidate.toPath())) {
        "$description must be a regular, non-symlink file"
    }
    val permissions = runCatching {
        Files.getPosixFilePermissions(candidate.toPath())
    }.getOrNull()
    if (permissions != null) {
        require(
            permissions == setOf(
                PosixFilePermission.OWNER_READ,
                PosixFilePermission.OWNER_WRITE,
            ),
        ) { "$description must have mode 0600" }
    }
}

val releaseSigningProperties = providers.environmentVariable(
    "DAYWEAVE_ANDROID_SIGNING_PROPERTIES",
).orNull?.let { rawPath ->
    val propertiesFile = file(rawPath)
    requirePrivateRegularFile(propertiesFile, "Android signing properties")
    Properties().apply {
        propertiesFile.inputStream().use(::load)
    }
}

fun Properties.requiredSigningValue(name: String): String =
    getProperty(name)?.trim()?.takeIf(String::isNotEmpty)
        ?: error("Missing Android signing property: $name")

if (dayWeaveApiBaseUrl.isNotEmpty()) {
    val configuredUri = URI(dayWeaveApiBaseUrl)
    require(
        configuredUri.scheme.equals("https", ignoreCase = true) &&
            !configuredUri.host.isNullOrBlank() &&
            configuredUri.userInfo == null &&
            configuredUri.query == null &&
            configuredUri.fragment == null
    ) {
        "dayweaveApiBaseUrl must be an HTTPS origin or path without credentials, query, or fragment"
    }
}

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.ksp)
    alias(libs.plugins.androidx.room)
}

android {
    namespace = "com.greengolddog.dayweave"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.greengolddog.dayweave"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"

        buildConfigField(
            "String",
            "DAYWEAVE_API_BASE_URL",
            dayWeaveApiBaseUrl.asBuildConfigString(),
        )

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        vectorDrawables.useSupportLibrary = true
    }

    signingConfigs {
        if (releaseSigningProperties != null) {
            create("dayWeaveRelease") {
                val keyStorePath = releaseSigningProperties.requiredSigningValue("storeFile")
                val keyStoreFile = file(keyStorePath)
                requirePrivateRegularFile(keyStoreFile, "Android release keystore")
                storeFile = keyStoreFile
                storePassword = releaseSigningProperties.requiredSigningValue("storePassword")
                keyAlias = releaseSigningProperties.requiredSigningValue("keyAlias")
                keyPassword = releaseSigningProperties.requiredSigningValue("keyPassword")
                enableV1Signing = true
                enableV2Signing = true
                enableV3Signing = true
                enableV4Signing = true
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            releaseSigningProperties?.let {
                signingConfig = signingConfigs.getByName("dayWeaveRelease")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }

    sourceSets.getByName("androidTest").assets.srcDir("$projectDir/schemas")
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        freeCompilerArgs.add("-Xannotation-default-target=param-property")
    }
}

room {
    schemaDirectory("$projectDir/schemas")
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.activity.compose)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.extended)

    implementation(libs.androidx.room.runtime)
    implementation(libs.androidx.room.ktx)
    implementation(libs.androidx.sqlite)
    implementation(libs.sqlcipher.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.okhttp)
    implementation(libs.androidx.work.runtime.ktx)
    ksp(libs.androidx.room.compiler)

    testImplementation(libs.junit)
    testImplementation(libs.okhttp.mockwebserver)
    testImplementation(libs.androidx.work.testing)
    testImplementation(libs.robolectric)

    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation(libs.androidx.espresso.core)
    androidTestImplementation(libs.androidx.room.testing)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.compose.ui.test.junit4)

    debugImplementation(libs.androidx.compose.ui.tooling)
    debugImplementation(libs.androidx.compose.ui.test.manifest)
}
