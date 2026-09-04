package com.greengolddog.dayweave.network

import java.io.IOException
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

/** A normal end is expected because authenticated habit streams expire deliberately. */
enum class HabitInvalidationStreamEnd {
    ENDED,
    UNSUPPORTED,
}

/** Separate from request/response transport so offline tests and older servers remain compatible. */
fun interface HabitInvalidationStreamTransport {
    suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableCursor: String?,
        onInvalidation: (String) -> Unit,
    ): HabitInvalidationStreamEnd
}

sealed class HabitInvalidationStreamException(
    message: String,
    cause: Throwable? = null,
) : IOException(message, cause) {
    class Authentication : HabitInvalidationStreamException(
        "The DayWeave habit stream rejected authentication",
    )

    class Http(val statusCode: Int) : HabitInvalidationStreamException(
        "The DayWeave habit stream returned HTTP $statusCode",
    )

    class Protocol(cause: Throwable? = null) : HabitInvalidationStreamException(
        "The DayWeave habit stream returned an invalid response",
        cause,
    )
}

/** Authenticated, content-free SSE transport for foreground habit invalidation hints. */
class OkHttpHabitInvalidationStreamTransport(
    private val client: OkHttpClient = defaultHabitInvalidationClient(),
) : HabitInvalidationStreamTransport {
    override suspend fun collect(
        configuration: AuthenticatedApiConfiguration,
        lastDurableCursor: String?,
        onInvalidation: (String) -> Unit,
    ): HabitInvalidationStreamEnd {
        if (lastDurableCursor != null && !isHabitDeltaCursor(lastDurableCursor)) {
            throw HabitInvalidationStreamException.Protocol()
        }
        val activeCall = AtomicReference<Call?>()
        val request = Request.Builder()
            .url(
                configuration.baseUrl.newBuilder()
                    .addPathSegments("v1/habits/stream")
                    .build(),
            )
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
                        401 -> throw HabitInvalidationStreamException.Authentication()
                        // A server being upgraded can omit SSE while delta polling stays valid.
                        404 -> HabitInvalidationStreamEnd.UNSUPPORTED
                        else -> {
                            response.consumeBoundedErrorBody()
                            throw HabitInvalidationStreamException.Http(response.code)
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
    ): HabitInvalidationStreamEnd {
        if (
            !response.hasStrictEventStreamMediaType() ||
            response.headers.values("Cache-Control").singleOrNull()?.lowercase() !=
            "no-store, no-cache" ||
            response.headers.values("Pragma").singleOrNull()?.lowercase() != "no-cache" ||
            response.headers.values("X-Accel-Buffering").singleOrNull()?.lowercase() != "no"
        ) {
            throw HabitInvalidationStreamException.Protocol()
        }
        val encodings = response.headers.values("Content-Encoding")
        if (encodings.size > 1) throw HabitInvalidationStreamException.Protocol()
        val encoding = encodings.singleOrNull()
        if (encoding != null && !encoding.equals("identity", ignoreCase = true)) {
            throw HabitInvalidationStreamException.Protocol()
        }
        withContext(Dispatchers.IO) {
            try {
                CanonicalItemInvalidationSseParser(
                    expectedInvalidationEvent = INVALIDATION_EVENT,
                    cursorValidator = ::isHabitDeltaCursor,
                ).parse(response.body.byteStream(), onInvalidation)
            } catch (error: OpaqueCursorInvalidationProtocolException) {
                throw HabitInvalidationStreamException.Protocol(error)
            } catch (error: IllegalArgumentException) {
                throw HabitInvalidationStreamException.Protocol(error)
            }
        }
        return HabitInvalidationStreamEnd.ENDED
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
        private const val INVALIDATION_EVENT = "habit-invalidation"
        private const val MAX_ERROR_BODY_BYTES = 8 * 1024

        fun defaultHabitInvalidationClient(): OkHttpClient = OkHttpClient.Builder()
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

/** The server emits canonical URL-safe unpadded Base64 cursors, bounded to 256 bytes. */
internal fun isHabitDeltaCursor(value: String): Boolean =
    value.length in 1..MAX_HABIT_DELTA_CURSOR_CHARS && value.all { character ->
        character in 'a'..'z' || character in 'A'..'Z' || character in '0'..'9' ||
            character == '-' || character == '_'
    }

private const val MAX_HABIT_DELTA_CURSOR_CHARS = 256
