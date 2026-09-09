package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.ITEM_PROGRESS_JSON
import com.greengolddog.dayweave.model.ItemProgressMutationResult
import com.greengolddog.dayweave.model.ItemProgressRequest
import com.greengolddog.dayweave.model.ItemProgressSnapshot
import com.greengolddog.dayweave.model.requireCanonicalUuid
import com.greengolddog.dayweave.model.decodeExactItemProgress
import com.greengolddog.dayweave.model.requireStrictItemProgressJson
import java.io.IOException
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

enum class ItemProgressFailureCode(val wire: String, val status: Int) {
    ITEM_STALE("item_progress_item_stale", 409),
    PROGRESS_STALE("item_progress_revision_stale", 409),
    OPERATION_REUSED("item_progress_operation_reused", 409),
    ITEM_MISSING("item_progress_item_missing", 404),
    INVALID("item_progress_invalid", 422),
}

sealed class ItemProgressApiException(message: String) : IOException(message) {
    class Definitive(val code: ItemProgressFailureCode) : ItemProgressApiException("Progress change requires review")
    class Authentication : ItemProgressApiException("Progress authentication is unavailable")
    class Uncertain : ItemProgressApiException("Progress response could not be verified")
}

interface ItemProgressTransport {
    suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String): ItemProgressSnapshot
    /** Exact encrypted request bytes, never regenerated for a replay. */
    suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemProgressMutationResult
}

class OkHttpItemProgressTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
) : ItemProgressTransport {
    override suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String): ItemProgressSnapshot {
        val request = request(configuration, itemId).get().build()
        val (body, _) = execute(configuration, request)
        return decode<ItemProgressSnapshot>(body).also {
            verify { it.requireValid(); require(it.itemId == itemId) }
        }
    }

    override suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemProgressMutationResult {
        val expected = decode<ItemProgressRequest>(requestJson).also { verify(it::requireValid) }
        verify { require(requestJson.toByteArray(Charsets.UTF_8).size <= 65_536) }
        val request = request(configuration, itemId).put(requestJson.toRequestBody(JSON_MEDIA_TYPE)).build()
        val (body, replayHeader) = execute(configuration, request)
        return decode<ItemProgressMutationResult>(body).also {
            verify {
                require(replayHeader == it.replayed.toString())
                require(it.operationId == expected.operationId)
                it.progress.requireValid()
                require(it.progress.itemId == itemId && it.progress.itemRevision == expected.expectedItemRevision)
                require(it.progress.revision == Math.addExact(expected.expectedProgressRevision, 1))
                require(it.progress.components == expected.components)
            }
        }
    }

    private fun request(configuration: AuthenticatedApiConfiguration, itemId: String): Request.Builder {
        verify { requireCanonicalUuid(itemId, "progress item") }
        return Request.Builder().url(configuration.baseUrl.newBuilder().addPathSegments("v1/items")
            .addPathSegment(itemId).addPathSegment("progress").build())
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", "application/json")
            .header("Cache-Control", "no-store, max-age=0")
            .header("Pragma", "no-cache")
            .header("Authorization", "Bearer ${configuration.bearerToken}")
    }

    private suspend fun execute(configuration: AuthenticatedApiConfiguration, request: Request): Pair<String, String?> {
        configuration.executeAuthenticated(client, request).use { response ->
            requireNoStore(response)
            val body = response.body.charStream().use { reader ->
                val buffer = CharArray(4_096)
                val result = StringBuilder()
                while (true) {
                    val count = reader.read(buffer)
                    if (count < 0) break
                    if (result.length + count > 131_072) throw ItemProgressApiException.Uncertain()
                    result.append(buffer, 0, count)
                }
                result.toString()
            }
            if (response.code != 200) {
                if (response.code in setOf(401, 403)) throw ItemProgressApiException.Authentication()
                val code = runCatching { requireStrictItemProgressJson(body); ITEM_PROGRESS_JSON.parseToJsonElement(body).jsonObject
                    .getValue("error").jsonObject.getValue("code").jsonPrimitive.content }.getOrNull()
                val definitive = ItemProgressFailureCode.entries.singleOrNull { it.wire == code && it.status == response.code }
                if (definitive != null) throw ItemProgressApiException.Definitive(definitive)
                throw ItemProgressApiException.Uncertain()
            }
            val replayHeaders = response.headers.values("Idempotency-Replayed")
            if (request.method == "PUT" && replayHeaders.size != 1) throw ItemProgressApiException.Uncertain()
            if (request.method == "GET" && replayHeaders.isNotEmpty()) throw ItemProgressApiException.Uncertain()
            return body to replayHeaders.singleOrNull()
        }
    }

    private fun requireNoStore(response: Response) {
        val directives = response.headers.values("Cache-Control").flatMap { it.lowercase().split(',') }.map(String::trim)
        if ("no-store" !in directives || "max-age=0" !in directives || response.header("Pragma")?.lowercase() != "no-cache") {
            throw ItemProgressApiException.Uncertain()
        }
    }

    private inline fun <reified T> decode(body: String): T = try {
        decodeExactItemProgress<T>(body)
    } catch (_: IllegalArgumentException) { throw ItemProgressApiException.Uncertain() }

    private inline fun verify(block: () -> Unit) {
        try { block() } catch (_: IllegalArgumentException) { throw ItemProgressApiException.Uncertain() }
        catch (_: ArithmeticException) { throw ItemProgressApiException.Uncertain() }
    }

    private companion object { val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType() }
}
