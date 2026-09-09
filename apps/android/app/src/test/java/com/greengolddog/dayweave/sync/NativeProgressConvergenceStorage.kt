package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.PlannerSnapshotDao
import com.greengolddog.dayweave.data.PlannerSnapshotEntity
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.TRUNCATE_EXISTING
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** Test-only disk adapter; production snapshot codec, not a claim to exercise SQLCipher/Keystore. */
internal class NativeConvergenceSnapshotDao(private val disk: NativeConvergenceDisk) : PlannerSnapshotDao {
    override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = disk.read("snapshot")?.let {
        val row = Json.decodeFromString<NativeConvergenceDiskRow>(it)
        require(row.singletonId == singletonId)
        PlannerSnapshotEntity(row.singletonId, row.payload, row.updatedAtEpochMillis, row.payloadFormat)
    }

    override suspend fun save(snapshot: PlannerSnapshotEntity) = disk.write("snapshot", Json.encodeToString(
        NativeConvergenceDiskRow(snapshot.singletonId, snapshot.payload, snapshot.updatedAtEpochMillis, snapshot.payloadFormat),
    ))
}

@Serializable
private data class NativeConvergenceDiskRow(
    val singletonId: Int,
    val payload: String,
    val updatedAtEpochMillis: Long,
    val payloadFormat: String,
)

/** Each fresh synthetic run owns a 0700 directory and an independent 0600 AES-256-GCM test key. */
internal class NativeConvergenceDisk(
    private val directory: Path,
    private val binding: String,
    prepare: Boolean,
) {
    private val key: SecretKeySpec

    init {
        if (prepare) {
            require(!Files.exists(directory, NOFOLLOW_LINKS)) { "Synthetic Android state already exists" }
            Files.createDirectory(directory, PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")))
        }
        requirePrivateConvergencePath(directory, directory = true)
        val keyPath = directory.resolve("synthetic-test-key.bin")
        if (prepare) atomicPrivateConvergenceWrite(keyPath, ByteArray(32).also(SecureRandom()::nextBytes))
        requirePrivateConvergencePath(keyPath)
        key = SecretKeySpec(Files.readAllBytes(keyPath).also { require(it.size == 32) }, "AES")
    }

    fun read(name: String): String? {
        val path = directory.resolve("$name.aesgcm")
        if (!Files.exists(path, NOFOLLOW_LINKS)) return null
        requirePrivateConvergencePath(path)
        require(Files.size(path) in 30..16_777_216)
        val sealed = Files.readAllBytes(path)
        require(sealed[0] == 1.toByte())
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, sealed.copyOfRange(1, 13)))
        cipher.updateAAD("$binding|$name".toByteArray(Charsets.UTF_8))
        return cipher.doFinal(sealed.copyOfRange(13, sealed.size)).toString(Charsets.UTF_8)
    }

    fun write(name: String, payload: String) {
        require(name.matches(Regex("[a-z_]+")))
        val nonce = ByteArray(12).also(SecureRandom()::nextBytes)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key, GCMParameterSpec(128, nonce))
        cipher.updateAAD("$binding|$name".toByteArray(Charsets.UTF_8))
        atomicPrivateConvergenceWrite(directory.resolve("$name.aesgcm"),
            byteArrayOf(1) + nonce + cipher.doFinal(payload.toByteArray(Charsets.UTF_8)))
    }
}

internal fun requirePrivateConvergencePath(path: Path, directory: Boolean = false) {
    require(path.isAbsolute && !Files.isSymbolicLink(path)) { "Convergence paths must be absolute and non-symlink" }
    require(if (directory) Files.isDirectory(path, NOFOLLOW_LINKS) else Files.isRegularFile(path, NOFOLLOW_LINKS))
    require(Files.getPosixFilePermissions(path, NOFOLLOW_LINKS) ==
        PosixFilePermissions.fromString(if (directory) "rwx------" else "rw-------")) {
        "Convergence artifacts must be owner-private"
    }
}

internal fun atomicPrivateConvergenceWrite(path: Path, bytes: ByteArray) {
    if (Files.exists(path, NOFOLLOW_LINKS)) requirePrivateConvergencePath(path)
    val temporary = Files.createTempFile(path.parent, ".convergence-", ".tmp",
        PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------")))
    try {
        FileChannel.open(temporary, WRITE, TRUNCATE_EXISTING).use { channel ->
            val buffer = ByteBuffer.wrap(bytes)
            while (buffer.hasRemaining()) channel.write(buffer)
            channel.force(true)
        }
        Files.move(temporary, path, ATOMIC_MOVE, REPLACE_EXISTING)
        FileChannel.open(path.parent, READ).use { it.force(true) }
    } finally {
        Files.deleteIfExists(temporary)
    }
}

internal fun convergenceSha256(value: String): String = MessageDigest.getInstance("SHA-256")
    .digest(value.toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it.toInt() and 255) }
