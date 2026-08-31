package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.Call
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

/** A normal end is expected because the server deliberately expires authenticated streams. */
enum class CanonicalItemInvalidationStreamEnd {
    ENDED,
    UNSUPPORTED,
}

/** Kept separate from planner request/response transport so older clients and fakes still work. */
fun interface CanonicalItemInvalidationStreamTransport {
    suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableCursor: String?,
        onInvalidation: (String) -> Unit,
    ): CanonicalItemInvalidationStreamEnd
}

sealed class CanonicalItemInvalidationStreamException(
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause) {
    class Authentication : CanonicalItemInvalidationStreamException(
        "The DayWeave item stream rejected authentication",
    )

    class Http(val statusCode: Int) : CanonicalItemInvalidationStreamException(
        "The DayWeave item stream returned HTTP $statusCode",
    )

    class Protocol(cause: Throwable? = null) : CanonicalItemInvalidationStreamException(
        "The DayWeave item stream returned an invalid response",
        cause,
    )
}

/** Authenticated, content-free SSE transport dedicated to foreground canonical-item hints. */
class OkHttpCanonicalItemInvalidationStreamTransport(
    private val client: OkHttpClient = defaultCanonicalItemInvalidationClient(),
) : CanonicalItemInvalidationStreamTransport {
    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableCursor: String?,
        onInvalidation: (String) -> Unit,
    ): CanonicalItemInvalidationStreamEnd {
        if (lastDurableCursor != null && !isCanonicalItemCursor(lastDurableCursor)) {
            throw CanonicalItemInvalidationStreamException.Protocol()
        }
        val activeCall = AtomicReference<Call?>()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/items/stream")
            .build()
        val request = Request.Builder()
            .url(url)
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", EVENT_STREAM_MEDIA_TYPE)
            .header("Cache-Control", "no-store, no-cache")
            .header("Pragma", "no-cache")
            .header("Accept-Encoding", "identity")
            .header("Authorization", "Bearer ${configuration.bearerToken}")
            .apply {
                if (lastDurableCursor != null) header("Last-Event-ID", lastDurableCursor)
            }
            .get()
            .build()

        val response = configuration.executeAuthenticatedCancellable(client, request) { call ->
            activeCall.set(call)
        }
        return coroutineScope {
            val cancellationCloser = launch(
                context = Dispatchers.IO,
                start = CoroutineStart.UNDISPATCHED,
            ) {
                try {
                    awaitCancellation()
                } finally {
                    activeCall.get()?.cancel()
                }
            }
            try {
                response.use {
                    when (response.code) {
                        200 -> collectSuccessfulResponse(response, onInvalidation)
                        401 -> throw CanonicalItemInvalidationStreamException.Authentication()
                        // Rollout fallback never waits for or consumes an attacker-controlled body.
                        404 -> CanonicalItemInvalidationStreamEnd.UNSUPPORTED
                        else -> {
                            response.consumeBoundedErrorBody()
                            throw CanonicalItemInvalidationStreamException.Http(response.code)
                        }
                    }
                }
            } finally {
                cancellationCloser.cancelAndJoin()
            }
        }
    }

    private suspend fun collectSuccessfulResponse(
        response: Response,
        onInvalidation: (String) -> Unit,
    ): CanonicalItemInvalidationStreamEnd {
        if (!response.hasStrictEventStreamMediaType()) {
            throw CanonicalItemInvalidationStreamException.Protocol()
        }
        val contentEncodings = response.headers.values("Content-Encoding")
        if (contentEncodings.size > 1) throw CanonicalItemInvalidationStreamException.Protocol()
        val contentEncoding = contentEncodings.singleOrNull()
        if (contentEncoding != null && !contentEncoding.equals("identity", ignoreCase = true)) {
            throw CanonicalItemInvalidationStreamException.Protocol()
        }

        withContext(Dispatchers.IO) {
            try {
                CanonicalItemInvalidationSseParser().parse(
                    input = response.body.byteStream(),
                    onInvalidation = onInvalidation,
                )
            } catch (error: CanonicalItemInvalidationProtocolException) {
                throw CanonicalItemInvalidationStreamException.Protocol(error)
            } catch (error: IllegalArgumentException) {
                throw CanonicalItemInvalidationStreamException.Protocol(error)
            }
        }
        return CanonicalItemInvalidationStreamEnd.ENDED
    }

    private suspend fun Response.consumeBoundedErrorBody() = withContext(Dispatchers.IO) {
        val input = body.byteStream()
        val buffer = ByteArray(1024)
        var remaining = MAX_ERROR_BODY_BYTES + 1
        while (remaining > 0) {
            val read = input.read(buffer, 0, minOf(buffer.size, remaining))
            if (read < 0) break
            remaining -= read
        }
    }

    private fun Response.hasStrictEventStreamMediaType(): Boolean {
        val values = headers.values("Content-Type")
        if (values.size != 1) return false
        val raw = values.single()
        val parts = raw.split(';').map(String::trim)
        if (!parts.first().equals(EVENT_STREAM_MEDIA_TYPE, ignoreCase = true)) return false
        if (parts.size == 1) return true
        if (parts.size != 2) return false
        val parameter = parts[1].split('=', limit = 2)
        return parameter.size == 2 &&
            parameter[0].trim().equals("charset", ignoreCase = true) &&
            parameter[1].trim().trim('"').equals("utf-8", ignoreCase = true)
    }

    companion object {
        private const val EVENT_STREAM_MEDIA_TYPE = "text/event-stream"
        private const val MAX_ERROR_BODY_BYTES = 8 * 1024

        fun defaultCanonicalItemInvalidationClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(45, TimeUnit.SECONDS)
            .writeTimeout(30, TimeUnit.SECONDS)
            .callTimeout(6, TimeUnit.MINUTES)
            .retryOnConnectionFailure(true)
            .followRedirects(false)
            .followSslRedirects(false)
            .build()
    }
}

/** Strict parser for exact opaque-cursor invalidations and heartbeat comments. */
internal class CanonicalItemInvalidationSseParser {
    private var eventCount = 0
    private var frameCount = 0

    fun parse(input: InputStream, onInvalidation: (String) -> Unit) {
        var frame = Frame()
        var frameBytes = 0
        var lineCount = 0
        while (true) {
            val line = input.readStrictLine()
            if (line == null) {
                if (!frame.isEmpty()) protocolFailure()
                return
            }
            frameBytes = checkedAdd(frameBytes, line.bytes.size + line.delimiterBytes)
            if (frameBytes > MAX_FRAME_BYTES) protocolFailure()
            lineCount = checkedAdd(lineCount, 1)
            if (lineCount > MAX_LINES_PER_FRAME) protocolFailure()

            if (line.bytes.isEmpty()) {
                if (frame.isEmpty()) protocolFailure()
                frameCount = checkedAdd(frameCount, 1)
                if (frameCount > MAX_FRAMES_PER_CONNECTION) protocolFailure()
                frame.finish()?.let { cursor ->
                    eventCount = checkedAdd(eventCount, 1)
                    if (eventCount > MAX_EVENTS_PER_CONNECTION) protocolFailure()
                    onInvalidation(cursor)
                }
                frame = Frame()
                frameBytes = 0
                lineCount = 0
            } else {
                frame.accept(line.bytes.decodeStrictUtf8())
            }
        }
    }

    private class Frame {
        private var commentSeen = false
        private var id: String? = null
        private var event: String? = null
        private var data: String? = null

        fun isEmpty(): Boolean = !commentSeen && id == null && event == null && data == null

        fun accept(line: String) {
            if (line == HEARTBEAT_COMMENT) {
                if (!isEmpty()) protocolFailure()
                commentSeen = true
                return
            }
            if (commentSeen || !line.contains(FIELD_SEPARATOR)) protocolFailure()
            val (name, value) = line.split(FIELD_SEPARATOR, limit = 2)
            when (name) {
                "id" -> {
                    if (id != null) protocolFailure()
                    id = value
                }
                "event" -> {
                    if (event != null) protocolFailure()
                    event = value
                }
                "data" -> {
                    if (data != null) protocolFailure()
                    data = value
                }
                else -> protocolFailure()
            }
        }

        fun finish(): String? {
            if (commentSeen) {
                if (id != null || event != null || data != null) protocolFailure()
                return null
            }
            val cursor = id ?: protocolFailure()
            if (!isCanonicalItemCursor(cursor) || event != INVALIDATION_EVENT) protocolFailure()
            val expectedData = "{\"cursor\":" + CURSOR_JSON.encodeToString(cursor) + "}"
            if (data != expectedData) protocolFailure()
            return cursor
        }
    }

    private data class StrictLine(val bytes: ByteArray, val delimiterBytes: Int)

    private fun InputStream.readStrictLine(): StrictLine? {
        val bytes = ArrayList<Byte>()
        while (true) {
            when (val next = read()) {
                -1 -> if (bytes.isEmpty()) return null else protocolFailure()
                0 -> protocolFailure()
                '\n'.code -> return StrictLine(bytes.toByteArray(), 1)
                '\r'.code -> {
                    if (read() != '\n'.code) protocolFailure()
                    return StrictLine(bytes.toByteArray(), 2)
                }
                else -> {
                    if (bytes.size >= MAX_LINE_BYTES) protocolFailure()
                    bytes += next.toByte()
                }
            }
        }
    }

    private fun ByteArray.decodeStrictUtf8(): String = try {
        StandardCharsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
            .decode(ByteBuffer.wrap(this))
            .toString()
    } catch (error: Exception) {
        throw CanonicalItemInvalidationProtocolException(error)
    }

    private fun checkedAdd(left: Int, right: Int): Int = try {
        Math.addExact(left, right)
    } catch (_: ArithmeticException) {
        protocolFailure()
    }

    private companion object {
        const val FIELD_SEPARATOR = ": "
        const val INVALIDATION_EVENT = "item-invalidation"
        const val HEARTBEAT_COMMENT = ": heartbeat"
        const val MAX_LINE_BYTES = 1024
        const val MAX_FRAME_BYTES = 4 * 1024
        const val MAX_LINES_PER_FRAME = 4
        const val MAX_EVENTS_PER_CONNECTION = 10_000
        const val MAX_FRAMES_PER_CONNECTION = 20_000
        val CURSOR_JSON = Json {
            encodeDefaults = true
            explicitNulls = false
        }
    }
}

/** Cursors remain opaque; this only guarantees bounded SSE/header/JSON transport safety. */
internal fun isCanonicalItemCursor(value: String): Boolean =
    value.isNotEmpty() && value.length <= MAX_CANONICAL_ITEM_CURSOR_BYTES &&
        value.all { character ->
            character.code in 0x21..0x7e && character != '"' && character != '\\'
        }

internal const val MAX_CANONICAL_ITEM_CURSOR_BYTES = 256

private class CanonicalItemInvalidationProtocolException(cause: Throwable? = null) :
    IOException("Invalid canonical item invalidation frame", cause)

private fun protocolFailure(): Nothing = throw CanonicalItemInvalidationProtocolException()
