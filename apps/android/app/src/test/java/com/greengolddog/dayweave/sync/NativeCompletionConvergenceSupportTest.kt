package com.greengolddog.dayweave.sync

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.PosixFilePermissions
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** Network-free opt-in and private artifact checks; no owner account or service is consulted. */
class NativeCompletionConvergenceSupportTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test fun onlyTwoCompletelyAbsentEnvironmentVariablesSkip() {
        assertFalse(nativeConvergenceOptIn(null, null))
        assertTrue(nativeConvergenceOptIn("/private/synthetic.json", "prepare_offline"))
        for ((path, phase) in listOf(null to "prepare_offline", "synthetic" to null, "" to "prepare_offline", "synthetic" to "")) {
            assertThrows(IllegalArgumentException::class.java) { nativeConvergenceOptIn(path, phase) }
        }
    }

    @Test fun strictConfigurationRejectsOtherOriginsBindingsAndUnknownFields() {
        val root = directory()
        val valid = config(root)
        val admitted = parse(root, "valid.json", valid)
        assertEquals(root, admitted.workDirectory)
        assertEquals(5, admitted.finalIds.size)
        val invalid = listOf(
            JsonObject(valid + ("base_url" to JsonPrimitive("https://api.example.test/"))),
            JsonObject(valid + ("base_url" to JsonPrimitive("http://localhost:54321/"))),
            JsonObject(valid + ("base_url" to JsonPrimitive("http://127.0.0.1:54321/other/"))),
            JsonObject(valid + ("bearer_token" to JsonPrimitive("unrelated-owner-token"))),
            JsonObject(valid + ("bearer_token" to JsonPrimitive("native-completion-synthetic-test-only"))),
            JsonObject(valid + ("bearer_token" to JsonPrimitive("dw_da1_" + "A".repeat(42)))),
            JsonObject(valid + ("bearer_token" to JsonPrimitive("dw_da1_" + "A".repeat(44)))),
            JsonObject(valid + ("bearer_token" to JsonPrimitive("dw_da1_" + "A".repeat(42) + "+"))),
            JsonObject(valid + ("branch_id" to valid.getValue("root_id"))),
            JsonObject(valid + ("unexpected_authority" to JsonPrimitive(true))),
            JsonObject(valid - "new_child_id"),
        )
        invalid.forEachIndexed { index, value -> assertThrows(IllegalArgumentException::class.java) { parse(root, "bad$index.json", value) } }
        val duplicated = root.resolve("duplicate.json")
        writeNativeCompletionPrivateBytes(duplicated, valid.toString().replace("\"schema_version\":1", "\"schema_version\":1,\"schema_version\":1"))
        assertThrows(IllegalArgumentException::class.java) { NativeCompletionConfig.read(duplicated) }
    }

    @Test fun markersAndRawCommandEvidenceArePrivateAndNeverOverwritten() {
        val root = directory()
        val marker = root.resolve("prepare_offline.json")
        val value = buildJsonObject { put("schema_version", 1); put("status", "passed") }
        writeNativeCompletionMarker(marker, value)
        requirePrivateConvergencePath(marker)
        assertEquals(value.toString(), Files.readAllBytes(marker).toString(Charsets.UTF_8))
        assertThrows(java.nio.file.FileAlreadyExistsException::class.java) { writeNativeCompletionMarker(marker, buildJsonObject {}) }
        assertEquals(value.toString(), Files.readAllBytes(marker).toString(Charsets.UTF_8))
        val command = root.resolve("operation-b.json")
        val bytes = " \n{\"operation_id\":\"synthetic\"}\n "
        writeNativeCompletionPrivateBytes(command, bytes)
        assertEquals(bytes, Files.readAllBytes(command).toString(Charsets.UTF_8))
        requirePrivateConvergencePath(command)
        assertThrows(java.nio.file.FileAlreadyExistsException::class.java) { writeNativeCompletionPrivateBytes(command, "changed") }
        assertEquals(bytes, Files.readAllBytes(command).toString(Charsets.UTF_8))
    }

    @Test fun scopedExecutorRejectsUnrelatedHostProviderExecutionAndPrematureItemWritesWithoutHttp() {
        val root = directory()
        val config = parse(root, "config.json", config(root))
        val credentials = NativeCompletionCredentials(config, "prepare_offline")
        val configuration = credentials.authenticatedConfiguration()
        val client = OkHttpClient()
        try {
            for (url in listOf("https://api.example.test/v1/items/delta", "http://127.0.0.1:54321/v1/google/accounts",
                "http://127.0.0.1:54321/v1/execution/current", "http://127.0.0.1:54321/v1/items/${config.newChildId}")) {
                assertThrows(IllegalArgumentException::class.java) { runBlocking {
                    credentials.executeAuthenticated(configuration, client, Request.Builder().url(url).get().build())
                } }
            }
            assertEquals(0, credentials.requestCount)
        } finally { client.dispatcher.executorService.shutdown(); client.connectionPool.evictAll() }
    }

    private fun directory(): Path = Files.createDirectory(temporary.root.toPath().resolve("dayweave-native-completion.fixture"),
        PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------"))).toRealPath()
    private fun parse(root: Path, name: String, value: JsonObject): NativeCompletionConfig {
        val path = root.resolve(name)
        writeNativeCompletionPrivateBytes(path, value.toString())
        return NativeCompletionConfig.read(path)
    }
    private fun config(root: Path) = buildJsonObject {
        put("schema_version", 1); put("run_id", "synthetic-check")
        put("base_url", "http://127.0.0.1:54321/"); put("bearer_token", "dw_da1_" + "A".repeat(43))
        put("work_directory", root.toString())
        listOf("root_id", "branch_id", "required_leaf_id", "optional_leaf_id", "new_child_id").forEachIndexed { index, name ->
            put(name, "00000000-0000-4000-8000-00000000000${index + 1}")
        }
    }
}
