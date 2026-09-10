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

@RunWith(AndroidJUnit4::class)
class RoutineOccurrenceMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    @get:Rule val helper = MigrationTestHelper(instrumentation, PlannerDatabase::class.java, emptyList(),
        SupportOpenHelperFactory("synthetic-occurrence-migration".encodeToByteArray(), null, true))
    @After fun cleanup() { instrumentation.targetContext.deleteDatabase(DATABASE) }

    @Test fun v23To24FencesRollbackWithoutRewritingEncryptedExistingJournalBytes() {
        System.loadLibrary("sqlcipher")
        val payload = """{"canary":"ROUTINE-V24-ROLLBACK-FENCE","exact":" unchanged "}"""
        helper.createDatabase(DATABASE, 23).apply {
            execSQL("INSERT INTO planner_snapshot(singletonId,payload,updatedAtEpochMillis,payloadFormat) VALUES(1,?,2400,?)",
                arrayOf(payload, PlannerSnapshotFormats.JSON_V23))
            close()
        }
        helper.runMigrationsAndValidate(DATABASE, 24, true, PlannerDatabaseMigrations.MIGRATION_23_24).use { database ->
            assertEquals(24, database.version)
            database.query("SELECT payload,updatedAtEpochMillis,payloadFormat FROM planner_snapshot WHERE singletonId=1").use {
                assertTrue(it.moveToFirst()); assertEquals(payload, it.getString(0)); assertEquals(2400L, it.getLong(1))
                assertEquals(PlannerSnapshotFormats.JSON_V23, it.getString(2))
            }
        }
    }
    private companion object { const val DATABASE = "routine-occurrence-migration-test.db" }
}
