package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
import java.time.Instant
import java.time.LocalDate
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
enum class RemoteHabitOutcomeStatus {
    @SerialName("unresolved")
    UNRESOLVED,

    @SerialName("partial")
    PARTIAL,

    @SerialName("completed")
    COMPLETED,

    @SerialName("skipped")
    SKIPPED,
}

@Serializable
data class RemoteHabitOutcome(
    val revision: Long,
    val status: RemoteHabitOutcomeStatus,
    @SerialName("progress_basis_points") val progressBasisPoints: Int,
    val quantity: Long?,
    val unit: String?,
    @SerialName("actual_seconds") val actualSeconds: Long?,
    val note: String?,
    @SerialName("occurred_at") val occurredAt: String,
    @SerialName("updated_at") val updatedAt: String,
)

@Serializable
data class RemoteHabitOccurrenceEvidence(
    val id: String,
    @SerialName("habit_id") val habitId: String,
    @SerialName("planner_occurrence_id") val plannerOccurrenceId: String,
    @SerialName("source_schedule_revision_id") val sourceScheduleRevisionId: String,
    @SerialName("source_item_revision") val sourceItemRevision: Long,
    @SerialName("policy_fingerprint") val policyFingerprint: String,
    val identity: JsonObject,
    @SerialName("nominal_start") val nominalStart: String,
    @SerialName("nominal_end") val nominalEnd: String,
    @SerialName("window_start") val windowStart: String,
    @SerialName("window_end") val windowEnd: String,
    @SerialName("local_date") val localDate: String,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("expected_duration_seconds") val expectedDurationSeconds: Long?,
    @SerialName("expected_quantity") val expectedQuantity: Long?,
    @SerialName("expected_unit") val expectedUnit: String?,
)

@Serializable
data class RemoteHabitOccurrence(
    val evidence: RemoteHabitOccurrenceEvidence,
    val outcome: RemoteHabitOutcome?,
)

@Serializable
data class RemoteHabitPause(
    val id: String,
    @SerialName("habit_id") val habitId: String,
    val revision: Long,
    @SerialName("started_at") val startedAt: String,
    @SerialName("ended_at") val endedAt: String?,
    @SerialName("preserves_streak") val preservesStreak: Boolean,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
)

@Serializable
sealed class RemoteHabitDeltaChange {
    @Serializable
    @SerialName("occurrence_upsert")
    data class OccurrenceUpsert(
        val occurrence: RemoteHabitOccurrence,
    ) : RemoteHabitDeltaChange()

    @Serializable
    @SerialName("pause_upsert")
    data class PauseUpsert(
        val pause: RemoteHabitPause,
    ) : RemoteHabitDeltaChange()
}

@Serializable
enum class RemoteHabitAnalyticsBucket {
    @SerialName("day")
    DAY,

    @SerialName("week")
    WEEK,

    @SerialName("month")
    MONTH,
}

@Serializable
enum class RemoteHabitSupportiveFactCode {
    @SerialName("no_data")
    NO_DATA,

    @SerialName("active_streak")
    ACTIVE_STREAK,

    @SerialName("strong_adherence")
    STRONG_ADHERENCE,

    @SerialName("fresh_start_available")
    FRESH_START_AVAILABLE,
}

@Serializable
data class RemoteHabitQuantityTotal(
    val unit: String,
    val amount: Long,
)

@Serializable
data class RemoteHabitTrendBucket(
    @SerialName("start_date") val startDate: String,
    @SerialName("end_date") val endDate: String,
    val expected: Long,
    val eligible: Long,
    val completed: Long,
    val partial: Long,
    val skipped: Long,
    val missed: Long,
    val excused: Long,
    val unresolved: Long,
    @SerialName("adherence_basis_points") val adherenceBasisPoints: Int,
    @SerialName("actual_seconds_total") val actualSecondsTotal: Long,
    @SerialName("quantity_totals") val quantityTotals: List<RemoteHabitQuantityTotal>,
)

@Serializable
data class RemoteHabitAnalytics(
    @SerialName("habit_id") val habitId: String,
    @SerialName("start_date") val startDate: String,
    @SerialName("end_date") val endDate: String,
    val bucket: RemoteHabitAnalyticsBucket,
    val expected: Long,
    val eligible: Long,
    val completed: Long,
    val partial: Long,
    val skipped: Long,
    val missed: Long,
    val excused: Long,
    val unresolved: Long,
    @SerialName("adherence_basis_points") val adherenceBasisPoints: Int,
    @SerialName("actual_seconds_total") val actualSecondsTotal: Long,
    @SerialName("quantity_totals") val quantityTotals: List<RemoteHabitQuantityTotal>,
    @SerialName("current_streak") val currentStreak: Int,
    @SerialName("longest_streak") val longestStreak: Int,
    val trends: List<RemoteHabitTrendBucket>,
    @SerialName("supportive_fact_codes")
    val supportiveFactCodes: List<RemoteHabitSupportiveFactCode>,
)

data class RemoteHabitOccurrencePage(
    val occurrences: List<RemoteHabitOccurrence>,
    val nextCursor: String?,
    val hasMore: Boolean,
)

data class RemoteHabitDeltaPage(
    val changes: List<RemoteHabitDeltaChange>,
    val nextCursor: String,
    val hasMore: Boolean,
)

data class RemoteHabitMutation<T>(
    val value: T,
    val replayed: Boolean,
)

@Serializable
private data class HabitOccurrenceEnvelope(
    val occurrence: RemoteHabitOccurrence,
    val replayed: Boolean,
)

@Serializable
private data class HabitOccurrenceListEnvelope(
    val occurrences: List<RemoteHabitOccurrence>,
    @SerialName("next_cursor") val nextCursor: String?,
    @SerialName("has_more") val hasMore: Boolean,
)

@Serializable
private data class HabitPauseEnvelope(
    val pause: RemoteHabitPause,
    val replayed: Boolean,
)

@Serializable
private data class HabitDeltaEnvelope(
    val changes: List<RemoteHabitDeltaChange>,
    @SerialName("next_cursor") val nextCursor: String,
    @SerialName("has_more") val hasMore: Boolean,
)

@Serializable
private data class HabitAnalyticsEnvelope(
    val analytics: RemoteHabitAnalytics,
)

sealed class HabitApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : HabitApiException("The DayWeave API rejected the bearer token")

    class NotFound : HabitApiException("The habit or occurrence was not found")

    class Conflict : HabitApiException("The habit changed on another device")

    class Validation(val statusCode: Int) : HabitApiException(
        "The DayWeave API rejected a habit request with HTTP $statusCode",
    )

    class Http(val statusCode: Int) : HabitApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : HabitApiException(
        "The DayWeave API returned an unreadable habit response",
        cause,
    )
}

interface HabitTransport {
    suspend fun listOccurrences(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        cursor: String? = null,
        limit: Int = 200,
    ): RemoteHabitOccurrencePage

    /** [requestJson] is the exact encrypted outbox body and must not be re-encoded on retry. */
    suspend fun putOutcome(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        occurrenceId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitOccurrence>

    suspend fun delta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String? = null,
        limit: Int = 200,
    ): RemoteHabitDeltaPage

    /** [requestJson] is the exact encrypted outbox body and must not be re-encoded on retry. */
    suspend fun startPause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause>

    /** [requestJson] is the exact encrypted outbox body and must not be re-encoded on retry. */
    suspend fun resumePause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        pauseId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause>

    suspend fun analytics(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        bucket: RemoteHabitAnalyticsBucket,
    ): RemoteHabitAnalytics
}

class OkHttpHabitTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
    private val json: Json = Json {
        ignoreUnknownKeys = false
        explicitNulls = true
        encodeDefaults = true
        classDiscriminator = "type"
    },
) : HabitTransport {
    override suspend fun listOccurrences(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        cursor: String?,
        limit: Int,
    ): RemoteHabitOccurrencePage {
        requireDateRange(startDate, endDate)
        require(limit in 1..MAX_PAGE_LIMIT)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/habits")
            .addPathSegment(habitId.requireCanonicalUuid())
            .addPathSegment("occurrences")
            .addQueryParameter("start_date", startDate.toString())
            .addQueryParameter("end_date", endDate.toString())
            .apply { cursor?.let { addQueryParameter("cursor", it.requireCursor()) } }
            .addQueryParameter("limit", limit.toString())
            .build()
        val envelope = execute<HabitOccurrenceListEnvelope>(
            requestBuilder(configuration, url.toString()).get().build(),
        )
        return validatedHabitResponse {
            require(envelope.occurrences.size <= limit)
            require(envelope.hasMore == (envelope.nextCursor != null))
            envelope.occurrences.forEach { it.requireValid(habitId) }
            envelope.nextCursor?.requireCursor()
            RemoteHabitOccurrencePage(
                occurrences = envelope.occurrences,
                nextCursor = envelope.nextCursor,
                hasMore = envelope.hasMore,
            )
        }
    }

    override suspend fun putOutcome(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        occurrenceId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitOccurrence> {
        val canonicalHabitId = habitId.requireCanonicalUuid()
        val canonicalOccurrenceId = occurrenceId.requireCanonicalUuid()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/habits")
            .addPathSegment(canonicalHabitId)
            .addPathSegment("occurrences")
            .addPathSegment(canonicalOccurrenceId)
            .build()
        val envelope = execute<HabitOccurrenceEnvelope>(
            mutationRequest(configuration, url.toString(), "PUT", idempotencyKey, requestJson),
        )
        return validatedHabitResponse {
            envelope.occurrence.requireValid(canonicalHabitId)
            require(envelope.occurrence.evidence.id == canonicalOccurrenceId)
            RemoteHabitMutation(envelope.occurrence, envelope.replayed)
        }
    }

    override suspend fun delta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
        limit: Int,
    ): RemoteHabitDeltaPage {
        require(limit in 1..MAX_PAGE_LIMIT)
        val previousCursor = cursor?.requireCursor()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/habits/occurrences/delta")
            .apply { previousCursor?.let { addQueryParameter("cursor", it) } }
            .addQueryParameter("limit", limit.toString())
            .build()
        val envelope = execute<HabitDeltaEnvelope>(
            requestBuilder(configuration, url.toString()).get().build(),
        )
        return validatedHabitResponse {
            require(envelope.changes.size <= limit)
            envelope.nextCursor.requireCursor()
            if (previousCursor != null) {
                require(envelope.nextCursor != previousCursor || !envelope.hasMore)
            }
            envelope.changes.forEach { change ->
                when (change) {
                    is RemoteHabitDeltaChange.OccurrenceUpsert ->
                        change.occurrence.requireValid(change.occurrence.evidence.habitId)
                    is RemoteHabitDeltaChange.PauseUpsert ->
                        change.pause.requireValid(change.pause.habitId)
                }
            }
            RemoteHabitDeltaPage(envelope.changes, envelope.nextCursor, envelope.hasMore)
        }
    }

    override suspend fun startPause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        val canonicalHabitId = habitId.requireCanonicalUuid()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/habits")
            .addPathSegment(canonicalHabitId)
            .addPathSegment("pauses")
            .build()
        return pauseMutation(
            configuration,
            canonicalHabitId,
            url.toString(),
            idempotencyKey,
            requestJson,
        )
    }

    override suspend fun resumePause(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        pauseId: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        val canonicalHabitId = habitId.requireCanonicalUuid()
        val canonicalPauseId = pauseId.requireCanonicalUuid()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/habits")
            .addPathSegment(canonicalHabitId)
            .addPathSegment("pauses")
            .addPathSegment(canonicalPauseId)
            .addPathSegment("resume")
            .build()
        val mutation = pauseMutation(
            configuration,
            canonicalHabitId,
            url.toString(),
            idempotencyKey,
            requestJson,
        )
        return validatedHabitResponse {
            require(mutation.value.id == canonicalPauseId)
            mutation
        }
    }

    override suspend fun analytics(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        startDate: LocalDate,
        endDate: LocalDate,
        bucket: RemoteHabitAnalyticsBucket,
    ): RemoteHabitAnalytics {
        requireDateRange(startDate, endDate)
        val canonicalHabitId = habitId.requireCanonicalUuid()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/habits")
            .addPathSegment(canonicalHabitId)
            .addPathSegment("analytics")
            .addQueryParameter("start_date", startDate.toString())
            .addQueryParameter("end_date", endDate.toString())
            .addQueryParameter("bucket", bucket.name.lowercase())
            .build()
        val value = execute<HabitAnalyticsEnvelope>(
            requestBuilder(configuration, url.toString()).get().build(),
        ).analytics
        return validatedHabitResponse {
            value.requireValid(canonicalHabitId, startDate, endDate, bucket)
            value
        }
    }

    private suspend fun pauseMutation(
        configuration: AuthenticatedApiConfiguration,
        habitId: String,
        url: String,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteHabitMutation<RemoteHabitPause> {
        val envelope = execute<HabitPauseEnvelope>(
            mutationRequest(configuration, url, "POST", idempotencyKey, requestJson),
        )
        return validatedHabitResponse {
            envelope.pause.requireValid(habitId)
            RemoteHabitMutation(envelope.pause, envelope.replayed)
        }
    }

    private fun mutationRequest(
        configuration: AuthenticatedApiConfiguration,
        url: String,
        method: String,
        idempotencyKey: String,
        requestJson: String,
    ): Request {
        idempotencyKey.requireCanonicalUuid()
        if (requestJson.length !in 2..MAX_REQUEST_CHARS) throw HabitApiException.InvalidResponse()
        val body = requestJson.toRequestBody(JSON_MEDIA_TYPE)
        return requestBuilder(configuration, url)
            .header("Idempotency-Key", idempotencyKey)
            .method(method, body)
            .build()
    }

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")

    private suspend inline fun <reified T> execute(request: Request): T {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw HabitApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != 200) throw response.toHabitApiException()
            val responseText = response.body.charStream().use { it.readBoundedHabitText() }
            try {
                return json.decodeFromString<T>(responseText)
            } catch (error: SerializationException) {
                throw HabitApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw HabitApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toHabitApiException(): HabitApiException = when (code) {
        401 -> HabitApiException.Authentication()
        404 -> HabitApiException.NotFound()
        409 -> HabitApiException.Conflict()
        400, 422 -> HabitApiException.Validation(code)
        else -> HabitApiException.Http(code)
    }

    private fun Reader.readBoundedHabitText(): String {
        val result = StringBuilder()
        val buffer = CharArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val read = read(buffer)
            if (read < 0) break
            if (result.length + read > MAX_RESPONSE_CHARS) {
                throw HabitApiException.InvalidResponse()
            }
            result.append(buffer, 0, read)
        }
        return result.toString()
    }

    private companion object {
        const val MAX_PAGE_LIMIT = 200
        const val MAX_REQUEST_CHARS = 64 * 1024
        const val MAX_RESPONSE_CHARS = 2 * 1024 * 1024
        val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
    }
}

private fun RemoteHabitOccurrence.requireValid(expectedHabitId: String) {
    evidence.requireValid(expectedHabitId)
    outcome?.requireValid()
}

private fun RemoteHabitOccurrenceEvidence.requireValid(expectedHabitId: String) {
    id.requireCanonicalUuid()
    habitId.requireCanonicalUuid()
    require(habitId == expectedHabitId)
    plannerOccurrenceId.requireCanonicalUuid()
    sourceScheduleRevisionId.requireCanonicalUuid()
    require(sourceItemRevision > 0)
    require(policyFingerprint.matches(Regex("sha256:[0-9a-f]{64}")))
    require(identity.size in 1..32)
    val nominalStartInstant = requireInstant(nominalStart)
    val nominalEndInstant = requireInstant(nominalEnd)
    val windowStartInstant = requireInstant(windowStart)
    val windowEndInstant = requireInstant(windowEnd)
    require(nominalStartInstant < nominalEndInstant)
    require(windowStartInstant < windowEndInstant)
    require(LocalDate.parse(localDate).toString() == localDate)
    require(timezoneName.length in 1..100 && timezoneName.none(Char::isISOControl))
    expectedDurationSeconds?.let { require(it in 1..MAX_EXPECTED_SECONDS) }
    expectedQuantity?.let { require(it in 1..MAX_QUANTITY) }
    require(expectedQuantity == null == (expectedUnit == null))
    expectedUnit?.let(::requireUnit)
}

private fun RemoteHabitOutcome.requireValid() {
    require(revision > 0)
    require(progressBasisPoints in 0..10_000)
    when (status) {
        RemoteHabitOutcomeStatus.UNRESOLVED -> require(
            progressBasisPoints == 0 && quantity == null && unit == null &&
                actualSeconds == null && note == null,
        )
        RemoteHabitOutcomeStatus.PARTIAL -> require(progressBasisPoints in 1..9_999)
        RemoteHabitOutcomeStatus.COMPLETED -> require(progressBasisPoints == 10_000)
        RemoteHabitOutcomeStatus.SKIPPED -> require(
            progressBasisPoints == 0 && quantity == null && unit == null && actualSeconds == null,
        )
    }
    require(quantity == null == (unit == null))
    quantity?.let { require(it in 0..MAX_QUANTITY) }
    unit?.let(::requireUnit)
    actualSeconds?.let { require(it in 0..MAX_EXPECTED_SECONDS) }
    note?.let { requireText(it, MAX_NOTE_CHARS, multiline = true) }
    requireInstant(occurredAt)
    requireInstant(updatedAt)
}

private fun RemoteHabitPause.requireValid(expectedHabitId: String) {
    id.requireCanonicalUuid()
    habitId.requireCanonicalUuid()
    require(habitId == expectedHabitId)
    require(revision > 0)
    val start = requireInstant(startedAt)
    val end = endedAt?.let(::requireInstant)
    val created = requireInstant(createdAt)
    val updated = requireInstant(updatedAt)
    require(end == null || end > start)
    require(created <= updated)
}

private fun RemoteHabitAnalytics.requireValid(
    expectedHabitId: String,
    requestedStart: LocalDate,
    requestedEnd: LocalDate,
    requestedBucket: RemoteHabitAnalyticsBucket,
) {
    habitId.requireCanonicalUuid()
    require(habitId == expectedHabitId)
    require(LocalDate.parse(startDate) == requestedStart)
    require(LocalDate.parse(endDate) == requestedEnd)
    require(bucket == requestedBucket)
    requireTotalsValid(
        expected,
        eligible,
        completed,
        partial,
        skipped,
        missed,
        excused,
        unresolved,
        adherenceBasisPoints,
        actualSecondsTotal,
        quantityTotals,
    )
    require(currentStreak >= 0 && longestStreak >= currentStreak)
    require(trends.size <= MAX_TREND_BUCKETS)
    var previousEnd: LocalDate? = null
    trends.forEach { trend ->
        val trendStart = LocalDate.parse(trend.startDate)
        val trendEnd = LocalDate.parse(trend.endDate)
        require(trendStart <= trendEnd && trendStart >= requestedStart && trendEnd <= requestedEnd)
        previousEnd?.let { require(trendStart > it) }
        previousEnd = trendEnd
        requireTotalsValid(
            trend.expected,
            trend.eligible,
            trend.completed,
            trend.partial,
            trend.skipped,
            trend.missed,
            trend.excused,
            trend.unresolved,
            trend.adherenceBasisPoints,
            trend.actualSecondsTotal,
            trend.quantityTotals,
        )
    }
    require(supportiveFactCodes.size <= RemoteHabitSupportiveFactCode.entries.size)
    require(supportiveFactCodes.distinct().size == supportiveFactCodes.size)
}

@Suppress("LongParameterList")
private fun requireTotalsValid(
    expected: Long,
    eligible: Long,
    completed: Long,
    partial: Long,
    skipped: Long,
    missed: Long,
    excused: Long,
    unresolved: Long,
    adherenceBasisPoints: Int,
    actualSecondsTotal: Long,
    quantityTotals: List<RemoteHabitQuantityTotal>,
) {
    listOf(expected, eligible, completed, partial, skipped, missed, excused, unresolved)
        .forEach { require(it >= 0) }
    require(eligible <= expected)
    require(excused <= expected)
    require(completed + partial + skipped + missed + unresolved == eligible)
    require(eligible + excused == expected)
    require(adherenceBasisPoints in 0..10_000)
    require(actualSecondsTotal >= 0)
    require(quantityTotals.size <= MAX_QUANTITY_TOTALS)
    require(quantityTotals.map { it.unit }.distinct().size == quantityTotals.size)
    quantityTotals.forEach {
        requireUnit(it.unit)
        require(it.amount >= 0)
    }
}

private fun requireDateRange(startDate: LocalDate, endDate: LocalDate) {
    require(!endDate.isBefore(startDate))
    require(endDate.toEpochDay() - startDate.toEpochDay() < 366)
}

private fun String.requireCanonicalUuid(): String {
    val parsed = runCatching { UUID.fromString(this) }.getOrNull()
    if (parsed == null || parsed == UUID(0L, 0L) || parsed.toString() != this) {
        throw HabitApiException.InvalidResponse()
    }
    return this
}

private inline fun <T> validatedHabitResponse(block: () -> T): T = try {
    block()
} catch (error: HabitApiException) {
    throw error
} catch (error: IllegalArgumentException) {
    throw HabitApiException.InvalidResponse(error)
}

private fun String.requireCursor(): String {
    if (length !in 1..512 || any { it.isISOControl() || it.isWhitespace() }) {
        throw HabitApiException.InvalidResponse()
    }
    return this
}

private fun requireInstant(value: String): Instant {
    val parsed = Instant.parse(value)
    require(parsed.toString() == value)
    return parsed
}

private fun requireUnit(value: String) = requireText(value, 200, multiline = false)

private fun requireText(value: String, maxChars: Int, multiline: Boolean) {
    require(value.isNotBlank() && value.length <= maxChars)
    require(value.none { it.isISOControl() && !(multiline && it in setOf('\n', '\r', '\t')) })
}

private const val MAX_NOTE_CHARS = 10_000
private const val MAX_QUANTITY = 1_000_000_000_000L
private const val MAX_EXPECTED_SECONDS = 366L * 24 * 60 * 60
private const val MAX_TREND_BUCKETS = 366
private const val MAX_QUANTITY_TOTALS = 200
