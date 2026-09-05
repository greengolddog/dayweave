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

enum class ScheduleInvalidationStreamEnd {
    ENDED,
    UNSUPPORTED,
}

fun interface ScheduleInvalidationStreamTransport {
    suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableRevision: ULong,
        onInvalidation: suspend (ULong) -> Unit,
    ): ScheduleInvalidationStreamEnd
}

sealed class ScheduleInvalidationStreamException(
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause) {
    class Authentication : ScheduleInvalidationStreamException(
        "The DayWeave schedule stream rejected authentication",
    )

    class Http(val statusCode: Int) : ScheduleInvalidationStreamException(
        "The DayWeave schedule stream returned HTTP $statusCode",
    )

    class Protocol(cause: Throwable? = null) : ScheduleInvalidationStreamException(
        "The DayWeave schedule stream returned an invalid response",
        cause,
    )
}

/** Content-free SSE transport; every event remains only a hint to perform an authoritative GET. */
class OkHttpScheduleInvalidationStreamTransport(
    private val client: OkHttpClient = defaultScheduleInvalidationClient(),
) : ScheduleInvalidationStreamTransport {
    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableRevision: ULong,
        onInvalidation: suspend (ULong) -> Unit,
    ): ScheduleInvalidationStreamEnd {
        val activeCall = AtomicReference<Call?>()
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/schedule/stream")
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
                        401 -> throw ScheduleInvalidationStreamException.Authentication()
                        404 -> ScheduleInvalidationStreamEnd.UNSUPPORTED
                        else -> {
                            response.consumeBoundedErrorBody()
                            throw ScheduleInvalidationStreamException.Http(response.code)
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
        lastDurableRevision: ULong,
        onInvalidation: suspend (ULong) -> Unit,
    ): ScheduleInvalidationStreamEnd {
        if (
            !response.hasStrictEventStreamMediaType() ||
            response.headers.values("Cache-Control").singleOrNull()?.lowercase() !=
            "no-store, no-cache" ||
            response.headers.values("Pragma").singleOrNull()?.lowercase() != "no-cache" ||
            response.headers.values("X-Accel-Buffering").singleOrNull()?.lowercase() != "no"
        ) {
            throw ScheduleInvalidationStreamException.Protocol()
        }
        val contentEncodings = response.headers.values("Content-Encoding")
        if (contentEncodings.size > 1) throw ScheduleInvalidationStreamException.Protocol()
        val contentEncoding = contentEncodings.singleOrNull()
        if (contentEncoding != null && !contentEncoding.equals("identity", ignoreCase = true)) {
            throw ScheduleInvalidationStreamException.Protocol()
        }
        withContext(Dispatchers.IO) {
            try {
                ScheduleInvalidationSseParser(lastDurableRevision).parse(
                    response.body.byteStream(),
                    onInvalidation,
                )
            } catch (error: ScheduleInvalidationProtocolException) {
                throw ScheduleInvalidationStreamException.Protocol(error)
            } catch (error: IllegalArgumentException) {
                throw ScheduleInvalidationStreamException.Protocol(error)
            }
        }
        return ScheduleInvalidationStreamEnd.ENDED
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
        val parts = values.single().split(';').map(String::trim)
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

        fun defaultScheduleInvalidationClient(): OkHttpClient = OkHttpClient.Builder()
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

internal class ScheduleInvalidationSseParser(initialRevision: ULong) {
    private var previousRevision = initialRevision
    private var eventCount = 0
    private var frameCount = 0

    suspend fun parse(input: InputStream, onInvalidation: suspend (ULong) -> Unit) {
        var frame = Frame()
        var frameBytes = 0
        var lineCount = 0
        while (true) {
            val line = input.readStrictScheduleLine()
            if (line == null) {
                if (!frame.isEmpty()) scheduleProtocolFailure()
                return
            }
            frameBytes = checkedScheduleAdd(frameBytes, line.bytes.size + line.delimiterBytes)
            if (frameBytes > MAX_FRAME_BYTES) scheduleProtocolFailure()
            lineCount = checkedScheduleAdd(lineCount, 1)
            if (lineCount > MAX_LINES_PER_FRAME) scheduleProtocolFailure()
            if (line.bytes.isEmpty()) {
                if (frame.isEmpty()) scheduleProtocolFailure()
                frameCount = checkedScheduleAdd(frameCount, 1)
                if (frameCount > MAX_FRAMES_PER_CONNECTION) scheduleProtocolFailure()
                frame.finish()?.let { revision ->
                    eventCount = checkedScheduleAdd(eventCount, 1)
                    if (eventCount > MAX_EVENTS_PER_CONNECTION || revision <= previousRevision) {
                        scheduleProtocolFailure()
                    }
                    previousRevision = revision
                    onInvalidation(revision)
                }
                frame = Frame()
                frameBytes = 0
                lineCount = 0
            } else {
                frame.accept(line.bytes.decodeStrictScheduleUtf8())
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
                if (!isEmpty()) scheduleProtocolFailure()
                commentSeen = true
                return
            }
            if (commentSeen || !line.contains(FIELD_SEPARATOR)) scheduleProtocolFailure()
            val (name, value) = line.split(FIELD_SEPARATOR, limit = 2)
            when (name) {
                "id" -> if (id == null) id = value else scheduleProtocolFailure()
                "event" -> if (event == null) event = value else scheduleProtocolFailure()
                "data" -> if (data == null) data = value else scheduleProtocolFailure()
                else -> scheduleProtocolFailure()
            }
        }

        fun finish(): ULong? {
            if (commentSeen) return null
            val revision = (id ?: scheduleProtocolFailure()).parseCanonicalScheduleRevision()
            if (event != INVALIDATION_EVENT || data != "{\"revision\":$revision}") {
                scheduleProtocolFailure()
            }
            return revision
        }
    }

    private companion object {
        const val FIELD_SEPARATOR = ": "
        const val INVALIDATION_EVENT = "schedule-invalidation"
        const val HEARTBEAT_COMMENT = ": heartbeat"
        const val MAX_LINE_BYTES = 4 * 1024
        const val MAX_FRAME_BYTES = 8 * 1024
        const val MAX_LINES_PER_FRAME = 4
        const val MAX_EVENTS_PER_CONNECTION = 10_000
        const val MAX_FRAMES_PER_CONNECTION = 20_000
    }
}

private data class StrictScheduleLine(val bytes: ByteArray, val delimiterBytes: Int)

private fun InputStream.readStrictScheduleLine(): StrictScheduleLine? {
    val bytes = ArrayList<Byte>()
    while (true) {
        when (val next = read()) {
            -1 -> if (bytes.isEmpty()) return null else scheduleProtocolFailure()
            0 -> scheduleProtocolFailure()
            '\n'.code -> return StrictScheduleLine(bytes.toByteArray(), 1)
            '\r'.code -> {
                if (read() != '\n'.code) scheduleProtocolFailure()
                return StrictScheduleLine(bytes.toByteArray(), 2)
            }
            else -> {
                if (bytes.size >= 4 * 1024) scheduleProtocolFailure()
                bytes += next.toByte()
            }
        }
    }
}

private fun ByteArray.decodeStrictScheduleUtf8(): String = try {
    StandardCharsets.UTF_8.newDecoder()
        .onMalformedInput(CodingErrorAction.REPORT)
        .onUnmappableCharacter(CodingErrorAction.REPORT)
        .decode(ByteBuffer.wrap(this))
        .toString()
} catch (error: Exception) {
    throw ScheduleInvalidationProtocolException(error)
}

private fun String.parseCanonicalScheduleRevision(): ULong {
    if (isEmpty() || this != "0" && (first() == '0' || any { it !in '0'..'9' })) {
        scheduleProtocolFailure()
    }
    if (any { it !in '0'..'9' }) scheduleProtocolFailure()
    return toULongOrNull()?.takeIf {
        it <= Long.MAX_VALUE.toULong() && it.toString() == this
    } ?: scheduleProtocolFailure()
}

private fun checkedScheduleAdd(left: Int, right: Int): Int = try {
    Math.addExact(left, right)
} catch (_: ArithmeticException) {
    scheduleProtocolFailure()
}

private class ScheduleInvalidationProtocolException(cause: Throwable? = null) :
    IOException("Invalid schedule invalidation frame", cause)

private fun scheduleProtocolFailure(): Nothing = throw ScheduleInvalidationProtocolException()
