package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.SocketEffect
import okhttp3.Call
import okhttp3.EventListener
import okio.Buffer
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

class OkHttpRoutineOccurrenceTransportTest {
    private lateinit var server: MockWebServer
    private val transport = OkHttpRoutineOccurrenceTransport()
    @Before fun start() { server = MockWebServer(); server.start() }
    @After fun close() { server.close() }

    @Test fun getAndExactPutBindLedgerIdentityAndPrivacyHeadersWithoutRequiringPragma() = runBlocking {
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(routineTestSnapshot())))
        assertEquals(routineTestSnapshot(),transport.get(configuration(),ROUTINE_INSTANCE))
        val get = server.takeRequest()
        assertEquals("/synthetic/v1/routine-occurrences/$ROUTINE_INSTANCE",get.url.encodedPath)
        assertEquals("Bearer synthetic-routine-test",get.headers["Authorization"])
        assertEquals("no-store, max-age=0",get.headers["Cache-Control"])
        val raw = ITEM_PROGRESS_JSON.encodeToString(routineTestRequest())
        for (replayed in listOf(false,true)) {
            server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(routineTestMutation(replayed)),listOf(replayed.toString())))
            assertEquals(replayed,transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_CHILD,raw).replayed)
            val put = server.takeRequest()
            assertEquals("/synthetic/v1/routine-occurrences/$ROUTINE_INSTANCE/members/$ROUTINE_CHILD",put.url.encodedPath)
            assertEquals(raw,requireNotNull(put.body).utf8())
            assertNull(put.headers["Idempotency-Key"])
        }
    }

    @Test fun currentAndDeltaPagesUseBoundedOpaqueQueryAndNeverSplitAnInstance() = runBlocking {
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(routineTestPage())))
        assertEquals(routineTestPage(),transport.list(configuration(),limit=1))
        val list = server.takeRequest()
        assertEquals("/synthetic/v1/routine-occurrences",list.url.encodedPath)
        assertEquals("1",list.url.queryParameter("limit"))
        val cursor = "DWR1.synthetic:opaque+/="
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(routineTestPage())))
        transport.delta(configuration(),cursor,25)
        val delta = server.takeRequest()
        assertEquals("/synthetic/v1/routine-occurrences/delta",delta.url.encodedPath)
        assertEquals(cursor,delta.url.queryParameter("cursor"))
        val repeating = routineTestPage().copy(hasMore=true,cursor=cursor)
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(repeating)))
        assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.list(configuration(),cursor,1) } }
        val duplicates = routineTestPage().copy(changes=listOf(RoutineOccurrenceChange(1,routineTestSnapshot()),RoutineOccurrenceChange(2,routineTestSnapshot(true))))
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(duplicates)))
        assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.list(configuration(),limit=100) } }
        server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(duplicates)))
        assertEquals(2,transport.delta(configuration(),limit=100).changes.size)
    }

    @Test fun cursorsRequireGraphicAsciiAndNonemptyTerminalPagesMustAdvance() = runBlocking {
        val invalid = listOf("", " ", "a b", "é", "a\tb", "a\nb", "a\u007fb", "a".repeat(513))
        for (current in listOf(true, false)) {
            suspend fun page(cursor: String?) = if (current) transport.list(configuration(), cursor, 1)
                else transport.delta(configuration(), cursor, 1)
            for (cursor in invalid) {
                val requests = server.requestCount
                assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { page(cursor) } }
                assertEquals(requests, server.requestCount)
                server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(routineTestPage().copy(cursor = cursor))))
                assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { page(null) } }
            }
            val same = "DWR1.synthetic-unchanged"
            server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(routineTestPage().copy(cursor = same, hasMore = false))))
            assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { page(same) } }
            val empty = routineTestPage().copy(changes = emptyList(), cursor = same, hasMore = false)
            server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(empty)))
            assertEquals(empty, page(same))
            val graphic = "!" + "a".repeat(510) + "~"
            val bounded = empty.copy(cursor = graphic)
            server.enqueue(response(ITEM_PROGRESS_JSON.encodeToString(bounded)))
            assertEquals(bounded, page(graphic))
        }
    }

    @Test fun responseRejectsRawIntegerAliasesMissingFieldsWrongRouteAndPrivacyProof() {
        val body = ITEM_PROGRESS_JSON.encodeToString(routineTestSnapshot())
        val invalid = listOf(response(body,listOf("false")),response(body,cache="private"),
            response(body,media="text/html"),response(body,media="application/json; charset=utf-16"),
            response(body.replaceFirst("\"revision\":1","\"revision\":1,\"rev\\u0069sion\":1")),
            response(body.replaceFirst("\"revision\":1","\"revision\":1e0")),
            response(body.replaceFirst("\"revision\":1","\"revision\":1.0")),
            response(body.replaceFirst("\"provenance\":null,","")),
            response(body.replaceFirst("\"type\":\"calendar_day\",","")),
            response(body.replace(ROUTINE_INSTANCE,ROUTINE_OPERATION)))
        invalid.forEach { server.enqueue(it); assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) {
            runBlocking { transport.get(configuration(),ROUTINE_INSTANCE) }
        } }
    }

    @Test fun immutableReceiptNeedsSingleMatchingReplayAndExactBothCasAndReviewedAction() {
        val body = ITEM_PROGRESS_JSON.encodeToString(routineTestMutation())
        val raw = ITEM_PROGRESS_JSON.encodeToString(routineTestRequest())
        val invalid = listOf(response(body),response(body,listOf("false","false")),response(body,listOf("true")),
            response(body,listOf("False")),response(body.replace(ROUTINE_OPERATION,ROUTINE_ROOT),listOf("false")),
            response(body.replaceFirst("\"revision\":2","\"revision\":3"),listOf("false")),
            response(body.replace(ROUTINE_INSTANCE,ROUTINE_OPERATION),listOf("false")))
        invalid.forEach { server.enqueue(it); assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) {
            runBlocking { transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_CHILD,raw) }
        } }
        server.enqueue(response(body,listOf("false")))
        assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_OPERATION,raw) } }
        server.enqueue(response(body,listOf("false")))
        val skipped = ITEM_PROGRESS_JSON.encodeToString(routineTestRequest().copy(action=RoutineOccurrenceAction.SetOutcome("skipped")))
        assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_CHILD,skipped) } }
    }

    @Test fun malformedUtf8OversizedBodiesAndUnknownErrorsCannotReleaseIntent() {
        server.enqueue(MockResponse.Builder().code(200).addHeader("Content-Type","application/json")
            .addHeader("Cache-Control","no-store, max-age=0").body(Buffer().write(byteArrayOf(0xc3.toByte(),0x28))).build())
        assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.get(configuration(),ROUTINE_INSTANCE) } }
        server.enqueue(response(" ".repeat(MAX_ROUTINE_OCCURRENCE_BYTES+1)))
        assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.get(configuration(),ROUTINE_INSTANCE) } }
        for (status in listOf(400,409,500,503)) {
            server.enqueue(response("""{"error":{"code":"gateway_unknown","message":"Synthetic private text"}}""",status=status))
            val error = assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.get(configuration(),ROUTINE_INSTANCE) } }
            assertFalse(error.message.orEmpty().contains("Synthetic private text"))
        }
    }

    @Test fun onlyExactKnownErrorAndStatusPairHasDefinitiveMeaning() {
        val request = ITEM_PROGRESS_JSON.encodeToString(routineTestRequest())
        for (code in RoutineOccurrenceFailureCode.entries) {
            val body = """{"error":{"code":"${code.wire}","message":"Synthetic error"}}"""
            server.enqueue(response(body,status=code.status))
            assertEquals(code,assertThrows(RoutineOccurrenceApiException.Definitive::class.java) {
                runBlocking { transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_CHILD,request) }
            }.code)
            server.enqueue(response(body,status=502))
            assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) { runBlocking { transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_CHILD,request) } }
        }
        for (status in listOf(401,403)) {
            server.enqueue(response("""{"error":{"code":"forbidden","message":"Synthetic"}}""",status=status))
            assertThrows(RoutineOccurrenceApiException.Authentication::class.java) { runBlocking { transport.get(configuration(),ROUTINE_INSTANCE) } }
        }
    }

    @Test fun malformedErrorEnvelopesAndAnyErrorReplayHeaderCannotReleaseIntent() {
        val request = ITEM_PROGRESS_JSON.encodeToString(routineTestRequest())
        val code = RoutineOccurrenceFailureCode.INSTANCE_STALE.wire
        val exact = """{"error":{"code":"$code","message":"Synthetic private error"}}"""
        val malformed = listOf(
            """{"error":{"code":"$code"}}""",
            """{"error":{"code":"$code","message":null}}""",
            """{"error":{"code":"$code","message":1}}""",
            """{"error":{"code":"$code","message":true}}""",
            """{"error":{"code":"$code","message":{}}}""",
            """{"error":{"code":"$code","message":"Synthetic","extra":true}}""",
            """{"error":{"code":"$code","message":"Synthetic","details":{}}}""",
            """{"error":{"code":"$code","message":"Synthetic"},"extra":true}""",
            """{"error":{"code":"$code","message":"Synthetic","mess\u0061ge":"Synthetic"}}""",
        )
        val invalid = malformed.map { response(it,status=409) } + listOf(
            response(exact,listOf("false"),status=409),
            response(exact,listOf("true"),status=409),
            response(exact,listOf("false","false"),status=409),
            response(exact,listOf(""),status=409),
            response(exact,listOf("false"),status=401),
            response(exact,listOf("false"),status=403),
        )
        invalid.forEach {
            server.enqueue(it)
            val error = assertThrows(RoutineOccurrenceApiException.Uncertain::class.java) {
                runBlocking { transport.put(configuration(),ROUTINE_INSTANCE,ROUTINE_CHILD,request) }
            }
            assertFalse(error.message.orEmpty().contains("Synthetic private error"))
        }
    }

    @Test fun cancellationAfterFlushedHeadersClosesStalledBodyWithoutAdmittingSuccessOrFailure() = runBlocking {
        val callerThread = Thread.currentThread()
        for (status in listOf(200, 409)) {
            val bodyStarted = CompletableDeferred<Pair<Call, Thread>>()
            val client = OkHttpCanonicalPlannerTransport.defaultClient().newBuilder().eventListener(object : EventListener() {
                override fun responseBodyStart(call: Call) {
                    bodyStarted.complete(call to Thread.currentThread())
                }
            }).build()
            val body = if (status == 200) ITEM_PROGRESS_JSON.encodeToString(routineTestSnapshot())
                else """{"error":{"code":"routine_occurrence_instance_stale","message":"Synthetic"}}"""
            server.enqueue(MockResponse.Builder().code(status).addHeader("Content-Type", "application/json")
                .addHeader("Cache-Control", "no-store, max-age=0").body(body).onResponseBody(SocketEffect.Stall).build())
            val acceptedProof = AtomicBoolean(false)
            val attempt = async {
                try {
                    OkHttpRoutineOccurrenceTransport(client).get(configuration(), ROUTINE_INSTANCE)
                    acceptedProof.set(true)
                } catch (error: RoutineOccurrenceApiException.Definitive) {
                    acceptedProof.set(true)
                    throw error
                }
            }
            val (call, readerThread) = withTimeout(2_000) { bodyStarted.await() }
            assertNotSame("Blocking body reads must leave the caller dispatcher", callerThread, readerThread)
            attempt.cancel()
            withTimeout(2_000) { attempt.join() }
            assertTrue(attempt.isCancelled)
            assertTrue(call.isCanceled())
            assertFalse(acceptedProof.get())
            assertThrows(CancellationException::class.java) { runBlocking { attempt.await() } }
        }
    }

    @Test fun cancellationBeforeHeadersAndBeforeInitialDispatchNeverAdmitsAResponse() = runBlocking {
        val callStarted = CompletableDeferred<Call>()
        val client = OkHttpCanonicalPlannerTransport.defaultClient().newBuilder().eventListener(object : EventListener() {
            override fun callStart(call: Call) { callStarted.complete(call) }
        }).build()
        val scoped = OkHttpRoutineOccurrenceTransport(client)
        server.enqueue(MockResponse.Builder().onResponseStart(SocketEffect.Stall).build())
        val waiting = async { scoped.get(configuration(), ROUTINE_INSTANCE) }
        val call = withTimeout(2_000) { callStarted.await() }
        withContext(Dispatchers.IO) { assertNotNull(server.takeRequest(2, TimeUnit.SECONDS)) }
        waiting.cancel()
        withTimeout(2_000) { waiting.join() }
        assertTrue(call.isCanceled())
        assertThrows(CancellationException::class.java) { runBlocking { waiting.await() } }

        val requestsBefore = server.requestCount
        val alreadyCancelled = Job().apply { cancel() }
        val initial = async(alreadyCancelled, start = CoroutineStart.UNDISPATCHED) {
            scoped.get(configuration(), ROUTINE_INSTANCE)
        }
        withTimeout(2_000) { initial.join() }
        assertThrows(CancellationException::class.java) { runBlocking { initial.await() } }
        assertEquals(requestsBefore, server.requestCount)
    }

    private fun configuration() = AuthenticatedApiConfiguration.createForLoopbackTest(server.url("/synthetic/").toString(),"synthetic-routine-test")
    private fun response(body: String,replay: List<String> = emptyList(),status: Int = 200,cache: String = "no-store, max-age=0",media: String = "application/json") =
        MockResponse.Builder().code(status).addHeader("Content-Type",media).addHeader("Cache-Control",cache)
            .apply { replay.forEach { addHeader("Idempotency-Replayed",it) } }.body(body).build()
}
