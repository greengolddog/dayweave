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
class ItemCompletionMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    @get:Rule val helper = MigrationTestHelper(instrumentation, PlannerDatabase::class.java, emptyList(),
        SupportOpenHelperFactory("synthetic-completion-migration".encodeToByteArray(), null, true))
    @After fun cleanup() { instrumentation.targetContext.deleteDatabase(DATABASE) }

    @Test fun v22To23FencesRollbackWithoutChangingEncryptedPriorIntentBytes() {
        System.loadLibrary("sqlcipher")
        val payload = "{\"canary\":\"COMPLETION-V23-ROLLBACK-FENCE\",\"exact\":\"unchanged\"}"
        helper.createDatabase(DATABASE, 22).apply {
            execSQL("INSERT INTO planner_snapshot(singletonId,payload,updatedAtEpochMillis,payloadFormat) VALUES(1,?,2300,?)",
                arrayOf(payload, PlannerSnapshotFormats.JSON_V22))
            close()
        }
        helper.runMigrationsAndValidate(DATABASE, 23, true, PlannerDatabaseMigrations.MIGRATION_22_23).use { database ->
            assertEquals(23, database.version)
            database.query("SELECT payload,updatedAtEpochMillis,payloadFormat FROM planner_snapshot WHERE singletonId=1").use {
                assertTrue(it.moveToFirst()); assertEquals(payload, it.getString(0)); assertEquals(2300L, it.getLong(1))
                assertEquals(PlannerSnapshotFormats.JSON_V22, it.getString(2))
            }
        }
    }
    private companion object { const val DATABASE = "item-completion-migration-test.db" }
}
