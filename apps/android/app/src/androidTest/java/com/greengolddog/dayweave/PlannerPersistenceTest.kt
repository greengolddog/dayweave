package com.greengolddog.dayweave

import android.content.Context
import androidx.room.testing.MigrationTestHelper
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.greengolddog.dayweave.data.KeystoreWrappedPassphraseProvider
import com.greengolddog.dayweave.data.PlannerDatabase
import com.greengolddog.dayweave.data.PlannerDatabaseFactory
import com.greengolddog.dayweave.data.PlannerDatabaseMigrations
import com.greengolddog.dayweave.data.PlannerSnapshotFormats
import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.notifications.PlannerTimedBreakNotificationStateAccess
import com.greengolddog.dayweave.notifications.TimedBreakDeliveryCompletion
import com.greengolddog.dayweave.notifications.TimedBreakNotificationDelivery
import com.greengolddog.dayweave.notifications.TimedBreakNotificationGateway
import com.greengolddog.dayweave.notifications.TimedBreakNotificationPostResult
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.security.KeyStore
import java.time.Instant
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import net.zetetic.database.sqlcipher.SupportOpenHelperFactory
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PlannerPersistenceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val migrationPassphrase = "dayweave-migration-test".encodeToByteArray()

    @get:Rule
    val migrationHelper = MigrationTestHelper(
        instrumentation,
        PlannerDatabase::class.java,
        emptyList(),
        SupportOpenHelperFactory(migrationPassphrase, null, true),
    )

    @After
    fun cleanUp() {
        context.deleteDatabase(RESTORE_DATABASE)
        context.deleteDatabase(MIGRATION_DATABASE)
        context.getSharedPreferences(TEST_PREFERENCES, Context.MODE_PRIVATE).edit().clear().commit()
        KeyStore.getInstance(ANDROID_KEY_STORE).apply {
            load(null)
            if (containsAlias(TEST_KEY_ALIAS)) deleteEntry(TEST_KEY_ALIAS)
        }
    }

    @Test
    fun encryptedDatabaseRestoresStateWithoutCrossingProposalSafetyBoundary() = runBlocking {
        cleanUp()
        val provider = KeystoreWrappedPassphraseProvider(
            context = context,
            preferencesName = TEST_PREFERENCES,
            keyAlias = TEST_KEY_ALIAS,
            databaseName = RESTORE_DATABASE,
        )
        val firstPassphrase = provider.getOrCreatePassphrase()
        val preview = DayWeaveUiState.preview()
        val store = PlannerStore(preview)
        val suggestion = preview.suggestions.first()
        store.approveSuggestion(suggestion.id)
        val approvedState = store.state.value

        var database = PlannerDatabaseFactory.create(context, RESTORE_DATABASE, firstPassphrase)
        RoomPlannerStateRepository(database.plannerSnapshotDao()).save(approvedState)
        database.close()

        val header = context.getDatabasePath(RESTORE_DATABASE).inputStream().use { input ->
            ByteArray(SQLITE_HEADER.size).also { bytes ->
                assertEquals(bytes.size, input.read(bytes))
            }
        }
        assertFalse("SQLCipher database exposed a plaintext SQLite header", header.contentEquals(SQLITE_HEADER))

        val secondPassphrase = KeystoreWrappedPassphraseProvider(
            context = context,
            preferencesName = TEST_PREFERENCES,
            keyAlias = TEST_KEY_ALIAS,
            databaseName = RESTORE_DATABASE,
        ).getOrCreatePassphrase()
        assertArrayEquals(firstPassphrase, secondPassphrase)

        database = PlannerDatabaseFactory.create(context, RESTORE_DATABASE, secondPassphrase)
        val restored = RoomPlannerStateRepository(database.plannerSnapshotDao()).load()
        database.close()
        firstPassphrase.fill(0)
        secondPassphrase.fill(0)

        requireNotNull(restored)
        assertEquals(approvedState, restored)
        assertEquals(preview.schedule, restored.schedule)
        assertEquals(
            SuggestionDisposition.APPROVED_FOR_INBOX,
            restored.suggestions.first { it.id == suggestion.id }.disposition,
        )
        val proposal = restored.inbox.first { it.source == InboxSource.EXTERNAL_PROPOSAL }
        assertTrue(proposal.requiresReview)
    }

    @Test
    fun encryptedTimedBreakClaimSurvivesCloseAndReopenAndCannotPostAgain() = runBlocking {
        cleanUp()
        val passphrase = "dayweave-timed-break-claim-test".encodeToByteArray()
        val deadline = Instant.now().minusSeconds(60).toEpochMilli()
        val initial = timedBreakState(deadline)
        var database = PlannerDatabaseFactory.create(
            context,
            RESTORE_DATABASE,
            passphrase.copyOf(),
        )
        RoomPlannerStateRepository(database.plannerSnapshotDao()).save(initial)
        val firstScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
        val digest: String
        try {
            val firstStore = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = RoomPlannerStateRepository(database.plannerSnapshotDao()),
                scope = firstScope,
                nowEpochMillis = { deadline + 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(5_000) {
                    firstStore.loadState.first { it != PlannerLoadState.LOADING }
                },
            )
            digest = firstStore.state.value
                .authoritativeTimedBreakNotificationIdentity()!!.digest
            assertEquals(
                com.greengolddog.dayweave.notifications.TimedBreakPreparation.READY,
                PlannerTimedBreakNotificationStateAccess(firstStore) { deadline + 1L }
                    .prepare(digest),
            )
            assertEquals(digest, firstStore.durableState.value
                ?.lastBreakEndNotificationAttemptDigest)
        } finally {
            firstScope.cancel()
            database.close()
        }

        database = PlannerDatabaseFactory.create(
            context,
            RESTORE_DATABASE,
            passphrase.copyOf(),
        )
        val restartedScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
        try {
            val restartedStore = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = RoomPlannerStateRepository(database.plannerSnapshotDao()),
                scope = restartedScope,
                nowEpochMillis = { deadline + 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(5_000) {
                    restartedStore.loadState.first { it != PlannerLoadState.LOADING }
                },
            )
            val gateway = CountingTimedBreakGateway()
            assertEquals(
                TimedBreakDeliveryCompletion.SUCCESS,
                TimedBreakNotificationDelivery(
                    stateAccess = PlannerTimedBreakNotificationStateAccess(restartedStore) {
                        deadline + 1L
                    },
                    gateway = gateway,
                ).deliver(digest),
            )
            assertEquals(0, gateway.posts)
            assertEquals(digest, restartedStore.durableState.value
                ?.lastBreakEndNotificationAttemptDigest)
            assertTrue(restartedStore.state.value.activeSession!!.isPaused)
            assertTrue(restartedStore.state.value.activeSession!!.timedBreakEnded)
        } finally {
            restartedScope.cancel()
            database.close()
            passphrase.fill(0)
        }
    }

    @Test
    fun existingDatabaseWithoutWrapperFailsBeforeCreatingReplacementKey() {
        cleanUp()
        val databaseFile = context.getDatabasePath(RESTORE_DATABASE)
        check(databaseFile.parentFile?.let { it.exists() || it.mkdirs() } == true)
        databaseFile.writeBytes(byteArrayOf(0x44, 0x57))

        val result = runCatching {
            KeystoreWrappedPassphraseProvider(
                context = context,
                preferencesName = TEST_PREFERENCES,
                keyAlias = TEST_KEY_ALIAS,
                databaseName = RESTORE_DATABASE,
            ).getOrCreatePassphrase()
        }

        assertTrue(result.exceptionOrNull() is IllegalStateException)
        assertFalse(
            context.getSharedPreferences(TEST_PREFERENCES, Context.MODE_PRIVATE)
                .contains("wrapped_database_passphrase"),
        )
        assertFalse(
            KeyStore.getInstance(ANDROID_KEY_STORE).apply { load(null) }.containsAlias(TEST_KEY_ALIAS),
        )
    }

    @Test
    fun migrationOneToTwoPreservesPayloadAndAddsItsFormat() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 1).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis) VALUES (1, '{}', 123)",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            2,
            true,
            PlannerDatabaseMigrations.MIGRATION_1_2,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals("{}", cursor.getString(0))
            assertEquals(123L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V1, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationThreeToFourPreservesV2PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 3).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-SENSITIVE-MIGRATION-ANDROID\"}', 456, " +
                    "'${PlannerSnapshotFormats.JSON_V2}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            4,
            true,
            PlannerDatabaseMigrations.MIGRATION_3_4,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-SENSITIVE-MIGRATION-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(456L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V2, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationFourToFivePreservesV3PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 4).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-AUTHORING-MIGRATION-ANDROID\"}', " +
                    "789, '${PlannerSnapshotFormats.JSON_V3}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            5,
            true,
            PlannerDatabaseMigrations.MIGRATION_4_5,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-AUTHORING-MIGRATION-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(789L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V3, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationFiveToSixPreservesV4PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 5).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-PUBLISH-MIGRATION-ANDROID\"}', " +
                    "987, '${PlannerSnapshotFormats.JSON_V4}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            6,
            true,
            PlannerDatabaseMigrations.MIGRATION_5_6,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-PUBLISH-MIGRATION-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(987L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V4, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationSixToSevenPreservesV5PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 6).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-PROPOSAL-MIGRATION-ANDROID\"}', " +
                    "1089, '${PlannerSnapshotFormats.JSON_V5}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            7,
            true,
            PlannerDatabaseMigrations.MIGRATION_6_7,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-PROPOSAL-MIGRATION-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(1089L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V5, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationSevenToEightPreservesV6PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 7).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-CANONICAL-AUTHORING-ANDROID\"}', " +
                    "1190, '${PlannerSnapshotFormats.JSON_V6}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            8,
            true,
            PlannerDatabaseMigrations.MIGRATION_7_8,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-CANONICAL-AUTHORING-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(1190L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V6, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationEightToNinePreservesV7PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 8).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-PUBLICATION-PROOF-ANDROID\"}', " +
                    "1291, '${PlannerSnapshotFormats.JSON_V7}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            9,
            true,
            PlannerDatabaseMigrations.MIGRATION_8_9,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-PUBLICATION-PROOF-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(1291L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V7, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationNineToTenPreservesV9PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 9).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-LOCAL-COMPOSITION-ANDROID\"}', " +
                    "1392, '${PlannerSnapshotFormats.JSON_V9}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            10,
            true,
            PlannerDatabaseMigrations.MIGRATION_9_10,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-LOCAL-COMPOSITION-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(1392L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V9, cursor.getString(2))
        }
        migrated.close()
    }

    @Test
    fun migrationTenToElevenPreservesV10PayloadUntilRepositoryRewrite() {
        System.loadLibrary("sqlcipher")
        migrationHelper.createDatabase(MIGRATION_DATABASE, 10).apply {
            execSQL(
                "INSERT INTO planner_snapshot " +
                    "(singletonId, payload, updatedAtEpochMillis, payloadFormat) " +
                    "VALUES (1, '{\"canary\":\"SYNTHETIC-GOOGLE-OUTBOUND-ANDROID\"}', " +
                    "1493, '${PlannerSnapshotFormats.JSON_V10}')",
            )
            close()
        }

        val migrated = migrationHelper.runMigrationsAndValidate(
            MIGRATION_DATABASE,
            11,
            true,
            PlannerDatabaseMigrations.MIGRATION_10_11,
        )
        migrated.query(
            "SELECT payload, updatedAtEpochMillis, payloadFormat " +
                "FROM planner_snapshot WHERE singletonId = 1",
        ).use { cursor ->
            assertTrue(cursor.moveToFirst())
            assertEquals(
                "{\"canary\":\"SYNTHETIC-GOOGLE-OUTBOUND-ANDROID\"}",
                cursor.getString(0),
            )
            assertEquals(1493L, cursor.getLong(1))
            assertEquals(PlannerSnapshotFormats.JSON_V10, cursor.getString(2))
        }
        migrated.close()
    }

    private companion object {
        const val RESTORE_DATABASE = "planner-persistence-restore-test.db"
        const val MIGRATION_DATABASE = "planner-persistence-migration-test.db"
        const val TEST_PREFERENCES = "planner-persistence-test-key"
        const val TEST_KEY_ALIAS = "com.greengolddog.dayweave.test-database-wrapping-key"
        const val ANDROID_KEY_STORE = "AndroidKeyStore"
        val SQLITE_HEADER = "SQLite format 3\u0000".encodeToByteArray()
    }
}

private class CountingTimedBreakGateway : TimedBreakNotificationGateway {
    var posts = 0

    override fun post(identityDigest: String): TimedBreakNotificationPostResult {
        posts += 1
        return TimedBreakNotificationPostResult.POSTED
    }

    override fun cancel() = Unit
}

private fun timedBreakState(deadlineEpochMillis: Long): DayWeaveUiState {
    val deadline = Instant.ofEpochMilli(deadlineEpochMillis).toString()
    return DayWeaveUiState(
        canonicalExecutionRevision = 7,
        canonicalExecutionSession = CanonicalExecutionSessionSnapshot(
            id = "11111111-1111-4111-8111-111111111111",
            itemId = "22222222-2222-4222-8222-222222222222",
            itemRevision = 2,
            sessionIndex = 0,
            plannedBlockId = "33333333-3333-4333-8333-333333333333",
            sourceDeviceId = "44444444-4444-4444-8444-444444444444",
            status = "paused",
            revision = 3,
            accumulatedSeconds = 300,
            startedAt = Instant.ofEpochMilli(deadlineEpochMillis - 600_000L).toString(),
            pausedAt = Instant.ofEpochMilli(deadlineEpochMillis - 300_000L).toString(),
            pauseUntil = deadline,
            createdAt = Instant.ofEpochMilli(deadlineEpochMillis - 600_000L).toString(),
            updatedAt = Instant.ofEpochMilli(deadlineEpochMillis - 300_000L).toString(),
        ),
        activeSession = ActiveSession(
            itemId = "33333333-3333-4333-8333-333333333333",
            elapsedMinutes = 5,
            isPaused = true,
            accumulatedSeconds = 300,
            pauseUntilEpochMillis = deadlineEpochMillis,
            canonicalExecutionSessionId = "11111111-1111-4111-8111-111111111111",
        ),
    )
}
