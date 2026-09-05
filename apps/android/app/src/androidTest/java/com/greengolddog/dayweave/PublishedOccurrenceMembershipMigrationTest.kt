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
class PublishedOccurrenceMembershipMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val passphrase = "published-membership-migration-test".encodeToByteArray()

    @get:Rule
    val migrationHelper = MigrationTestHelper(
        instrumentation,
        PlannerDatabase::class.java,
        emptyList(),
        SupportOpenHelperFactory(passphrase, null, true),
    )

    @After
    fun cleanUp() {
        context.deleteDatabase(DATABASE_NAME)
    }

    @Test
    fun migrationNineteenToTwentyPreservesEncryptedPayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(DATABASE_NAME, 19).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"MEMBERSHIP-V20-ROLLBACK-FENCE\"}', " +
                    "2200, '${PlannerSnapshotFormats.JSON_V19}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            DATABASE_NAME,
            20,
            true,
            PlannerDatabaseMigrations.MIGRATION_19_20,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals("{\"canary\":\"MEMBERSHIP-V20-ROLLBACK-FENCE\"}", cursor.getString(0))
            assertEquals(2200L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V19, cursor.getString(2))
        }
        migrated.close()
    }

    private companion object {
        const val DATABASE_NAME = "published-occurrence-membership-migration-test.db"
    }
}
