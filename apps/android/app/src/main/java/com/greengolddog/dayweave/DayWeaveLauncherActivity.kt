package com.greengolddog.dayweave

import android.app.Activity
import android.content.Intent
import android.os.Bundle

/**
 * Exported launcher boundary. External callers can open DayWeave, but no action, data, category,
 * flag, or extra supplied to this Activity is ever forwarded to the non-exported planner UI.
 */
class DayWeaveLauncherActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        startActivity(
            Intent(this, MainActivity::class.java)
                .setAction(Intent.ACTION_MAIN)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        )
        finish()
    }
}
