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
}

@Database(
    entities = [PlannerSnapshotEntity::class],
    version = 8,
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
            )
            .build()
    }
}
