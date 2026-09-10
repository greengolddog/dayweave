package com.greengolddog.dayweave

import androidx.room.testing.MigrationTestHelper
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.data.PlannerDatabase
import com.greengolddog.dayweave.data.PlannerDatabaseMigrations
import com.greengolddog.dayweave.data.PlannerSnapshotFormats
import net.zetetic.database.sqlcipher.SupportOpenHelperFactory
import org.junit.After
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Inert migration instrumentation source; no owner/device execution is claimed. */
@RunWith(AndroidJUnit4::class)
class RoutinePlanningDisplayMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    @get:Rule val helper = MigrationTestHelper(instrumentation, PlannerDatabase::class.java, emptyList(),
        SupportOpenHelperFactory("synthetic-preview-migration".encodeToByteArray(), null, true))
    @After fun cleanup() { instrumentation.targetContext.deleteDatabase(DATABASE) }
    @Test fun v25To26LeavesExactPriorEncryptedPayloadAndJournalsUntouched() {
        System.loadLibrary("sqlcipher")
        val payload = """{"canary":"PREVIEW-V26","exact":" unchanged \n "}"""
        helper.createDatabase(DATABASE, 25).apply {
            execSQL("INSERT INTO planner_snapshot(singletonId,payload,updatedAtEpochMillis,payloadFormat) VALUES(1,?,2600,?)",
                arrayOf(payload, PlannerSnapshotFormats.JSON_V25)); close()
        }
        helper.runMigrationsAndValidate(DATABASE, 26, true, PlannerDatabaseMigrations.MIGRATION_25_26).use { database ->
            assertEquals(26, database.version)
            database.query("SELECT payload,updatedAtEpochMillis,payloadFormat FROM planner_snapshot WHERE singletonId=1").use {
                assertTrue(it.moveToFirst()); assertEquals(payload, it.getString(0)); assertEquals(2600L, it.getLong(1))
                assertEquals(PlannerSnapshotFormats.JSON_V25, it.getString(2))
            }
        }
    }
    private companion object { const val DATABASE = "routine-planning-display-migration-test.db" }
}
