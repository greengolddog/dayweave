package com.greengolddog.dayweave

import androidx.room.testing.MigrationTestHelper
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.data.PlannerDatabase
import com.greengolddog.dayweave.data.PlannerDatabaseMigrations
import com.greengolddog.dayweave.data.PlannerSnapshotFormats
import net.zetetic.database.sqlcipher.SupportOpenHelperFactory
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ItemProgressMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext

    @get:Rule
    val migrationHelper = MigrationTestHelper(
        instrumentation,
        PlannerDatabase::class.java,
        emptyList(),
        SupportOpenHelperFactory("synthetic-progress-migration".encodeToByteArray(), null, true),
    )

    @After
    fun cleanUp() { context.deleteDatabase(DATABASE_NAME) }

    @Test
    fun migrationTwentyOneToTwentyTwoFencesRollbackWithoutRewritingEncryptedJournalBytes() {
        System.loadLibrary("sqlcipher")
        val payload = "{\"canary\":\"PROGRESS-V22-ROLLBACK-FENCE\",\"exact\":\"unchanged\"}"
        migrationHelper.createDatabase(DATABASE_NAME, 21).apply {
            execSQL(
                "INSERT INTO planner_snapshot (singletonId,payload,updatedAtEpochMillis,payloadFormat) " +
                    "VALUES (1,?,2300,?)",
                arrayOf(payload, PlannerSnapshotFormats.JSON_V21),
            )
            close()
        }
        val migrated = migrationHelper.runMigrationsAndValidate(
            DATABASE_NAME, 22, true, PlannerDatabaseMigrations.MIGRATION_21_22,
        )
        assertEquals(22, migrated.version)
        migrated.query("SELECT payload,updatedAtEpochMillis,payloadFormat FROM planner_snapshot WHERE singletonId=1")
            .use { cursor ->
                assertTrue(cursor.moveToFirst())
                assertEquals(payload, cursor.getString(0))
                assertEquals(2300L, cursor.getLong(1))
                assertEquals(PlannerSnapshotFormats.JSON_V21, cursor.getString(2))
            }
        migrated.close()
    }

    private companion object {
        const val DATABASE_NAME = "item-progress-migration-test.db"
    }
}
