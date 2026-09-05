package com.greengolddog.dayweave.network

import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.util.Base64
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import okio.Buffer

class OkHttpDeviceAuthTransportTest {
    private val now = Instant.parse("2026-08-29T12:00:00Z")
    private val json = Json { explicitNulls = true; encodeDefaults = true }
    private lateinit var server: MockWebServer
    private lateinit var transport: OkHttpDeviceAuthTransport

    @Before
    fun setUp() {
        server = MockWebServer()
        server.start()
        transport = OkHttpDeviceAuthTransport(
            now = { now },
            allowCleartextLoopbackForTests = true,
        )
    }

    @After
    fun tearDown() {
        server.close()
    }

    @Test
    fun bootstrapCreationUsesStrictHeadersAndAndroidContract() = runBlocking {
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 1)
        server.enqueue(
            strictJson(
                201,
                """{"id":"$SYNTHETIC_SESSION_ID","enrollment_token":"$enrollment","expires_at":"${now.plus(Duration.ofMinutes(10))}","client_contract_version":$DEVICE_AUTH_CONTRACT_VERSION,"replayed":false}""",
            ),
        )

        val issued = transport.createEnrollment(
            buildEnrollmentCreationHttpRequest(
                baseUrl(),
                "synthetic-bootstrap",
            CreateDeviceEnrollmentRequest(
                id = SYNTHETIC_SESSION_ID,
                enrollmentToken = enrollment,
                clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                deviceLabel = SYNTHETIC_DEVICE_LABEL,
                clientVersion = SYNTHETIC_CLIENT_VERSION,
            ),
                allowCleartextLoopback = true,
            ),
        )

        assertEquals(enrollment, issued.enrollmentToken)
        assertFalse(issued.replayed)
        val request = server.takeRequest()
        assertEquals("POST", request.method)
        assertEquals("/tenant/v1/auth/device-enrollments", request.url.encodedPath)
        assertEquals("Bearer synthetic-bootstrap", request.headers["Authorization"])
        assertEquals("no-store", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])
        val body = Json.parseToJsonElement(requireNotNull(request.body).utf8()).jsonObject
        assertEquals(SYNTHETIC_SESSION_ID, body["id"]?.jsonPrimitive?.content)
        assertEquals(enrollment, body["enrollment_token"]?.jsonPrimitive?.content)
        assertEquals("android", body["client_kind"]?.jsonPrimitive?.content)
        assertEquals(SYNTHETIC_CLIENT_INSTANCE_ID, body["client_instance_id"]?.jsonPrimitive?.content)
        assertEquals(
            ANDROID_DEVICE_AUTH_SCOPES,
            body["scopes"]?.let { element ->
                element.toString().removePrefix("[").removeSuffix("]")
                    .split(',').map { it.trim().trim('"') }
            },
        )
    }

    @Test
    fun bootstrapCreationReplayRequiresExactStatusAndEchoedTuple() = runBlocking {
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 30)
        val future = now.plusSeconds(600)
        fun response(id: String, token: String, replayed: Boolean) =
            """{"id":"$id","enrollment_token":"$token","expires_at":"$future","client_contract_version":$DEVICE_AUTH_CONTRACT_VERSION,"replayed":$replayed}"""

        server.enqueue(strictJson(200, response(SYNTHETIC_SESSION_ID, enrollment, true)))
        assertTrue(createEnrollment().replayed)

        server.enqueue(strictJson(201, response(SYNTHETIC_SESSION_ID, enrollment, true)))
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }
        server.enqueue(strictJson(200, response(SYNTHETIC_SESSION_ID, enrollment, false)))
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }
        server.enqueue(
            strictJson(
                201,
                response("99999999-9999-4999-8999-999999999999", enrollment, false),
            ),
        )
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }
        server.enqueue(
            strictJson(
                201,
                response(
                    SYNTHETIC_SESSION_ID,
                    syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 31),
                    false,
                ),
            ),
        )
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }
        Unit
    }

    @Test
    fun consumeAcceptsOnlyMatchingStatusReplayPairAndExactSessionBody() = runBlocking {
        val session = syntheticSession(now)
        server.enqueue(strictJson(201, json.encodeToString(DeviceSessionMutationResponse(session, false))))
        val request = ConsumeDeviceEnrollmentRequest(
            session.id,
            syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 2),
            syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 3),
        )

        val mutation = transport.consumeEnrollment(
            baseUrl(),
            syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 4),
            request,
        )

        assertFalse(mutation.replayed)
        val recorded = server.takeRequest()
        assertEquals("/tenant/v1/auth/device-enrollments/consume", recorded.url.encodedPath)
        assertEquals("no-store", recorded.headers["Cache-Control"])
        assertEquals("no-cache", recorded.headers["Pragma"])

        server.enqueue(strictJson(200, json.encodeToString(DeviceSessionMutationResponse(session, false))))
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking {
                transport.consumeEnrollment(
                    baseUrl(),
                    syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 4),
                    request,
                )
            }
        }
        Unit
    }

    @Test
    fun onlyExactTrustedUnauthorizedBecomesAuthenticationFailure() {
        val request = RefreshDeviceSessionRequest(
            syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 5),
            syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 6),
        )
        server.enqueue(
            strictJson(401, unauthorizedBody())
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
        )
        assertThrows(DeviceAuthApiException.Authentication::class.java) {
            runBlocking {
                transport.refreshSession(
                    baseUrl(),
                    syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 7),
                    request,
                )
            }
        }

        val untrusted = listOf(
            MockResponse.Builder()
                .code(401)
                .addHeader("Content-Type", "application/json")
                .body(unauthorizedBody())
                .build(),
            strictJson(401, unauthorizedBody()),
            strictJson(401, """{"error":{"code":"forbidden","message":"synthetic"}}""")
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(401, """{"error":{"code":"unauthorized","message":"synthetic","future":true}}""")
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(401, """{"error":{"code":"unauthorized","message":"synthetic","details":null}}""")
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(401, unauthorizedBody(), contentType = "application/json-patch+json")
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            trustedUnauthorizedWith("Cache-Control", "no-store"),
            unauthorizedWithCacheControl("no-store, max-age=0,"),
            unauthorizedWithCacheControl("no-store, max-age=0, no-store"),
            trustedUnauthorizedWith("Pragma", "no-cache"),
            strictJson(401, unauthorizedBody(), contentType = "application/json; profile=synthetic")
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(
                401,
                unauthorizedBody(),
                contentType = "application/json; charset=utf-8; charset=utf-8",
            ).newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(401, unauthorizedBody(), contentType = "application/json; charset")
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(401, unauthorizedBody())
                .newBuilder()
                .addHeader("Content-Type", "application/json")
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
            strictJson(401, unauthorizedBody())
                .newBuilder()
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .build(),
        )
        untrusted.forEachIndexed { index, response ->
            server.enqueue(response)
            val failure = runCatching {
                runBlocking {
                    transport.refreshSession(
                        baseUrl(),
                        syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 7),
                        request,
                    )
                }
            }.exceptionOrNull()
            assertTrue(
                "Untrusted unauthorized fixture $index was classified as ${failure?.javaClass?.simpleName}",
                failure is DeviceAuthApiException.Unavailable,
            )
        }
    }

    @Test
    fun malformedUtf8UnauthorizedCannotTriggerCredentialRotation() {
        val invalidBody = Buffer()
            .writeUtf8("{\"error\":{\"code\":\"unauthorized\",\"message\":\"")
            .writeByte(0xc3)
            .writeUtf8("\"}}")
        server.enqueue(
            MockResponse.Builder()
                .code(401)
                .addHeader("Content-Type", "application/json; charset=utf-8")
                .addHeader("Cache-Control", "no-store, max-age=0")
                .addHeader("Pragma", "no-cache")
                .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
                .body(invalidBody)
                .build(),
        )

        assertThrows(DeviceAuthApiException.Unavailable::class.java) {
            runBlocking {
                transport.refreshSession(
                    baseUrl(),
                    syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 7),
                    RefreshDeviceSessionRequest(
                        syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 5),
                        syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 6),
                    ),
                )
            }
        }
    }

    @Test
    fun redirectIsNeverFollowedAndCannotBecomeCredentialRejection() {
        server.enqueue(
            strictJson(302, """{"error":{"code":"redirect","message":"synthetic"}}""")
                .newBuilder()
                .addHeader("Location", server.url("/credential-sink"))
                .build(),
        )

        assertThrows(DeviceAuthApiException.Unavailable::class.java) {
            runBlocking {
                transport.refreshSession(
                    baseUrl(),
                    syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 8),
                    RefreshDeviceSessionRequest(
                        syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 9),
                        syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 10),
                    ),
                )
            }
        }
        assertEquals(1, server.requestCount)
    }

    @Test
    fun revokeRequiresEmpty204AndNeverTreats404AsSuccess() = runBlocking {
        val access = syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 11)
        server.enqueue(strictNoContent())
        transport.revokeSession(baseUrl(), access, SYNTHETIC_SESSION_ID)
        val request = server.takeRequest()
        assertEquals("DELETE", request.method)
        assertEquals("/tenant/v1/auth/sessions/$SYNTHETIC_SESSION_ID", request.url.encodedPath)
        assertEquals("no-store", request.headers["Cache-Control"])
        assertEquals("no-cache", request.headers["Pragma"])

        server.enqueue(MockResponse.Builder().code(204).build())
        assertThrows(DeviceSessionDeleteOutcomeAmbiguousException::class.java) {
            runBlocking { transport.revokeSession(baseUrl(), access, SYNTHETIC_SESSION_ID) }
        }

        server.enqueue(strictJson(404, """{"error":{"code":"not_found","message":"synthetic"}}"""))
        val missing = assertThrows(DeviceAuthApiException.Http::class.java) {
            runBlocking { transport.revokeSession(baseUrl(), access, SYNTHETIC_SESSION_ID) }
        }
        assertEquals(404, missing.statusCode)
    }

    @Test
    fun revokeKeepsMalformedUnauthorizedAndClientContractsDeterministic() = runBlocking {
        val access = syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 111)
        server.enqueue(
            strictJson(
                401,
                """{"error":{"code":"unauthorized","message":"synthetic"}}""",
            ),
        )
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { transport.revokeSession(baseUrl(), access, SYNTHETIC_SESSION_ID) }
        }

        server.enqueue(strictJson(404, """{"error":{"code":"future","message":"synthetic"}}"""))
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { transport.revokeSession(baseUrl(), access, SYNTHETIC_SESSION_ID) }
        }

        val requestsBeforePreflight = server.requestCount
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { transport.revokeSession(baseUrl(), access, "not-a-session-id") }
        }
        assertEquals(requestsBeforePreflight, server.requestCount)
        Unit
    }

    @Test
    fun revokeTreatsRetryableResponseAsOutcomeAmbiguous() {
        val access = syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 112)
        listOf(408, 425, 429, 500, 504).forEach { status ->
            val response = MockResponse.Builder().code(status)
            if (status == 408) {
                response.addHeader("Retry-After", "1")
            }
            server.enqueue(response.build())
            assertThrows(DeviceAuthApiException.Unavailable::class.java) {
                runBlocking { transport.revokeSession(baseUrl(), access, SYNTHETIC_SESSION_ID) }
            }
        }
    }

    @Test
    fun successWithoutExactNoStoreOrJsonMediaTypeFailsClosed() {
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 12)
        val body = """{"id":"$SYNTHETIC_SESSION_ID","enrollment_token":"$enrollment","expires_at":"${now.plus(Duration.ofMinutes(10))}","client_contract_version":$DEVICE_AUTH_CONTRACT_VERSION,"replayed":false}"""
        server.enqueue(
            MockResponse.Builder()
                .code(201)
                .addHeader("Content-Type", "application/json")
                .addHeader("Cache-Control", "no-store")
                .addHeader("Pragma", "no-cache")
                .body(body)
                .build(),
        )
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }

        server.enqueue(strictJson(201, body, contentType = "application/json-seq"))
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }

        val expired = """{"id":"$SYNTHETIC_SESSION_ID","enrollment_token":"$enrollment","expires_at":"$now","client_contract_version":$DEVICE_AUTH_CONTRACT_VERSION,"replayed":false}"""
        server.enqueue(strictJson(201, expired))
        assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }
    }

    @Test
    fun canonicalBase64UrlAliasesAreRejected() {
        val canonical = syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 0)
        val payload = canonical.removePrefix(DEVICE_ACCESS_TOKEN_PREFIX)
        val canonicalLast = payload.last()
        val alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
        val index = alphabet.indexOf(canonicalLast)
        val aliasLast = alphabet[(index and 0b111100) + ((index + 1) and 0b11)]
        val alias = DEVICE_ACCESS_TOKEN_PREFIX + payload.dropLast(1) + aliasLast

        assertEquals(
            Base64.getUrlDecoder().decode(payload).toList(),
            Base64.getUrlDecoder().decode(alias.removePrefix(DEVICE_ACCESS_TOKEN_PREFIX)).toList(),
        )
        assertThrows(IllegalArgumentException::class.java) {
            validateExactDeviceToken(alias, DEVICE_ACCESS_TOKEN_PREFIX)
        }
    }

    @Test
    fun malformedSecretBearingResponseCannotEscapeThroughErrorDiagnostics() {
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 30)
        server.enqueue(
            strictJson(
                201,
                """{"id":"$SYNTHETIC_SESSION_ID","enrollment_token":"$enrollment","expires_at":""",
            ),
        )

        val failure = assertThrows(DeviceAuthApiException.InvalidResponse::class.java) {
            runBlocking { createEnrollment() }
        }

        assertNull(failure.cause)
        assertFalse(failure.toString().contains(enrollment))
    }

    private suspend fun createEnrollment(): DeviceEnrollmentIssuedResponse {
        val enrollment = syntheticDeviceToken(DEVICE_ENROLLMENT_TOKEN_PREFIX, 30)
        return transport.createEnrollment(
            buildEnrollmentCreationHttpRequest(
                baseUrl(),
                "synthetic-bootstrap",
                CreateDeviceEnrollmentRequest(
                    id = SYNTHETIC_SESSION_ID,
                    enrollmentToken = enrollment,
                    clientInstanceId = SYNTHETIC_CLIENT_INSTANCE_ID,
                    deviceLabel = SYNTHETIC_DEVICE_LABEL,
                    clientVersion = SYNTHETIC_CLIENT_VERSION,
                ),
                allowCleartextLoopback = true,
            ),
        )
    }

    private fun baseUrl(): String = server.url("/tenant/").toString()

    private fun strictJson(
        code: Int,
        body: String,
        contentType: String = "application/json; charset=utf-8",
    ): MockResponse = MockResponse.Builder()
        .code(code)
        .addHeader("Content-Type", contentType)
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .body(body)
        .build()

    private fun strictNoContent(): MockResponse = MockResponse.Builder()
        .code(204)
        .addHeader("Cache-Control", "no-store, max-age=0")
        .addHeader("Pragma", "no-cache")
        .build()

    private fun trustedUnauthorizedWith(name: String, duplicateValue: String): MockResponse =
        strictJson(401, unauthorizedBody())
            .newBuilder()
            .addHeader(name, duplicateValue)
            .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
            .build()

    private fun unauthorizedWithCacheControl(cacheControl: String): MockResponse =
        MockResponse.Builder()
            .code(401)
            .addHeader("Content-Type", "application/json")
            .addHeader("Cache-Control", cacheControl)
            .addHeader("Pragma", "no-cache")
            .addHeader("WWW-Authenticate", "Bearer realm=\"dayweave\"")
            .body(unauthorizedBody())
            .build()

    private fun unauthorizedBody(): String =
        """{"error":{"code":"unauthorized","message":"A valid bearer token is required"}}"""
}
