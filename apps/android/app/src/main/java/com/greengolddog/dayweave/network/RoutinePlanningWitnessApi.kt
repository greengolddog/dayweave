package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import java.io.ByteArrayOutputStream
import java.io.IOException
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
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.json.*
import okhttp3.Call
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

enum class RoutinePlanningWitnessFailureCode(val wire: String, val status: Int) {
    INVALID("routine_planning_invalid", 422),
    TOO_LARGE("routine_planning_too_large", 413),
    SOURCE_CHANGED("routine_planning_source_changed", 409),
    CURSOR_CHANGED("routine_planning_cursor_changed", 409),
    UNAVAILABLE("service_unavailable", 503),
}

sealed class RoutinePlanningWitnessApiException(message: String) : IOException(message) {
    class Rejected(val code: RoutinePlanningWitnessFailureCode) : RoutinePlanningWitnessApiException("Planning evidence requires review")
    class Authentication : RoutinePlanningWitnessApiException("Planning authentication is unavailable")
    class Uncertain : RoutinePlanningWitnessApiException("Planning response could not be verified")
}

fun interface RoutinePlanningWitnessTransport {
    suspend fun capture(configuration: AuthenticatedApiConfiguration, request: RoutinePlanningWitnessRequest): RoutinePlanningWitnessResponse
}

/** Read-only POST: no operation receipt or replay header is ever admitted. */
class OkHttpRoutinePlanningWitnessTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
) : RoutinePlanningWitnessTransport {
    override suspend fun capture(configuration: AuthenticatedApiConfiguration, request: RoutinePlanningWitnessRequest): RoutinePlanningWitnessResponse = withContext(Dispatchers.IO) {
        val context = currentCoroutineContext(); context.ensureActive()
        try {
            val bytes = encodeRoutinePlanningWitnessRequest(request)
            context.ensureActive()
            val http = Request.Builder()
                .url(configuration.baseUrl.newBuilder().addPathSegments("v1/routine-occurrences/planning-witness").build())
                .tag(AuthenticatedApiConfiguration::class.java, configuration)
                .header("Accept", "application/json")
                .header("Cache-Control", "no-store, max-age=0")
                .header("Authorization", "Bearer ${configuration.bearerToken}")
                .post(bytes.toRequestBody("application/json; charset=utf-8".toMediaType())).build()
            val response = decodeExactRoutinePlanningWitness<RoutinePlanningWitnessResponse>(execute(configuration, http))
            response.requireValid()
            (response.result as? RoutinePlanningWitnessResult.Qualified)?.witness?.let { witness ->
                require(witness.sourceItemRevisions == request.expectedSourceItemRevisions && witness.terminalCursor == request.terminalCursor)
                witness.schedule.requireImmutableInput(request.schedule)
            }
            // Owner IDs and complete current-source membership are joined by the
            // operation owner before custody. Configuration itself has no owner IDs.
            context.ensureActive()
            response
        } catch (error: CancellationException) { throw error }
        catch (error: RoutinePlanningWitnessApiException) { context.ensureActive(); throw error }
        catch (_: Exception) { context.ensureActive(); throw RoutinePlanningWitnessApiException.Uncertain() }
    }

    @OptIn(InternalCoroutinesApi::class)
    private suspend fun execute(configuration: AuthenticatedApiConfiguration, request: Request): String {
        val context = currentCoroutineContext(); context.ensureActive()
        val active = AtomicReference<Call?>()
        val cancellation = context.job.invokeOnCompletion(onCancelling = true, invokeImmediately = true) { cause ->
            if (cause is CancellationException) active.get()?.cancel()
        }
        try {
            configuration.executeAuthenticatedCancellable(client, request) { call ->
                active.set(call); if (!context.isActive) call.cancel()
            }.use { response ->
                context.ensureActive()
                val directives = response.headers.values("Cache-Control").flatMap { it.lowercase(Locale.ROOT).split(',') }.map(String::trim)
                require("no-store" in directives && "max-age=0" in directives)
                require(response.headers.values("Idempotency-Replayed").isEmpty())
                val media = response.headers.values("Content-Type").singleOrNull()?.toMediaTypeOrNull()
                require(media?.type == "application" && media.subtype == "json" && media.charset(StandardCharsets.UTF_8) == StandardCharsets.UTF_8)
                require(response.body.contentLength() <= MAX_ROUTINE_PLANNING_WITNESS_BYTES)
                val output = BoundedWitnessBytes()
                response.body.byteStream().use { stream ->
                    val buffer = ByteArray(8192)
                    while (true) {
                        context.ensureActive()
                        val count = stream.read(buffer); context.ensureActive()
                        if (count < 0) break
                        output.write(buffer, 0, count)
                    }
                }
                val body = decodePlanningUtf8(output.toByteArray())
                context.ensureActive()
                if (response.code == 200) return body
                if (response.code == 401 || response.code == 403) throw RoutinePlanningWitnessApiException.Authentication()
                requirePlanningJson(body)
                val envelope = ROUTINE_PLANNING_JSON.parseToJsonElement(body).jsonObject
                require(envelope.keys == setOf("error"))
                val error = envelope.getValue("error").jsonObject
                require(error.keys == setOf("code", "message"))
                val code = error.getValue("code").jsonPrimitive.also { require(it.isString) }.content
                require(error.getValue("message").jsonPrimitive.isString)
                val known = RoutinePlanningWitnessFailureCode.entries.singleOrNull { it.wire == code && it.status == response.code }
                context.ensureActive()
                if (known != null) throw RoutinePlanningWitnessApiException.Rejected(known)
                throw RoutinePlanningWitnessApiException.Uncertain()
            }
        } finally {
            active.set(null); cancellation.dispose()
        }
    }
}

/** The returned bytes can be frozen as encrypted original-request custody by the operation owner. */
@OptIn(ExperimentalSerializationApi::class)
internal fun encodeRoutinePlanningWitnessRequest(request: RoutinePlanningWitnessRequest): ByteArray {
    try {
        request.requireValid()
        val output = BoundedWitnessBytes()
        ROUTINE_PLANNING_JSON.encodeToStream(RoutinePlanningWitnessRequest.serializer(), request, output)
        return output.toByteArray().also { requirePlanningJson(decodePlanningUtf8(it)) }
    } catch (_: Exception) { throw RoutinePlanningWitnessProtocolException() }
}

private class BoundedWitnessBytes : ByteArrayOutputStream(65_536) {
    override fun write(value: Int) { require(count < MAX_ROUTINE_PLANNING_WITNESS_BYTES); super.write(value) }
    override fun write(bytes: ByteArray, offset: Int, length: Int) {
        require(offset >= 0 && length >= 0 && offset <= bytes.size - length && length <= MAX_ROUTINE_PLANNING_WITNESS_BYTES - count)
        super.write(bytes, offset, length)
    }
}
