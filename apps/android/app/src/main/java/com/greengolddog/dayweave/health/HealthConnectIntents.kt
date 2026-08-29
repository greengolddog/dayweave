package com.greengolddog.dayweave.health

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.health.connect.client.HealthConnectClient

object HealthConnectIntents {
    const val PROVIDER_PACKAGE = "com.google.android.apps.healthdata"

    fun manageAccess(): Intent = Intent(HealthConnectClient.ACTION_HEALTH_CONNECT_SETTINGS)

    fun installOrUpdate(context: Context): Intent = Intent(Intent.ACTION_VIEW).apply {
        setPackage("com.android.vending")
        data = Uri.parse(
            "market://details?id=$PROVIDER_PACKAGE&url=healthconnect%3A%2F%2Fonboarding",
        )
        putExtra("overlay", true)
        putExtra("callerId", context.packageName)
    }

    fun browserFallback(): Intent = Intent(
        Intent.ACTION_VIEW,
        Uri.parse("https://play.google.com/store/apps/details?id=$PROVIDER_PACKAGE"),
    )
}
