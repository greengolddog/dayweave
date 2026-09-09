package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.*
import java.io.IOException
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

enum class ItemCompletionFailureCode(val wire: String, val status: Int) {
    ITEM_STALE("item_completion_item_stale", 409),
    POLICY_STALE("item_completion_revision_stale", 409),
    EVIDENCE_STALE("item_completion_evidence_stale", 409),
    OPERATION_REUSED("item_completion_operation_reused", 409),
    ITEM_MISSING("item_completion_item_missing", 404),
    INVALID("item_completion_invalid", 422),
    PARENT_REQUIRED("item_completion_parent_required", 422),
    REOPENING_REQUIRED("item_completion_reopening_review_required", 409),
    OCCURRENCE_REQUIRED("item_completion_occurrence_evidence_required", 409),
    EXECUTION_CONFLICT("item_completion_execution_conflict", 409),
    TOO_LARGE("item_completion_too_large", 413),
}

sealed class ItemCompletionApiException(message: String) : IOException(message) {
    class Definitive(val code: ItemCompletionFailureCode) : ItemCompletionApiException("Completion requires review")
    class Authentication : ItemCompletionApiException("Completion authentication is unavailable")
    class Uncertain : ItemCompletionApiException("Completion response could not be verified")
}

interface ItemCompletionTransport {
    suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String): ItemCompletionSnapshot
    suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemCompletionMutationResult
}

class OkHttpItemCompletionTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
) : ItemCompletionTransport {
    override suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String): ItemCompletionSnapshot {
        val (body, _) = execute(configuration, request(configuration, itemId).get().build())
        return decode<ItemCompletionSnapshot>(body).also {
            verify { it.requireValid(); require(it.itemId == itemId) }
        }
    }

    override suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemCompletionMutationResult {
        val expected = decode<ItemCompletionRequest>(requestJson).also { verify { it.requireValid(itemId) } }
        verify { require(requestJson.toByteArray(Charsets.UTF_8).size <= 16_384) }
        val request = request(configuration, itemId).put(requestJson.toRequestBody(JSON_MEDIA_TYPE)).build()
        val (body, replay) = execute(configuration, request)
        return decode<ItemCompletionMutationResult>(body).also {
            verify { require(replay == it.replayed.toString()); it.requireMatches(itemId, expected) }
        }
    }

    private fun request(configuration: AuthenticatedApiConfiguration, itemId: String): Request.Builder {
        verify { requireCanonicalUuid(itemId, "completion item") }
        return Request.Builder().url(configuration.baseUrl.newBuilder().addPathSegments("v1/items")
            .addPathSegment(itemId).addPathSegment("completion").build())
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", "application/json")
            .header("Cache-Control", "no-store, max-age=0")
            .header("Pragma", "no-cache")
            .header("Authorization", "Bearer ${configuration.bearerToken}")
    }

    private suspend fun execute(configuration: AuthenticatedApiConfiguration, request: Request): Pair<String, String?> {
        configuration.executeAuthenticated(client, request).use { response ->
            val directives = response.headers.values("Cache-Control").flatMap { it.lowercase().split(',') }.map(String::trim)
            if ("no-store" !in directives || "max-age=0" !in directives ||
                response.headers.values("Pragma") != listOf("no-cache")) throw ItemCompletionApiException.Uncertain()
            val body = response.body.charStream().use { reader ->
                val buffer = CharArray(4_096)
                val result = StringBuilder()
                while (true) {
                    val count = reader.read(buffer)
                    if (count < 0) break
                    if (result.length + count > 65_536) throw ItemCompletionApiException.Uncertain()
                    result.append(buffer, 0, count)
                }
                result.toString()
            }
            if (response.code != 200) {
                if (response.code in setOf(401, 403)) throw ItemCompletionApiException.Authentication()
                val code = runCatching {
                    requireStrictItemProgressJson(body)
                    ITEM_PROGRESS_JSON.parseToJsonElement(body).jsonObject.getValue("error").jsonObject
                        .getValue("code").jsonPrimitive.content
                }.getOrNull()
                val definitive = ItemCompletionFailureCode.entries.singleOrNull { it.wire == code && it.status == response.code }
                if (definitive != null) throw ItemCompletionApiException.Definitive(definitive)
                throw ItemCompletionApiException.Uncertain()
            }
            val replay = response.headers.values("Idempotency-Replayed")
            if (request.method == "PUT" && replay.size != 1 || request.method == "GET" && replay.isNotEmpty()) {
                throw ItemCompletionApiException.Uncertain()
            }
            return body to replay.singleOrNull()
        }
    }

    private inline fun <reified T> decode(body: String): T = try { decodeExactItemCompletion<T>(body) }
    catch (_: Exception) { throw ItemCompletionApiException.Uncertain() }

    private inline fun verify(block: () -> Unit) {
        try { block() } catch (_: IllegalArgumentException) { throw ItemCompletionApiException.Uncertain() }
        catch (_: IllegalStateException) { throw ItemCompletionApiException.Uncertain() }
        catch (_: ArithmeticException) { throw ItemCompletionApiException.Uncertain() }
    }

    private companion object { val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType() }
}
