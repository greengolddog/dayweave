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
class OnboardingReadinessProofMigrationTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val migrationPassphrase = "onboarding-proof-migration-test".encodeToByteArray()

    @get:Rule
    val migrationHelper = MigrationTestHelper(
        instrumentation,
        PlannerDatabase::class.java,
        emptyList(),
        SupportOpenHelperFactory(migrationPassphrase, null, true),
    )

    @After
    fun cleanUp() {
        context.deleteDatabase(DATABASE_NAME)
    }

    @Test
    fun migrationFourteenToSixteenPreservesPayloadUntilStrictRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(DATABASE_NAME, 14).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"ONBOARDING-V15-ROLLBACK-FENCE\"}', " +
                    "1800, '${PlannerSnapshotFormats.JSON_V14}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            DATABASE_NAME,
            16,
            true,
            PlannerDatabaseMigrations.MIGRATION_14_15,
            PlannerDatabaseMigrations.MIGRATION_15_16,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"ONBOARDING-V15-ROLLBACK-FENCE\"}",
                cursor.getString(0),
            )
            assertEquals(1800L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V14, cursor.getString(2))
        }
        migrated.close()
    }

    private companion object {
        const val DATABASE_NAME = "onboarding-proof-migration-test.db"
    }
}
