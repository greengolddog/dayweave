package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.util.Locale
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*
import okhttp3.Call
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

enum class RoutineOccurrenceFailureCode(val wire: String, val status: Int) {
    INVALID("routine_occurrence_invalid", 422),
    TOO_LARGE("routine_occurrence_too_large", 413),
    DEFINITION_CHANGED("routine_occurrence_definition_changed", 409),
    SOURCE_INELIGIBLE("routine_occurrence_source_ineligible", 409),
    INSTANCE_STALE("routine_occurrence_instance_stale", 409),
    MEMBER_STALE("routine_occurrence_member_stale", 409),
    EVIDENCE_STALE("routine_occurrence_evidence_stale", 409),
    MEMBER_MISSING("routine_occurrence_member_missing", 404),
    OCCURRENCE_MISSING("routine_occurrence_missing", 404),
    OPERATION_REUSED("routine_occurrence_operation_reused", 409),
    INVALID_CURSOR("routine_occurrence_invalid_cursor", 409),
    LEAF_REQUIRED("routine_occurrence_leaf_required", 422),
    PARENT_REQUIRED("routine_occurrence_parent_required", 422),
    OCCURRENCE_EVIDENCE_REQUIRED("routine_occurrence_evidence_required", 409),
    EXECUTION_CONFLICT("routine_occurrence_execution_conflict", 409),
}

sealed class RoutineOccurrenceApiException(message: String) : IOException(message) {
    class Definitive(val code: RoutineOccurrenceFailureCode) : RoutineOccurrenceApiException("Occurrence requires review")
    class Authentication : RoutineOccurrenceApiException("Occurrence authentication is unavailable")
    class Uncertain : RoutineOccurrenceApiException("Occurrence response could not be verified")
}

interface RoutineOccurrenceTransport {
    suspend fun lookup(configuration: AuthenticatedApiConfiguration, seriesItemId: String, occurrenceId: String): RoutineOccurrenceSnapshot
    suspend fun get(configuration: AuthenticatedApiConfiguration, instanceId: String): RoutineOccurrenceSnapshot
    suspend fun put(configuration: AuthenticatedApiConfiguration, instanceId: String, memberId: String, requestJson: String): RoutineOccurrenceMutationResult
    suspend fun list(configuration: AuthenticatedApiConfiguration, cursor: String? = null, limit: Int = 50): RoutineOccurrencePage
    suspend fun delta(configuration: AuthenticatedApiConfiguration, cursor: String? = null, limit: Int = 50): RoutineOccurrencePage
}

/** This client never installs pages or interprets an opaque cursor as local proof. */
class OkHttpRoutineOccurrenceTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
) : RoutineOccurrenceTransport {
    override suspend fun lookup(configuration: AuthenticatedApiConfiguration, seriesItemId: String, occurrenceId: String): RoutineOccurrenceSnapshot = admittedIO {
        verify {
            requireCanonicalUuid(seriesItemId, "occurrence series")
            requireCanonicalUuid(occurrenceId, "planner occurrence")
            require(java.util.UUID.fromString(occurrenceId).version() == 5)
        }
        val builder = request(configuration, "lookup")
        val url = builder.build().url.newBuilder().addQueryParameter("series_item_id", seriesItemId)
            .addQueryParameter("occurrence_id", occurrenceId).build()
        val (body, _) = execute(configuration, builder.url(url).get().build())
        decode<RoutineOccurrenceSnapshot>(body).also { snapshot ->
            verify { snapshot.requireValid(); require(snapshot.aggregate.manifest.seriesItemId == seriesItemId &&
                snapshot.aggregate.manifest.occurrenceId == occurrenceId) }
        }
    }

    override suspend fun get(configuration: AuthenticatedApiConfiguration, instanceId: String): RoutineOccurrenceSnapshot = admittedIO {
        verify { requireCanonicalUuid(instanceId, "occurrence instance") }
        val (body, _) = execute(configuration, request(configuration, instanceId).get().build())
        decode<RoutineOccurrenceSnapshot>(body).also {
            verify { it.requireValid(); require(it.aggregate.manifest.id == instanceId) }
        }
    }

    override suspend fun put(configuration: AuthenticatedApiConfiguration, instanceId: String, memberId: String, requestJson: String): RoutineOccurrenceMutationResult = admittedIO {
        verify {
            requireCanonicalUuid(instanceId, "occurrence instance")
            requireCanonicalUuid(memberId, "occurrence member")
            require(requestJson.toByteArray(Charsets.UTF_8).size <= MAX_ROUTINE_OCCURRENCE_REQUEST_BYTES)
        }
        val expected = decode<RoutineOccurrenceRequest>(requestJson).also { verify { it.requireValid(memberId) } }
        val request = request(configuration, instanceId, "members", memberId)
            .put(requestJson.toRequestBody(JSON_MEDIA_TYPE)).build()
        val (body, replay) = execute(configuration, request)
        decode<RoutineOccurrenceMutationResult>(body).also {
            verify { require(replay == it.replayed.toString()); it.requireMatches(instanceId, memberId, expected) }
        }
    }

    override suspend fun list(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int): RoutineOccurrencePage =
        page(configuration, cursor, limit, current = true)

    override suspend fun delta(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int): RoutineOccurrencePage =
        page(configuration, cursor, limit, current = false)

    private suspend fun page(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int, current: Boolean): RoutineOccurrencePage = admittedIO {
        verify { require(limit in 1..100); cursor?.let(::requireRoutineCursor) }
        val path = if (current) emptyArray() else arrayOf("delta")
        val builder = request(configuration, *path)
        val url = builder.build().url.newBuilder().addQueryParameter("limit", limit.toString())
        cursor?.let { url.addQueryParameter("cursor", it) }
        val (body, _) = execute(configuration, builder.url(url.build()).get().build())
        decode<RoutineOccurrencePage>(body).also {
            verify { it.requireValid(current, limit); require(it.changes.isEmpty() && !it.hasMore || it.cursor != cursor) }
        }
    }

    private fun request(configuration: AuthenticatedApiConfiguration, vararg path: String): Request.Builder {
        val url = configuration.baseUrl.newBuilder().addPathSegments("v1/routine-occurrences")
        path.forEach(url::addPathSegment)
        return Request.Builder().url(url.build())
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", "application/json")
            .header("Cache-Control", "no-store, max-age=0")
            .header("Authorization", "Bearer ${configuration.bearerToken}")
    }

    @OptIn(InternalCoroutinesApi::class)
    private suspend fun execute(configuration: AuthenticatedApiConfiguration, request: Request): Pair<String, String?> {
        val context = currentCoroutineContext()
        context.ensureActive()
        val activeCall = AtomicReference<Call?>()
        val cancellationHandle = context.job.invokeOnCompletion(onCancelling = true, invokeImmediately = true) { cause ->
            if (cause is CancellationException) activeCall.get()?.cancel()
        }
        try {
            configuration.executeAuthenticatedCancellable(client, request) { call ->
                activeCall.set(call)
                // Cancellation can race an authenticated retry's replacement Call.
                if (!context.isActive) call.cancel()
            }.use { response ->
                context.ensureActive()
                val directives = response.headers.values("Cache-Control").flatMap { it.lowercase(Locale.ROOT).split(',') }.map(String::trim)
                if ("no-store" !in directives || "max-age=0" !in directives) throw RoutineOccurrenceApiException.Uncertain()
                // Pragma is optional: this endpoint's admission relies on Cache-Control.
                val media = response.headers.values("Content-Type").singleOrNull()?.toMediaTypeOrNull()
                if (media?.type != "application" || media.subtype != "json" || media.charset(StandardCharsets.UTF_8) != StandardCharsets.UTF_8) {
                    throw RoutineOccurrenceApiException.Uncertain()
                }
                val length = response.body.contentLength()
                if (length > MAX_ROUTINE_OCCURRENCE_BYTES) throw RoutineOccurrenceApiException.Uncertain()
                val bytes = ByteArrayOutputStream()
                response.body.byteStream().use { stream ->
                    val buffer = ByteArray(8_192)
                    while (true) {
                        context.ensureActive()
                        val count = stream.read(buffer)
                        context.ensureActive()
                        if (count < 0) break
                        if (bytes.size() > MAX_ROUTINE_OCCURRENCE_BYTES - count) throw RoutineOccurrenceApiException.Uncertain()
                        bytes.write(buffer, 0, count)
                    }
                }
                val body = try {
                    StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
                        .decode(ByteBuffer.wrap(bytes.toByteArray())).toString()
                } catch (_: java.nio.charset.CharacterCodingException) { throw RoutineOccurrenceApiException.Uncertain() }
                context.ensureActive()
                val replay = response.headers.values("Idempotency-Replayed")
                if (response.code != 200) {
                    if (replay.isNotEmpty()) throw RoutineOccurrenceApiException.Uncertain()
                    if (response.code == 401 || response.code == 403) throw RoutineOccurrenceApiException.Authentication()
                    val code = runCatching {
                        requireStrictItemProgressJson(body)
                        val envelope = ITEM_PROGRESS_JSON.parseToJsonElement(body).jsonObject
                        require(envelope.keys == setOf("error"))
                        val error = envelope.getValue("error").jsonObject
                        require(error.keys == setOf("code", "message"))
                        require(error.getValue("message").jsonPrimitive.isString)
                        error.getValue("code").jsonPrimitive.also { require(it.isString) }.content
                    }.getOrNull()
                    val known = RoutineOccurrenceFailureCode.entries.singleOrNull { it.wire == code && it.status == response.code }
                    context.ensureActive()
                    if (known != null) throw RoutineOccurrenceApiException.Definitive(known)
                    throw RoutineOccurrenceApiException.Uncertain()
                }
                if (request.method == "PUT" && replay.singleOrNull() !in setOf("true", "false") ||
                    request.method == "GET" && replay.isNotEmpty()) throw RoutineOccurrenceApiException.Uncertain()
                context.ensureActive()
                return body to replay.singleOrNull()
            }
        } catch (error: IOException) {
            context.ensureActive()
            throw error
        } finally {
            activeCall.set(null)
            cancellationHandle.dispose()
        }
    }

    /** Body reads and strict decoding never block the caller's UI dispatcher. */
    private suspend fun <T> admittedIO(block: suspend () -> T): T = withContext(Dispatchers.IO) {
        val context = currentCoroutineContext()
        context.ensureActive()
        try {
            val result = block()
            context.ensureActive()
            result
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            context.ensureActive()
            throw error
        }
    }

    private inline fun <reified T> decode(body: String): T = try { decodeExactRoutineOccurrence<T>(body) }
    catch (error: CancellationException) { throw error }
    catch (_: Exception) { throw RoutineOccurrenceApiException.Uncertain() }

    private inline fun verify(block: () -> Unit) {
        try { block() } catch (error: CancellationException) { throw error }
        catch (_: IllegalArgumentException) { throw RoutineOccurrenceApiException.Uncertain() }
        catch (_: IllegalStateException) { throw RoutineOccurrenceApiException.Uncertain() }
        catch (_: ArithmeticException) { throw RoutineOccurrenceApiException.Uncertain() }
        catch (_: NoSuchElementException) { throw RoutineOccurrenceApiException.Uncertain() }
        catch (_: java.time.DateTimeException) { throw RoutineOccurrenceApiException.Uncertain() }
    }

    private companion object { val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType() }
}
