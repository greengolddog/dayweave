package com.greengolddog.dayweave.data

import android.content.Context
import com.greengolddog.dayweave.model.DayWeaveUiState
import kotlinx.serialization.json.Json

interface PlannerStateRepository {
    suspend fun load(): DayWeaveUiState?
    suspend fun save(state: DayWeaveUiState)
}

/** Defers Keystore and database initialization until the store's IO-scoped restore starts. */
class EncryptedRoomPlannerStateRepository(context: Context) : PlannerStateRepository {
    private val applicationContext = context.applicationContext
    private val delegate: RoomPlannerStateRepository by lazy {
        val database = PlannerDatabaseFactory.createEncrypted(applicationContext)
        RoomPlannerStateRepository(database.plannerSnapshotDao())
    }

    override suspend fun load(): DayWeaveUiState? = delegate.load()

    override suspend fun save(state: DayWeaveUiState) = delegate.save(state)
}

class RoomPlannerStateRepository(
    private val dao: PlannerSnapshotDao,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) : PlannerStateRepository {
    override suspend fun load(): DayWeaveUiState? = dao.load()?.let { snapshot ->
        when (snapshot.payloadFormat) {
            PlannerSnapshotFormats.JSON_V2 -> SNAPSHOT_JSON.decodeFromString(snapshot.payload)
            PlannerSnapshotFormats.JSON_V1 -> {
                val migrated = LEGACY_SNAPSHOT_JSON.decodeFromString<DayWeaveUiState>(snapshot.payload)
                save(migrated)
                migrated
            }
            else -> error("Unsupported planner snapshot format")
        }
    }

    override suspend fun save(state: DayWeaveUiState) {
        dao.save(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = SNAPSHOT_JSON.encodeToString(state),
                updatedAtEpochMillis = nowEpochMillis(),
                payloadFormat = PlannerSnapshotFormats.JSON_V2,
            ),
        )
    }

    private companion object {
        val SNAPSHOT_JSON = Json {
            encodeDefaults = true
            ignoreUnknownKeys = false
        }
        val LEGACY_SNAPSHOT_JSON = Json {
            encodeDefaults = true
            ignoreUnknownKeys = true
        }
    }
}
