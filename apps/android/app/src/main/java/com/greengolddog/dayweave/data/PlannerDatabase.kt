package com.greengolddog.dayweave.data

import android.content.Context
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.Upsert
import androidx.room.migration.Migration
import androidx.sqlite.db.SupportSQLiteDatabase
import net.zetetic.database.sqlcipher.SupportOpenHelperFactory

@Entity(tableName = "planner_snapshot")
data class PlannerSnapshotEntity(
    @PrimaryKey
    val singletonId: Int,
    val payload: String,
    val updatedAtEpochMillis: Long,
    val payloadFormat: String = PlannerSnapshotFormats.JSON_V1,
)

@Dao
interface PlannerSnapshotDao {
    @Query("SELECT * FROM planner_snapshot WHERE singletonId = :singletonId LIMIT 1")
    suspend fun load(singletonId: Int = PlannerSnapshotEntityIds.CURRENT): PlannerSnapshotEntity?

    @Upsert
    suspend fun save(snapshot: PlannerSnapshotEntity)
}

private object PlannerSnapshotEntityIds {
    const val CURRENT = 1
}

object PlannerSnapshotFormats {
    const val JSON_V1 = "json-v1"
    const val JSON_V2 = "json-v2-canonical-execution"
    const val JSON_V3 = "json-v3-sensitive-items"
    const val JSON_V4 = "json-v4-sensitive-authoring"
    const val JSON_V5 = "json-v5-schedule-publication-journal"
    const val JSON_V6 = "json-v6-proposal-application-journal"
    const val JSON_V7 = "json-v7-canonical-authoring"
    const val JSON_V8 = "json-v8-exact-schedule-publication-proof"
    /** Encrypted-payload-only fence for exact timed-break delivery/tap/acknowledgement receipts. */
    const val JSON_V9 = "json-v9-timed-break-notification-receipts"
    /** Local provenance plus scheduling profile; older binaries must not ignore either safety gap. */
    const val JSON_V10 = "json-v10-local-schedule-composition-provenance"
    /** Encrypted Google preview/approval authority; rollback must never ignore an open journal. */
    const val JSON_V11 = "json-v11-google-calendar-outbound-recovery"
    /** Configurable firm-horizon policy; older labels must not accept an injected day count. */
    const val JSON_V12 = "json-v12-configurable-firm-horizon"
    /** Generic Google outbound recovery; older labels remain Calendar-upsert-only. */
    const val JSON_V13 = "json-v13-google-outbound-recovery"
    /** Generated-schedule Google review, approval authority, and accepted status recovery. */
    const val JSON_V14 = "json-v14-google-schedule-publication-recovery"
}

@Database(
    entities = [PlannerSnapshotEntity::class],
    version = 14,
    exportSchema = true,
)
abstract class PlannerDatabase : RoomDatabase() {
    abstract fun plannerSnapshotDao(): PlannerSnapshotDao
}

object PlannerDatabaseMigrations {
    val MIGRATION_1_2 = object : Migration(1, 2) {
        override fun migrate(db: SupportSQLiteDatabase) {
            db.execSQL(
                "ALTER TABLE planner_snapshot " +
                    "ADD COLUMN payloadFormat TEXT NOT NULL DEFAULT '${PlannerSnapshotFormats.JSON_V1}'",
            )
        }
    }

    /**
     * No columns change. Advancing the database version makes rollback fail closed while the
     * repository migrates each encrypted JSON payload from v1 to the strict v2 contract.
     */
    val MIGRATION_2_3 = object : Migration(2, 3) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No columns change. The version fence prevents rollback while each encrypted JSON payload is
     * rewritten from v2 with explicit non-sensitive legacy values into the strict v3 contract.
     */
    val MIGRATION_3_4 = object : Migration(3, 4) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No columns change. The version fence prevents rollback while encrypted snapshots gain
     * explicit Inbox sensitivity and an exact sensitivity target for pending canonical writes.
     */
    val MIGRATION_4_5 = object : Migration(4, 5) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No columns change. The encrypted payload gains an exact schedule-publication journal and a
     * published revision receipt; the version fence prevents an older binary from ignoring them.
     */
    val MIGRATION_5_6 = object : Migration(5, 6) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * The encrypted payload gains exact proposal apply/undo recovery evidence and content-free
     * receipts. The version fence prevents an older binary from ignoring an unresolved write.
     */
    val MIGRATION_6_7 = object : Migration(6, 7) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * The encrypted payload gains typed canonical authoring journals and recently-deleted rows.
     * No plaintext columns change; the Room version is a rollback fence for the JSON contract.
     */
    val MIGRATION_7_8 = object : Migration(7, 8) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * The encrypted payload gains exact per-block publication authority. No plaintext columns
     * change; the Room version prevents rollback to a binary that could ignore this start gate.
     */
    val MIGRATION_8_9 = object : Migration(8, 9) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No plaintext columns change. The encrypted payload gains explicit display-only provenance;
     * the Room version prevents rollback to code that could mistake that plan for server output.
     */
    val MIGRATION_9_10 = object : Migration(9, 10) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No plaintext columns change. The encrypted payload gains one-shot Google Calendar approval
     * recovery; the Room version prevents rollback to code that could ignore or reuse it.
     */
    val MIGRATION_10_11 = object : Migration(10, 11) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No plaintext columns change. The encrypted scheduling profile gains an explicit bounded
     * firm-horizon day count; the Room version prevents rollback from silently ignoring it.
     */
    val MIGRATION_11_12 = object : Migration(11, 12) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No plaintext columns change. The encrypted outbound journal gains an explicit provider
     * entity kind and delete operation; the Room version prevents rollback to Calendar-only code
     * that could ignore generalized recovery authority.
     */
    val MIGRATION_12_13 = object : Migration(12, 13) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }

    /**
     * No plaintext columns change. The SQLCipher payload gains generated-schedule publication
     * recovery; the Room version prevents rollback to code that could ignore one-shot authority.
     */
    val MIGRATION_13_14 = object : Migration(13, 14) {
        override fun migrate(db: SupportSQLiteDatabase) = Unit
    }
}

object PlannerDatabaseFactory {
    const val DATABASE_NAME = "encrypted.db"

    fun createEncrypted(
        context: Context,
        databaseName: String = DATABASE_NAME,
        passphraseProvider: DatabasePassphraseProvider = KeystoreWrappedPassphraseProvider(
            context = context,
            databaseName = databaseName,
        ),
    ): PlannerDatabase {
        val passphrase = passphraseProvider.getOrCreatePassphrase()
        return try {
            create(context, databaseName, passphrase)
        } finally {
            passphrase.fill(0)
        }
    }

    fun create(
        context: Context,
        databaseName: String,
        passphrase: ByteArray,
    ): PlannerDatabase {
        System.loadLibrary("sqlcipher")
        val sqlCipherFactory = SupportOpenHelperFactory(
            passphrase.copyOf(),
            null,
            true,
        )
        return Room.databaseBuilder(
            context.applicationContext,
            PlannerDatabase::class.java,
            databaseName,
        )
            .openHelperFactory(sqlCipherFactory)
            .setJournalMode(RoomDatabase.JournalMode.WRITE_AHEAD_LOGGING)
            .addMigrations(
                PlannerDatabaseMigrations.MIGRATION_1_2,
                PlannerDatabaseMigrations.MIGRATION_2_3,
                PlannerDatabaseMigrations.MIGRATION_3_4,
                PlannerDatabaseMigrations.MIGRATION_4_5,
                PlannerDatabaseMigrations.MIGRATION_5_6,
                PlannerDatabaseMigrations.MIGRATION_6_7,
                PlannerDatabaseMigrations.MIGRATION_7_8,
                PlannerDatabaseMigrations.MIGRATION_8_9,
                PlannerDatabaseMigrations.MIGRATION_9_10,
                PlannerDatabaseMigrations.MIGRATION_10_11,
                PlannerDatabaseMigrations.MIGRATION_11_12,
                PlannerDatabaseMigrations.MIGRATION_12_13,
                PlannerDatabaseMigrations.MIGRATION_13_14,
            )
            .build()
    }
}
