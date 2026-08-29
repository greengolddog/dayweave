package com.greengolddog.dayweave

import android.content.Context
import android.content.pm.ApplicationInfo
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

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
}
