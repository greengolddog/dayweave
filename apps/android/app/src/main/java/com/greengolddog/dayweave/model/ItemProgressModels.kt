package com.greengolddog.dayweave.model

import java.time.Instant
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement

/** Independent self-reported values, never lifecycle or scheduling authority. */
@Serializable
sealed interface ItemProgressValue {
    @Serializable @SerialName("percentage")
    data class Percentage(@SerialName("basis_points") val basisPoints: Int) : ItemProgressValue
    @Serializable @SerialName("time")
    data class Time(@SerialName("elapsed_seconds") val elapsedSeconds: Long,
        @SerialName("remaining_seconds") val remainingSeconds: Long?) : ItemProgressValue
    @Serializable @SerialName("quantity")
    data class Quantity(val current: String, val unit: String, val target: ItemProgressTarget?) : ItemProgressValue
}

@Serializable
enum class ItemProgressDirection {
    @SerialName("at_least") AT_LEAST,
    @SerialName("at_most") AT_MOST,
}

@Serializable
data class ItemProgressTarget(val value: String, val direction: ItemProgressDirection)

@Serializable
data class ItemProgressComponent(val id: String, val name: String, val value: ItemProgressValue) {
    fun requireValid() {
        requireCanonicalUuid(id, "progress component")
        requireProgressText(name, 80)
        when (val value = value) {
            is ItemProgressValue.Percentage -> require(value.basisPoints in 0..10_000)
            is ItemProgressValue.Time -> {
                require(value.elapsedSeconds in 0..MAX_ITEM_PROGRESS_SECONDS)
                require(value.remainingSeconds == null || value.remainingSeconds in 0..MAX_ITEM_PROGRESS_SECONDS)
            }
            is ItemProgressValue.Quantity -> {
                requireCanonicalProgressDecimal(value.current)
                requireProgressText(value.unit, 32)
                value.target?.let { requireCanonicalProgressDecimal(it.value) }
            }
        }
    }
}

@Serializable
data class ItemProgressSnapshot(
    @SerialName("schema_version") val schemaVersion: Int,
    @SerialName("item_id") val itemId: String,
    @SerialName("item_revision") val itemRevision: Long,
    val revision: Long,
    val components: List<ItemProgressComponent>,
    @SerialName("updated_at") val updatedAt: String?,
) {
    fun requireValid() {
        require(schemaVersion == 1 && itemRevision > 0 && revision >= 0)
        requireCanonicalUuid(itemId, "progress item")
        requireProgressComponents(components)
        if (revision == 0L) require(components.isEmpty() && updatedAt == null)
        else requireProgressInstant(requireNotNull(updatedAt))
    }

    fun sameProgress(other: ItemProgressSnapshot): Boolean = itemId == other.itemId &&
        revision == other.revision && components == other.components && updatedAt == other.updatedAt
}

@Serializable
data class ItemProgressRequest(
    @SerialName("schema_version") val schemaVersion: Int = 1,
    @SerialName("operation_id") val operationId: String,
    @SerialName("expected_item_revision") val expectedItemRevision: Long,
    @SerialName("expected_progress_revision") val expectedProgressRevision: Long,
    val components: List<ItemProgressComponent>,
) {
    fun requireValid() {
        require(schemaVersion == 1 && expectedItemRevision > 0 && expectedProgressRevision >= 0 && expectedProgressRevision < Long.MAX_VALUE)
        requireCanonicalUuid(operationId, "progress operation")
        requireProgressComponents(components)
    }
}

@Serializable
data class ItemProgressMutationResult(
    @SerialName("operation_id") val operationId: String,
    val replayed: Boolean,
    val progress: ItemProgressSnapshot,
)

@Serializable
data class ItemProgressObservation(val snapshot: ItemProgressSnapshot, val observedAt: String, val isGetProof: Boolean)

@Serializable
enum class ItemProgressDisposition { PENDING, REVIEW_REQUIRED, ITEM_MISSING, REJECTED }

@Serializable
data class PendingItemProgressMutation(
    val schemaVersion: Int = 1,
    val operationId: String,
    val itemId: String,
    val syncOrigin: String,
    val configurationId: String,
    val expectedItemRevision: Long,
    val expectedProgressRevision: Long,
    val requestJson: String,
    val createdAt: String,
    val submittedAt: String? = null,
    val disposition: ItemProgressDisposition = ItemProgressDisposition.PENDING,
    /** Sticky privacy of the reviewed intent, independent of later canonical reparenting. */
    val wasSensitive: Boolean = true,
) {
    fun request(): ItemProgressRequest = decodeExactItemProgress(requestJson)
    fun requireValid() {
        require(schemaVersion == 1)
        requireCanonicalUuid(itemId, "progress item")
        require(syncOrigin.isNotBlank() && configurationId.isNotBlank())
        require(syncOrigin.length <= 4_096 && configurationId.length <= 4_096)
        require(requestJson.toByteArray(Charsets.UTF_8).size <= 65_536)
        requireProgressInstant(createdAt)
        submittedAt?.let(::requireProgressInstant)
        val body = request().also(ItemProgressRequest::requireValid)
        require(body.operationId == operationId && body.expectedItemRevision == expectedItemRevision &&
            body.expectedProgressRevision == expectedProgressRevision)
    }
}

@Serializable
data class ItemProgressLedger(
    val schemaVersion: Int = 1,
    val syncOrigin: String? = null,
    val configurationId: String? = null,
    val observations: Map<String, ItemProgressObservation> = emptyMap(),
    val pending: List<PendingItemProgressMutation> = emptyList(),
) {
    fun requireValid() {
        require(schemaVersion == 1 && (syncOrigin == null) == (configurationId == null))
        if (syncOrigin == null) require(observations.isEmpty() && pending.isEmpty())
        else require(syncOrigin.isNotBlank() && !configurationId.isNullOrBlank())
        require(observations.size <= 256 && pending.size <= 64)
        require(pending.sumOf { it.requestJson.toByteArray(Charsets.UTF_8).size.toLong() } <= 2_097_152)
        observations.forEach { (id, value) ->
            value.snapshot.requireValid()
            require(id == value.snapshot.itemId)
            requireProgressInstant(value.observedAt)
        }
        require(pending.map { it.operationId }.distinct().size == pending.size)
        require(pending.map { it.itemId }.distinct().size == pending.size)
        pending.forEach { it.requireValid(); require(it.syncOrigin == syncOrigin && it.configurationId == configurationId) }
    }
}

internal const val MAX_ITEM_PROGRESS_SECONDS = 3_155_760_000L
internal val ITEM_PROGRESS_JSON = Json { ignoreUnknownKeys = false; explicitNulls = true; encodeDefaults = true }

/** Reject quoted/fractional integers, omitted nullable/default fields and any newer shape. */
internal inline fun <reified T> decodeExactItemProgress(body: String): T {
    requireStrictItemProgressJson(body)
    val raw = ITEM_PROGRESS_JSON.parseToJsonElement(body)
    val decoded = ITEM_PROGRESS_JSON.decodeFromJsonElement<T>(raw)
    require(ITEM_PROGRESS_JSON.encodeToJsonElement(decoded) == raw)
    return decoded
}

internal fun requireProgressComponents(components: List<ItemProgressComponent>) {
    require(components.size <= 16 && components.map { it.id }.distinct().size == components.size)
    components.forEach(ItemProgressComponent::requireValid)
}

internal fun requireCanonicalProgressDecimal(value: String) {
    require(PROGRESS_DECIMAL.matches(value))
    require(value != "-0")
}

private val PROGRESS_DECIMAL = Regex("-?(?:0|[1-9][0-9]{0,11})(?:\\.[0-9]{0,5}[1-9])?")

internal fun requireProgressText(value: String, maximum: Int) {
    require(value.isNotBlank() && value.trim() == value && value.codePointCount(0, value.length) <= maximum)
    require(value.none { it.code in 0..31 || it.code in 127..159 })
    // Unpaired UTF-16 surrogates are not Unicode scalars.
    var index = 0
    while (index < value.length) {
        val char = value[index++]
        if (char.isHighSurrogate()) require(index < value.length && value[index++].isLowSurrogate())
        else require(!char.isLowSurrogate())
    }
}

internal fun requireProgressInstant(value: String) {
    val instant = Instant.parse(value)
    require(value.endsWith("Z") && instant.nano % 1_000 == 0)
}
