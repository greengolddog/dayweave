package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.assistant.ASSISTANT_JSON
import com.greengolddog.dayweave.assistant.AssistantContext
import com.greengolddog.dayweave.assistant.AssistantContextProjector
import com.greengolddog.dayweave.assistant.AssistantHistoryMessage
import com.greengolddog.dayweave.assistant.AssistantTurnRequest
import com.greengolddog.dayweave.assistant.AssistantTurnResponse
import com.greengolddog.dayweave.assistant.DAYWEAVE_ASSISTANT_CONTEXT_SCHEMA_V1
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_CONTEXT_BYTES
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_HISTORY_BYTES
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_HISTORY_ENTRIES
import com.greengolddog.dayweave.assistant.MAX_ASSISTANT_MESSAGE_BYTES
import com.greengolddog.dayweave.assistant.isValidAssistantContextText
import com.greengolddog.dayweave.assistant.isValidAssistantConversationText
import com.greengolddog.dayweave.assistant.utf8Size
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.ZoneId
import java.util.Locale
import java.util.UUID
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.job
import kotlinx.serialization.SerializationException
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.Call
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

sealed class AssistantApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : AssistantApiException("The DayWeave API rejected the bearer token")

    class Forbidden : AssistantApiException("The DayWeave API denied assistant access")

    class Validation(val statusCode: Int) : AssistantApiException(
        "The DayWeave API rejected the assistant request with HTTP $statusCode",
    )

    class RateLimited : AssistantApiException("The DayWeave assistant is temporarily rate limited")

    class Unavailable : AssistantApiException("The DayWeave assistant is temporarily unavailable")

    class Http(val statusCode: Int) : AssistantApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : AssistantApiException(
        "The DayWeave API returned an unreadable assistant response",
        cause,
    )
}

interface AssistantTransport {
    suspend fun turn(
        configuration: AuthenticatedApiConfiguration,
        request: AssistantTurnRequest,
    ): AssistantTurnResponse
}

class OkHttpAssistantTransport(
    client: OkHttpClient = defaultClient(),
) : AssistantTransport {
    private val client = client.newBuilder()
        .retryOnConnectionFailure(false)
        .followRedirects(false)
        .followSslRedirects(false)
        .build()

    @OptIn(InternalCoroutinesApi::class)
    override suspend fun turn(
        configuration: AuthenticatedApiConfiguration,
        request: AssistantTurnRequest,
    ): AssistantTurnResponse {
        validateAssistantRequest(request)
        val encoded = ASSISTANT_JSON.encodeToString(request)
        val bodyBytes = encoded.toByteArray(StandardCharsets.UTF_8)
        require(bodyBytes.size <= MAX_REQUEST_BYTES) { "Assistant request exceeds 128 KiB" }
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/assistant/turns")
            .build()
        val httpRequest = Request.Builder()
            .url(url)
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", "application/json")
            .header("Authorization", "Bearer ${configuration.bearerToken}")
            .header("Cache-Control", "no-store")
            .header("Pragma", "no-cache")
            .post(bodyBytes.toRequestBody(JSON_MEDIA_TYPE))
            .build()
        val activeCall = AtomicReference<Call?>()
        val coroutineContext = currentCoroutineContext()
        val cancellationHandle = coroutineContext.job.invokeOnCompletion(
            onCancelling = true,
            invokeImmediately = true,
        ) { cause ->
            if (cause is CancellationException) activeCall.get()?.cancel()
        }
        try {
            val response = configuration.executeAuthenticatedCancellable(
                client,
                httpRequest,
                activeCall::set,
            )
            response.use {
                if (!response.hasPrivateCacheHeaders()) {
                    throw AssistantApiException.InvalidResponse()
                }
                if (response.code != 200) throw response.toAssistantApiException()
                if (!response.hasJsonMediaType()) throw AssistantApiException.InvalidResponse()
                val responseText = response.body.byteStream().use { stream ->
                    stream.readBoundedAssistantText()
                }
                coroutineContext.ensureActive()
                val duplicateKeys = runCatching {
                    StrictAssistantJsonKeyScanner(responseText, ASSISTANT_JSON).hasDuplicateKeys()
                }.getOrElse { error ->
                    throw AssistantApiException.InvalidResponse(error)
                }
                if (duplicateKeys) throw AssistantApiException.InvalidResponse()
                return try {
                    ASSISTANT_JSON.decodeFromString<AssistantTurnResponse>(responseText).also { decoded ->
                        validateAssistantResponse(request.requestId, decoded)
                    }
                } catch (error: AssistantApiException.InvalidResponse) {
                    throw error
                } catch (error: SerializationException) {
                    throw AssistantApiException.InvalidResponse(error)
                } catch (error: IllegalArgumentException) {
                    throw AssistantApiException.InvalidResponse(error)
                }
            }
        } catch (error: IOException) {
            coroutineContext.ensureActive()
            throw error
        } finally {
            activeCall.set(null)
            cancellationHandle.dispose()
        }
    }

    private fun Response.toAssistantApiException(): AssistantApiException = when (code) {
        401 -> AssistantApiException.Authentication()
        403 -> AssistantApiException.Forbidden()
        400, 413, 422 -> AssistantApiException.Validation(code)
        429 -> AssistantApiException.RateLimited()
        502, 503, 504 -> AssistantApiException.Unavailable()
        else -> AssistantApiException.Http(code)
    }

    private fun Response.hasJsonMediaType(): Boolean {
        val values = headers.values("Content-Type")
        if (values.size != 1) return false
        val mediaType = values.single().toMediaTypeOrNull() ?: return false
        val charset = mediaType.charset()
        return mediaType.type == "application" && mediaType.subtype == "json" &&
            (charset == null || charset == StandardCharsets.UTF_8)
    }

    private fun Response.hasPrivateCacheHeaders(): Boolean {
        val cacheDirectives = headers.values("Cache-Control")
            .flatMap { it.split(',') }
            .map { it.substringBefore('=').trim().lowercase(Locale.ROOT) }
        val pragmaDirectives = headers.values("Pragma")
            .flatMap { it.split(',') }
            .map { it.trim().lowercase(Locale.ROOT) }
        return "no-store" in cacheDirectives && "no-cache" in pragmaDirectives
    }

    companion object {
        private const val MAX_REQUEST_BYTES = 128 * 1_024
        private const val MAX_RESPONSE_BYTES = 64 * 1_024
        private const val MAX_REPLY_BYTES = 32 * 1_024
        private const val MAX_MODEL_BYTES = 128
        private const val MAX_TIMESTAMP_BYTES = 64
        private val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(45, TimeUnit.SECONDS)
            .writeTimeout(30, TimeUnit.SECONDS)
            .callTimeout(60, TimeUnit.SECONDS)
            .retryOnConnectionFailure(false)
            .followRedirects(false)
            .followSslRedirects(false)
            .build()

        private fun InputStream.readBoundedAssistantText(): String {
            val output = ByteArrayOutputStream()
            val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = read(buffer)
                if (read < 0) break
                if (output.size() > MAX_RESPONSE_BYTES - read) {
                    throw AssistantApiException.InvalidResponse()
                }
                output.write(buffer, 0, read)
            }
            return try {
                StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(output.toByteArray()))
                    .toString()
            } catch (error: java.nio.charset.CharacterCodingException) {
                throw AssistantApiException.InvalidResponse(error)
            }
        }

        private fun validateAssistantRequest(request: AssistantTurnRequest) {
            requireCanonicalUuid(request.requestId, "Assistant request ID")
            require(
                request.message.isValidAssistantConversationText(MAX_ASSISTANT_MESSAGE_BYTES),
            ) {
                "Assistant message must contain at most 8 KiB"
            }
            require(request.history.size <= MAX_ASSISTANT_HISTORY_ENTRIES) {
                "Assistant history contains too many entries"
            }
            var historyBytes = 0
            request.history.forEach { entry ->
                validateHistoryEntry(entry)
                val entryBytes = entry.content.utf8Size()
                require(historyBytes <= MAX_ASSISTANT_HISTORY_BYTES - entryBytes) {
                    "Assistant history exceeds 32 KiB"
                }
                historyBytes += entryBytes
            }
            validateContext(request.context)
            require(ASSISTANT_JSON.encodeToString(request.context).utf8Size() <= MAX_ASSISTANT_CONTEXT_BYTES) {
                "Assistant context exceeds 64 KiB"
            }
        }

        private fun validateHistoryEntry(entry: AssistantHistoryMessage) {
            require(
                entry.content.isValidAssistantConversationText(MAX_ASSISTANT_MESSAGE_BYTES),
            ) { "Assistant history entry is invalid" }
        }

        private fun validateContext(context: AssistantContext) {
            require(context.schema == DAYWEAVE_ASSISTANT_CONTEXT_SCHEMA_V1)
            requireInstant(context.generatedAt)
            requireNamedTimezone(context.timezone)
            require(context.scheduledBlocks.size <= AssistantContextProjector.MAX_SCHEDULED_BLOCKS)
            require(context.privateBusySpans.size <= AssistantContextProjector.MAX_PRIVATE_BUSY_SPANS)
            require(context.plannerItems.size <= AssistantContextProjector.MAX_PLANNER_ITEMS)
            require(context.totalScheduledBlockCount >=
                context.scheduledBlocks.size + context.privateBusySpans.size)
            require(context.totalPlannerItemCount >= context.plannerItems.size)
            require(context.pendingSuggestionCount >= 0)
            require(context.omittedFields == AssistantContextProjector.OMITTED_FIELDS)

            context.scheduledBlocks.forEachIndexed { index, block ->
                require(block.reference == "block-${index + 1}")
                requireSafeText(block.title, 160, allowEmpty = true)
                requireSafeText(block.kind, 32, allowEmpty = false)
                requireInterval(block.startsAt, block.endsAt, block.durationMinutes)
                requireSafeText(block.status, 32, allowEmpty = false)
                block.project?.let { requireSafeText(it, 80, allowEmpty = true) }
                requireSafeText(block.energy, 32, allowEmpty = false)
            }
            context.privateBusySpans.forEach { span ->
                requireInterval(span.startsAt, span.endsAt, span.durationMinutes)
            }
            val itemReferences = context.plannerItems.map { it.reference }.toSet()
            require(itemReferences.size == context.plannerItems.size)
            context.plannerItems.forEachIndexed { index, item ->
                require(item.reference == "item-${index + 1}")
                require(item.parentReference == null || item.parentReference in itemReferences)
                requireSafeText(item.title, 160, allowEmpty = true)
                requireSafeText(item.kind, 32, allowEmpty = false)
                requireSafeText(item.status, 32, allowEmpty = false)
                requireNamedTimezone(item.timezone)
                require(item.durationMinutes == null || item.durationMinutes >= 0)
                item.deadlineAt?.let(::requireInstant)
                item.earliestStartAt?.let(::requireInstant)
                requireSafeText(item.splitPolicy, 80, allowEmpty = false)
                require(item.importance in 0..100 && item.urgency in 0..100)
            }
        }

        private fun validateAssistantResponse(
            expectedRequestId: String,
            response: AssistantTurnResponse,
        ) {
            requireCanonicalUuid(response.requestId, "Assistant response request ID")
            if (response.requestId != expectedRequestId) throw AssistantApiException.InvalidResponse()
            if (!response.reply.isValidAssistantConversationText(MAX_REPLY_BYTES)) {
                throw AssistantApiException.InvalidResponse()
            }
            if (
                !MODEL_NAME.matches(response.model)
            ) {
                throw AssistantApiException.InvalidResponse()
            }
            if (response.generatedAt.utf8Size() > MAX_TIMESTAMP_BYTES) {
                throw AssistantApiException.InvalidResponse()
            }
            try {
                requireInstant(response.generatedAt)
            } catch (error: IllegalArgumentException) {
                throw AssistantApiException.InvalidResponse(error)
            }
        }

        private fun requireInterval(startsAt: String, endsAt: String, durationMinutes: Int) {
            val start = requireInstant(startsAt)
            val end = requireInstant(endsAt)
            require(end > start && durationMinutes > 0)
            require(java.time.Duration.between(start, end).toMinutes() == durationMinutes.toLong())
        }

        private fun requireInstant(value: String): Instant {
            require(value.utf8Size() <= MAX_TIMESTAMP_BYTES)
            return requireNotNull(runCatching { Instant.parse(value) }.getOrNull())
        }

        private fun requireCanonicalUuid(value: String, description: String) {
            val parsed = runCatching { UUID.fromString(value) }.getOrNull()
            require(parsed != null && parsed != UUID(0, 0) && parsed.toString() == value) {
                "$description is invalid"
            }
        }

        private fun requireSafeText(value: String, maximumBytes: Int, allowEmpty: Boolean) {
            require(value.isValidAssistantContextText(maximumBytes, allowEmpty))
        }

        private fun requireNamedTimezone(value: String) {
            requireSafeText(value, 64, allowEmpty = false)
            require(value in ZoneId.getAvailableZoneIds())
            requireNotNull(runCatching { ZoneId.of(value) }.getOrNull())
        }

        private val MODEL_NAME = Regex("^[A-Za-z0-9._:-]{1,$MAX_MODEL_BYTES}$")
    }
}

/** Detects duplicate object keys, including equivalent escaped spellings, before decoding. */
private class StrictAssistantJsonKeyScanner(
    private val source: String,
    private val json: Json,
) {
    private var index = 0

    fun hasDuplicateKeys(): Boolean {
        skipWhitespace()
        val duplicate = parseValue(depth = 0)
        skipWhitespace()
        require(index == source.length)
        return duplicate
    }

    private fun parseValue(depth: Int): Boolean {
        require(depth <= MAX_DEPTH)
        skipWhitespace()
        require(index < source.length)
        return when (source[index]) {
            '{' -> parseObject(depth)
            '[' -> parseArray(depth)
            '"' -> {
                parseString()
                false
            }
            else -> {
                parsePrimitive()
                false
            }
        }
    }

    private fun parseObject(depth: Int): Boolean {
        index += 1
        skipWhitespace()
        if (takeIfPresent('}')) return false
        val keys = hashSetOf<String>()
        var duplicate = false
        while (true) {
            skipWhitespace()
            require(source.getOrNull(index) == '"')
            if (!keys.add(parseString())) duplicate = true
            skipWhitespace()
            require(takeIfPresent(':'))
            if (parseValue(depth + 1)) duplicate = true
            skipWhitespace()
            when {
                takeIfPresent('}') -> return duplicate
                takeIfPresent(',') -> Unit
                else -> throw IllegalArgumentException("Invalid JSON object")
            }
        }
    }

    private fun parseArray(depth: Int): Boolean {
        index += 1
        skipWhitespace()
        if (takeIfPresent(']')) return false
        var duplicate = false
        while (true) {
            if (parseValue(depth + 1)) duplicate = true
            skipWhitespace()
            when {
                takeIfPresent(']') -> return duplicate
                takeIfPresent(',') -> Unit
                else -> throw IllegalArgumentException("Invalid JSON array")
            }
        }
    }

    private fun parseString(): String {
        val start = index
        require(takeIfPresent('"'))
        while (index < source.length) {
            when (source[index++]) {
                '"' -> return json.decodeFromString(source.substring(start, index))
                '\\' -> {
                    require(index < source.length)
                    if (source[index++] == 'u') {
                        repeat(4) {
                            require(source.getOrNull(index)?.isHexDigit() == true)
                            index += 1
                        }
                    }
                }
            }
        }
        throw IllegalArgumentException("Unterminated JSON string")
    }

    private fun parsePrimitive() {
        val start = index
        while (index < source.length && source[index] !in PRIMITIVE_DELIMITERS) index += 1
        require(index > start)
    }

    private fun skipWhitespace() {
        while (index < source.length && source[index] in JSON_WHITESPACE) index += 1
    }

    private fun takeIfPresent(character: Char): Boolean {
        if (source.getOrNull(index) != character) return false
        index += 1
        return true
    }

    private fun Char.isHexDigit(): Boolean =
        this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

    private companion object {
        const val MAX_DEPTH = 32
        val JSON_WHITESPACE = setOf(' ', '\t', '\r', '\n')
        val PRIMITIVE_DELIMITERS = JSON_WHITESPACE + setOf(',', ']', '}')
    }
}
