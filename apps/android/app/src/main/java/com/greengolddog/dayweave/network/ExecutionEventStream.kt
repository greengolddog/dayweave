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
import okhttp3.Call
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

/** A stream ending is expected: the server deliberately expires authenticated connections. */
enum class ExecutionInvalidationStreamEnd {
    ENDED,
    UNSUPPORTED,
}

/**
 * Optional transport kept separate from [ExecutionTransport], so existing clients and test fakes
 * continue to use the stable request/response contract unchanged.
 */
fun interface ExecutionInvalidationStreamTransport {
    suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableRevision: Long,
        onInvalidation: (Long) -> Unit,
    ): ExecutionInvalidationStreamEnd
}

sealed class ExecutionInvalidationStreamException(
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause) {
    class Authentication : ExecutionInvalidationStreamException(
        "The DayWeave execution stream rejected authentication",
    )

    class Http(val statusCode: Int) : ExecutionInvalidationStreamException(
        "The DayWeave execution stream returned HTTP $statusCode",
    )

    class Protocol(cause: Throwable? = null) : ExecutionInvalidationStreamException(
        "The DayWeave execution stream returned an invalid response",
        cause,
    )
}

/** Authenticated, content-free SSE transport dedicated to foreground invalidation hints. */
class OkHttpExecutionInvalidationStreamTransport(
    private val client: OkHttpClient = defaultExecutionInvalidationClient(),
) : ExecutionInvalidationStreamTransport {
    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableRevision: Long,
        onInvalidation: (Long) -> Unit,
    ): ExecutionInvalidationStreamEnd {
        require(lastDurableRevision >= 0)
        val activeCall = AtomicReference<Call?>()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/execution/stream")
            .build()
        val request = Request.Builder()
            .url(url)
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", EVENT_STREAM_MEDIA_TYPE)
            .header("Last-Event-ID", lastDurableRevision.toString())
            .header("Cache-Control", "no-store, no-cache")
            .header("Pragma", "no-cache")
            .header("Accept-Encoding", "identity")
            .header("Authorization", "Bearer ${configuration.bearerToken}")
            .get()
            .build()

        val response = configuration.executeAuthenticatedCancellable(client, request) { call ->
            activeCall.set(call)
        }
        return coroutineScope {
            // Canceling the exact observed call interrupts both SSE and bounded error-body reads.
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
                        200 -> collectSuccessfulResponse(
                            response,
                            lastDurableRevision,
                            onInvalidation,
                        )
                        401 -> throw ExecutionInvalidationStreamException.Authentication()
                        // Unsupported fallback never reads an attacker-controlled response body.
                        404 -> ExecutionInvalidationStreamEnd.UNSUPPORTED
                        else -> {
                            response.consumeBoundedErrorBody()
                            throw ExecutionInvalidationStreamException.Http(response.code)
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
        lastDurableRevision: Long,
        onInvalidation: (Long) -> Unit,
    ): ExecutionInvalidationStreamEnd {
        if (!response.hasStrictEventStreamMediaType()) {
            throw ExecutionInvalidationStreamException.Protocol()
        }
        val contentEncodings = response.headers.values("Content-Encoding")
        if (contentEncodings.size > 1) throw ExecutionInvalidationStreamException.Protocol()
        val contentEncoding = contentEncodings.singleOrNull()
        if (contentEncoding != null && !contentEncoding.equals("identity", ignoreCase = true)) {
            throw ExecutionInvalidationStreamException.Protocol()
        }

        withContext(Dispatchers.IO) {
            try {
                ExecutionInvalidationSseParser(lastDurableRevision).parse(
                    input = response.body.byteStream(),
                    onInvalidation = onInvalidation,
                )
            } catch (error: ExecutionInvalidationProtocolException) {
                throw ExecutionInvalidationStreamException.Protocol(error)
            } catch (error: IllegalArgumentException) {
                throw ExecutionInvalidationStreamException.Protocol(error)
            }
        }
        return ExecutionInvalidationStreamEnd.ENDED
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
        // Bytes are deliberately discarded. Neither remote text nor credentials reach errors.
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

        fun defaultExecutionInvalidationClient(): OkHttpClient = OkHttpClient.Builder()
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

/**
 * Strict byte-level parser for the only two frames emitted by the DayWeave endpoint:
 * a content-free revision invalidation and a `heartbeat` comment.
 */
internal class ExecutionInvalidationSseParser(
    initialRevision: Long,
) {
    private var previousRevision = initialRevision.also { require(it >= 0) }
    private var eventCount = 0
    private var frameCount = 0

    fun parse(input: InputStream, onInvalidation: (Long) -> Unit) {
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
                frame.finish()?.let { revision ->
                    eventCount = checkedAdd(eventCount, 1)
                    if (eventCount > MAX_EVENTS_PER_CONNECTION || revision <= previousRevision) {
                        protocolFailure()
                    }
                    previousRevision = revision
                    onInvalidation(revision)
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

        /** A heartbeat returns null; an invalidation returns its exact canonical revision. */
        fun finish(): Long? {
            if (commentSeen) {
                if (id != null || event != null || data != null) protocolFailure()
                return null
            }
            val idText = id ?: protocolFailure()
            if (event != INVALIDATION_EVENT) protocolFailure()
            val revision = idText.parseCanonicalRevision()
            if (data != "{\"revision\":$revision}") protocolFailure()
            return revision
        }

        private fun String.parseCanonicalRevision(): Long {
            if (isEmpty() || this != "0" && (first() == '0' || any { it !in '0'..'9' })) {
                protocolFailure()
            }
            if (any { it !in '0'..'9' }) protocolFailure()
            var result = 0L
            forEach { character ->
                result = try {
                    Math.addExact(Math.multiplyExact(result, 10L), (character - '0').toLong())
                } catch (_: ArithmeticException) {
                    protocolFailure()
                }
            }
            return result
        }
    }

    private data class StrictLine(
        val bytes: ByteArray,
        val delimiterBytes: Int,
    )

    private fun InputStream.readStrictLine(): StrictLine? {
        val bytes = ArrayList<Byte>()
        while (true) {
            when (val next = read()) {
                -1 -> if (bytes.isEmpty()) return null else protocolFailure()
                0 -> protocolFailure()
                '\n'.code -> return StrictLine(bytes.toByteArray(), delimiterBytes = 1)
                '\r'.code -> {
                    if (read() != '\n'.code) protocolFailure()
                    return StrictLine(bytes.toByteArray(), delimiterBytes = 2)
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
        throw ExecutionInvalidationProtocolException(error)
    }

    private fun checkedAdd(left: Int, right: Int): Int = try {
        Math.addExact(left, right)
    } catch (_: ArithmeticException) {
        protocolFailure()
    }

    private companion object {
        const val FIELD_SEPARATOR = ": "
        const val INVALIDATION_EVENT = "execution-invalidation"
        const val HEARTBEAT_COMMENT = ": heartbeat"
        const val MAX_LINE_BYTES = 4 * 1024
        const val MAX_FRAME_BYTES = 8 * 1024
        const val MAX_LINES_PER_FRAME = 4
        const val MAX_EVENTS_PER_CONNECTION = 10_000
        const val MAX_FRAMES_PER_CONNECTION = 20_000
    }
}

private class ExecutionInvalidationProtocolException(cause: Throwable? = null) :
    IOException("Invalid execution invalidation frame", cause)

private fun protocolFailure(): Nothing = throw ExecutionInvalidationProtocolException()
