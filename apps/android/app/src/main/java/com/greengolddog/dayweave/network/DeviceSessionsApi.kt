package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.hasAtMostUnicodeScalars
import java.io.IOException
import java.time.DateTimeException
import java.time.Duration
import java.time.Instant
import java.time.format.DateTimeParseException
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

@Serializable
internal data class DeviceSessionListResponse(
    val sessions: List<DeviceSessionContract>,
)

internal interface DeviceSessionsTransport {
    suspend fun listSessions(
        configuration: AuthenticatedApiConfiguration,
    ): DeviceSessionListResponse

    /** Revokes one session and accepts only the server's exact empty 204 contract. */
    suspend fun revokeSession(
        configuration: AuthenticatedApiConfiguration,
        sessionId: String,
    )
}

/** The DELETE was dispatched, but its response cannot prove whether the mutation committed. */
internal class DeviceSessionDeleteOutcomeAmbiguousException :
    IOException("Device-session revocation outcome is ambiguous")

/** Strict, no-store transport for owner-visible device-session management. */
internal class OkHttpDeviceSessionsTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
    private val now: () -> Instant = Instant::now,
) : DeviceSessionsTransport {
    override suspend fun listSessions(
        configuration: AuthenticatedApiConfiguration,
    ): DeviceSessionListResponse {
        val request = requestBuilder(
            configuration,
            configuration.baseUrl.newBuilder()
                .addPathSegments("v1/auth/sessions")
                .build()
                .toString(),
        ).get().build()
        val response = execute(configuration, request, setOf(200))
        return decodeSessionList(response)
    }

    override suspend fun revokeSession(
        configuration: AuthenticatedApiConfiguration,
        sessionId: String,
    ) {
        requireCanonicalNonNilUuid(sessionId)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/auth/sessions")
            .addPathSegment(sessionId)
            .build()
        val response = execute(
            configuration,
            requestBuilder(configuration, url.toString()).delete().build(),
            setOf(204),
            invalidExpectedResponse = ::DeviceSessionDeleteOutcomeAmbiguousException,
        )
        response.use {
            val body = try {
                readStrictBoundedBody(response, MAX_DEVICE_AUTH_RESPONSE_BYTES)
            } catch (_: DeviceAuthApiException.InvalidResponse) {
                throw DeviceSessionDeleteOutcomeAmbiguousException()
            }
            if (body.isNotEmpty()) {
                throw DeviceSessionDeleteOutcomeAmbiguousException()
            }
        }
    }

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .header("Accept", "application/json")
        // The coordinator strips and replaces this immediately before dispatch. Supplying the
        // captured value keeps the uncoordinated test/legacy configuration path authenticated.
        .header("Authorization", "Bearer ${configuration.bearerToken}")
        .header("Cache-Control", "no-store")
        .header("Pragma", "no-cache")

    private suspend fun execute(
        configuration: AuthenticatedApiConfiguration,
        request: Request,
        expectedStatuses: Set<Int>,
        invalidExpectedResponse: () -> IOException = { DeviceAuthApiException.InvalidResponse() },
    ): Response {
        val response = configuration.executeAuthenticated(client, request)
        if (!hasStrictNoStoreHeaders(response)) {
            val failure = if (response.code in expectedStatuses) {
                invalidExpectedResponse()
            } else {
                DeviceAuthApiException.Unavailable()
            }
            response.close()
            throw failure
        }
        if (response.code !in expectedStatuses) {
            throw response.use(::decodeTrustedDeviceAuthError)
        }
        if (response.code != 204 && !hasExactJsonMediaType(response)) {
            response.close()
            throw DeviceAuthApiException.InvalidResponse()
        }
        return response
    }

    private fun decodeSessionList(response: Response): DeviceSessionListResponse = response.use {
        val text = readStrictBoundedBody(response, MAX_DEVICE_SESSIONS_RESPONSE_BYTES)
        // Capture the freshness boundary only after the entire response has arrived.
        val receivedAt = now()
        val duplicateKeys = try {
            StrictDeviceSessionsJsonScanner(text).hasDuplicateKeys()
        } catch (_: SerializationException) {
            throw DeviceAuthApiException.InvalidResponse()
        } catch (_: IllegalArgumentException) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        if (duplicateKeys) throw DeviceAuthApiException.InvalidResponse()
        val root = try {
            DEVICE_AUTH_JSON.parseToJsonElement(text) as? JsonObject
                ?: throw DeviceAuthApiException.InvalidResponse()
        } catch (_: SerializationException) {
            throw DeviceAuthApiException.InvalidResponse()
        } catch (_: IllegalArgumentException) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        if (root.keys != SESSION_LIST_KEYS) throw DeviceAuthApiException.InvalidResponse()
        val rows = root["sessions"] as? JsonArray
            ?: throw DeviceAuthApiException.InvalidResponse()
        if (rows.size > MAX_ACTIVE_DEVICE_SESSIONS) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        rows.forEach { row ->
            if ((row as? JsonObject)?.keys != DEVICE_SESSION_KEYS) {
                throw DeviceAuthApiException.InvalidResponse()
            }
        }
        val decoded = try {
            DEVICE_AUTH_JSON.decodeFromJsonElement<DeviceSessionListResponse>(root)
        } catch (_: SerializationException) {
            throw DeviceAuthApiException.InvalidResponse()
        } catch (_: IllegalArgumentException) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        try {
            if (decoded.sessions.map { it.id }.toSet().size != decoded.sessions.size) {
                throw IllegalArgumentException("Duplicate device-session identifier")
            }
            decoded.sessions.forEach { validateListedDeviceSession(it, receivedAt) }
            validateListedDeviceSessionOrder(decoded.sessions)
        } catch (_: IllegalArgumentException) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        decoded
    }

    private fun readStrictBoundedBody(response: Response, maximumBytes: Int): String {
        val declaredLength = response.body.contentLength()
        if (declaredLength > maximumBytes) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        val bytes = try {
            response.body.byteStream().use { input ->
                val buffer = ByteArray(maximumBytes + 1)
                var total = 0
                try {
                    while (total < buffer.size) {
                        val read = input.read(buffer, total, buffer.size - total)
                        if (read < 0) break
                        total += read
                    }
                    if (total > maximumBytes || input.read() >= 0) {
                        throw DeviceAuthApiException.InvalidResponse()
                    }
                    buffer.copyOf(total)
                } finally {
                    buffer.fill(0)
                }
            }
        } catch (error: DeviceAuthApiException) {
            throw error
        } catch (_: IOException) {
            throw DeviceAuthApiException.Unavailable()
        }
        return try {
            decodeStrictUtf8(bytes) ?: throw DeviceAuthApiException.InvalidResponse()
        } finally {
            bytes.fill(0)
        }
    }

    private companion object {
        val SESSION_LIST_KEYS = setOf("sessions")
        val DEVICE_SESSION_KEYS = setOf(
            "id",
            "client_instance_id",
            "client_kind",
            "device_label",
            "scopes",
            "client_contract_version",
            "client_version",
            "client_capabilities",
            "created_at",
            "last_seen_at",
            "credential_issued_at",
            "access_expires_at",
            "refresh_idle_expires_at",
            "absolute_expires_at",
            "revision",
        )
    }
}

internal fun validateListedDeviceSession(
    session: DeviceSessionContract,
    receivedAt: Instant,
) {
    requireCanonicalNonNilUuid(session.id)
    requireCanonicalNonNilUuid(session.clientInstanceId)
    require(session.clientKind in DEVICE_CLIENT_KINDS) { "Unknown device client kind" }
    requireSafeDeviceSessionLabel(session.deviceLabel, 200)
    requireSafeDeviceSessionLabel(session.clientVersion, 100)
    require(session.clientContractVersion in 1..DEVICE_AUTH_CONTRACT_VERSION)
    require(session.scopes.isNotEmpty() && session.scopes.size <= ANDROID_DEVICE_AUTH_SCOPES.size)
    require(session.scopes.distinct().size == session.scopes.size)
    require(session.scopes.all(KNOWN_DEVICE_AUTH_SCOPES::contains))
    require(
        session.clientContractVersion != 1 || "schedule_publish" !in session.scopes,
    ) { "A v1 device session cannot publish schedules" }
    require(session.clientCapabilities.size <= MAX_CLIENT_CAPABILITIES)
    require(session.clientCapabilities.distinct().size == session.clientCapabilities.size)
    require(session.clientCapabilities.all { requireSafeDeviceSessionLabel(it, 100) })
    require(session.revision in 1 until Long.MAX_VALUE)

    val created = parseDeviceSessionInstant(session.createdAt)
    val lastSeen = parseDeviceSessionInstant(session.lastSeenAt)
    val issued = parseDeviceSessionInstant(session.credentialIssuedAt)
    val accessExpiry = parseDeviceSessionInstant(session.accessExpiresAt)
    val refreshIdleExpiry = parseDeviceSessionInstant(session.refreshIdleExpiresAt)
    val absoluteExpiry = parseDeviceSessionInstant(session.absoluteExpiresAt)
    require(!issued.isBefore(created) && !lastSeen.isBefore(issued))
    require(lastSeen.isBefore(absoluteExpiry))
    require(accessExpiry.isAfter(issued))
    require(
        accessExpiry <= checkedDeviceSessionPlus(
            issued,
            DEVICE_AUTH_ACCESS_TTL.plus(DEVICE_SESSION_TTL_TOLERANCE),
        ),
    )
    require(accessExpiry <= absoluteExpiry)
    require(refreshIdleExpiry.isAfter(issued))
    require(
        refreshIdleExpiry <= checkedDeviceSessionPlus(
            issued,
            DEVICE_AUTH_REFRESH_IDLE_TTL.plus(DEVICE_SESSION_TTL_TOLERANCE),
        ),
    )
    require(refreshIdleExpiry <= absoluteExpiry)
    require(absoluteExpiry.isAfter(issued))
    require(
        absoluteExpiry <= checkedDeviceSessionPlus(
            created,
            DEVICE_AUTH_ABSOLUTE_TTL.plus(DEVICE_SESSION_TTL_TOLERANCE),
        ),
    )

    val latestServerTime = checkedDeviceSessionPlus(receivedAt, DEVICE_SESSION_CLOCK_SKEW)
    val earliestRefreshableExpiry = checkedDeviceSessionMinus(
        receivedAt,
        DEVICE_SESSION_CLOCK_SKEW,
    )
    require(!created.isAfter(latestServerTime))
    require(!issued.isAfter(latestServerTime))
    require(!lastSeen.isAfter(latestServerTime))
    require(refreshIdleExpiry.isAfter(earliestRefreshableExpiry))
    require(absoluteExpiry.isAfter(earliestRefreshableExpiry))
}

internal fun validateListedDeviceSessionOrder(sessions: List<DeviceSessionContract>) {
    sessions.zipWithNext().forEach { (left, right) ->
        val leftLastSeen = parseDeviceSessionInstant(left.lastSeenAt)
        val rightLastSeen = parseDeviceSessionInstant(right.lastSeenAt)
        require(
            leftLastSeen > rightLastSeen ||
                leftLastSeen == rightLastSeen && left.id < right.id,
        ) { "Device sessions are not in canonical server order" }
    }
}

private fun requireCanonicalNonNilUuid(value: String) {
    val parsed = try {
        UUID.fromString(value)
    } catch (_: IllegalArgumentException) {
        throw IllegalArgumentException("Invalid device-session identifier")
    }
    require(parsed != UUID(0, 0) && parsed.toString() == value) {
        "Invalid device-session identifier"
    }
}

private fun requireSafeDeviceSessionLabel(value: String, maximumCodePoints: Int): Boolean {
    require(
        value.isNotBlank() && value.hasAtMostUnicodeScalars(maximumCodePoints) &&
            value.none(Char::isISOControl),
    ) { "Invalid device-session metadata" }
    return true
}

private fun parseDeviceSessionInstant(value: String): Instant = try {
    Instant.parse(value)
} catch (_: DateTimeParseException) {
    throw IllegalArgumentException("Invalid device-session timestamp")
}

private fun checkedDeviceSessionPlus(
    value: Instant,
    duration: java.time.Duration,
): Instant = try {
    value.plus(duration)
} catch (_: DateTimeException) {
    throw IllegalArgumentException("Invalid device-session timestamp")
} catch (_: ArithmeticException) {
    throw IllegalArgumentException("Invalid device-session timestamp")
}

private fun checkedDeviceSessionMinus(
    value: Instant,
    duration: Duration,
): Instant = try {
    value.minus(duration)
} catch (_: DateTimeException) {
    throw IllegalArgumentException("Invalid device-session timestamp")
} catch (_: ArithmeticException) {
    throw IllegalArgumentException("Invalid device-session timestamp")
}

private val KNOWN_DEVICE_AUTH_SCOPES = ANDROID_DEVICE_AUTH_SCOPES.toSet()
private val DEVICE_CLIENT_KINDS = setOf("android", "macos")
private const val MAX_CLIENT_CAPABILITIES = 100
internal const val MAX_ACTIVE_DEVICE_SESSIONS = 16
internal const val MAX_DEVICE_SESSIONS_RESPONSE_BYTES = 1024 * 1024
private val DEVICE_SESSION_CLOCK_SKEW: Duration = Duration.ofMinutes(5)
private val DEVICE_SESSION_TTL_TOLERANCE: Duration = Duration.ofSeconds(1)

/** Rejects equivalent duplicate object keys and noncanonical numbers before tree decoding. */
private class StrictDeviceSessionsJsonScanner(
    private val source: String,
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
                '"' -> return DEVICE_AUTH_JSON.decodeFromString(
                    source.substring(start, index),
                )
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
        val token = source.substring(start, index)
        require(token in JSON_LITERAL_TOKENS || CANONICAL_JSON_INTEGER_PATTERN.matches(token)) {
            "JSON numbers must use canonical base-10 integer syntax"
        }
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
        val JSON_LITERAL_TOKENS = setOf("false", "null", "true")
        val CANONICAL_JSON_INTEGER_PATTERN = Regex("(?:0|[1-9][0-9]*|-[1-9][0-9]*)")
    }
}
