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

/** SQLCipher migration source; it does not claim an owner-device execution. */
@RunWith(AndroidJUnit4::class)
class RoutinePlanningInputMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    @get:Rule val helper = MigrationTestHelper(instrumentation, PlannerDatabase::class.java, emptyList(),
        SupportOpenHelperFactory("synthetic-planning-input-migration".encodeToByteArray(), null, true))
    @After fun cleanup() { instrumentation.targetContext.deleteDatabase(DATABASE) }

    @Test fun v24To25FencesRollbackWithoutChangingAnyExistingEncryptedJournalBytes() {
        System.loadLibrary("sqlcipher")
        val payload = """{"canary":"PLANNING-INPUT-V25","exact":" unchanged \n "}"""
        helper.createDatabase(DATABASE, 24).apply {
            execSQL("INSERT INTO planner_snapshot(singletonId,payload,updatedAtEpochMillis,payloadFormat) VALUES(1,?,2500,?)",
                arrayOf(payload, PlannerSnapshotFormats.JSON_V24))
            close()
        }
        helper.runMigrationsAndValidate(DATABASE, 25, true, PlannerDatabaseMigrations.MIGRATION_24_25).use { database ->
            assertEquals(25, database.version)
            database.query("SELECT payload,updatedAtEpochMillis,payloadFormat FROM planner_snapshot WHERE singletonId=1").use {
                assertTrue(it.moveToFirst()); assertEquals(payload, it.getString(0)); assertEquals(2500L, it.getLong(1))
                assertEquals(PlannerSnapshotFormats.JSON_V24, it.getString(2))
            }
        }
    }
    private companion object { const val DATABASE = "routine-planning-input-migration-test.db" }
}
