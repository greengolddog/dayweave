package com.greengolddog.dayweave

import android.content.ComponentName
import android.content.Context
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.xmlpull.v1.XmlPullParser

@RunWith(AndroidJUnit4::class)
class ManifestPrivacyTest {
    @Test
    fun applicationBackupIsDisabled() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val backupFlag = context.applicationInfo.flags and ApplicationInfo.FLAG_ALLOW_BACKUP

        assertEquals(0, backupFlag)
    }

    @Test
    fun healthPermissionScopeIsReadSleepOnly() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        @Suppress("DEPRECATION")
        val requested = context.packageManager.getPackageInfo(
            context.packageName,
            android.content.pm.PackageManager.GET_PERMISSIONS,
        ).requestedPermissions.orEmpty().filter { it.startsWith("android.permission.health.") }

        assertEquals(listOf("android.permission.health.READ_SLEEP"), requested)
    }

    @Test
    fun biometricPromptPermissionIsDeclared() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        @Suppress("DEPRECATION")
        val requested = context.packageManager.getPackageInfo(
            context.packageName,
            android.content.pm.PackageManager.GET_PERMISSIONS,
        ).requestedPermissions.orEmpty()

        assertTrue("android.permission.USE_BIOMETRIC" in requested)
    }

    @Test
    fun legacyAppLockPreferencesRemainExcludedDuringAtomicMigration() {
        val context = ApplicationProvider.getApplicationContext<Context>()

        assertEquals(
            1,
            excludedPathCount(context, R.xml.backup_rules, "dayweave_app_lock.xml"),
        )
        assertEquals(
            2,
            excludedPathCount(context, R.xml.data_extraction_rules, "dayweave_app_lock.xml"),
        )
    }

    @Test
    fun plannerActivityIsProcessUniqueForAppLockLifecycleAccounting() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        @Suppress("DEPRECATION")
        val activityInfo = context.packageManager.getActivityInfo(
            ComponentName(context, MainActivity::class.java),
            0,
        )

        assertEquals(ActivityInfo.LAUNCH_SINGLE_TASK, activityInfo.launchMode)
    }

    private fun excludedPathCount(context: Context, resourceId: Int, path: String): Int {
        val parser = context.resources.getXml(resourceId)
        var matches = 0
        while (parser.eventType != XmlPullParser.END_DOCUMENT) {
            if (
                parser.eventType == XmlPullParser.START_TAG && parser.name == "exclude" &&
                parser.getAttributeValue(null, "path") == path
            ) {
                matches += 1
            }
            parser.next()
        }
        parser.close()
        return matches
    }
}
