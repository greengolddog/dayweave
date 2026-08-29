import CryptoKit
import Darwin
import Foundation
import Security

@MainActor
protocol CodexRuntimeLaunching {
    func launch() throws -> CodexRuntimeSession
}

@MainActor
final class CodexRuntimeSession {
    let process: Process
    let input: FileHandle
    let output: FileHandle
    let codexHome: URL

    private let runtimeRoot: URL
    private let runtimeRootIdentity: CodexFileIdentity
    private let runtimeLease: FileHandle?
    private var didCleanUp = false

    init(
        process: Process,
        input: FileHandle,
        output: FileHandle,
        codexHome: URL,
        runtimeRoot: URL,
        runtimeRootIdentity: CodexFileIdentity,
        runtimeLease: FileHandle? = nil
    ) {
        self.process = process
        self.input = input
        self.output = output
        self.codexHome = codexHome
        self.runtimeRoot = runtimeRoot
        self.runtimeRootIdentity = runtimeRootIdentity
        self.runtimeLease = runtimeLease
    }

    func cleanUpAfterTermination() {
        guard !didCleanUp, !process.isRunning else { return }
        didCleanUp = true
        try? input.close()
        try? output.close()
        if (try? CodexRuntimeLauncher.identity(of: runtimeRoot, followSymlink: false))
            == runtimeRootIdentity {
            try? FileManager.default.removeItem(at: runtimeRoot)
        }
        try? runtimeLease?.close()
    }
}

struct CodexRuntimeLauncher: CodexRuntimeLaunching {
    static let runtimeVersion = "0.150.1"
    static let unavailableMessage =
        "The verified Codex runtime is not sealed into this DayWeave build."

    private static let runtimeSHA256 =
        "a14f9a907c12c8812878b70e6b7d65f81c39ed795513e46a55817d7428c0ca6b"
    private static let manifestSHA256 =
        "e95c31a03fe867f7242d995ad099ca6903c432876ef70068d90385b1d5230084"
    private static let legacySchemaSHA256 =
        "18ba0e2282f69f7b3a05ffdc8ab0801c1468f25d72de3b4a37f1c8be67432a1d"
    private static let v2SchemaSHA256 =
        "8cdccfc35582696d7141e7f916e0d5a664ab5b5e90b732f104284d2507f369f8"
    private static let runtimeRequirement =
        "identifier \"codex\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"2DC432GLL2\""
    private static let maximumRuntimeBytes: UInt64 = 256 * 1_048_576
    private static let maximumManifestBytes: UInt64 = 16 * 1_024
    private static let maximumSchemaBytes: UInt64 = 2 * 1_048_576

    func launch() throws -> CodexRuntimeSession {
        let sealed = try verifySealedResources()
        let directories = try preparePrivateDirectories()
        try removeAbandonedRuntimeCopies(in: directories.runtimeParent)
        let runtimeRoot = directories.runtimeParent
            .appendingPathComponent(UUID().uuidString.lowercased(), isDirectory: true)
        try createPrivateDirectory(runtimeRoot)
        let rootIdentity = try Self.identity(of: runtimeRoot, followSymlink: false)
        let runtimeLease: FileHandle
        do {
            runtimeLease = try createRuntimeLease(in: runtimeRoot)
        } catch {
            if (try? Self.identity(of: runtimeRoot, followSymlink: false)) == rootIdentity {
                try? FileManager.default.removeItem(at: runtimeRoot)
            }
            throw error
        }

        do {
            let runtimeCopy = runtimeRoot.appendingPathComponent("codex", isDirectory: false)
            try FileManager.default.copyItem(at: sealed.runtime, to: runtimeCopy)
            guard chmod(runtimeCopy.path, S_IRUSR | S_IXUSR) == 0 else {
                throw CodexRuntimeLaunchError.runtimeUnavailable
            }
            try verifyRuntime(runtimeCopy)

            let inputPipe = Pipe()
            let outputPipe = Pipe()
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/sandbox-exec")
            process.arguments = [
                "-p",
                try sandboxProfile(
                    codexHome: directories.codexHome,
                    runtimeRoot: runtimeRoot,
                    runtime: runtimeCopy
                ),
                runtimeCopy.path,
                "app-server",
                "--stdio",
                "--strict-config",
                "-c", "cli_auth_credentials_store=\"file\"",
                "-c", "check_for_update_on_startup=false",
                "-c", "analytics.enabled=false",
                "-c", "agents.enabled=false",
                "-c", "tools.web_search=false",
                "-c", "approval_policy=\"never\"",
                "-c", "sandbox_mode=\"read-only\"",
                "-c", "allow_login_shell=false",
                "-c", "shell_environment_policy.inherit=\"none\"",
                "-c", "shell_environment_policy.ignore_default_excludes=false",
            ]
            process.standardInput = inputPipe
            process.standardOutput = outputPipe
            process.standardError = FileHandle.nullDevice
            process.currentDirectoryURL = directories.codexHome
            process.environment = [
                "CODEX_HOME": directories.codexHome.path,
                "HOME": directories.codexHome.path,
                "TMPDIR": directories.temporary.path + "/",
                "LANG": "en_US.UTF-8",
            ]
            try process.run()
            return CodexRuntimeSession(
                process: process,
                input: inputPipe.fileHandleForWriting,
                output: outputPipe.fileHandleForReading,
                codexHome: directories.codexHome,
                runtimeRoot: runtimeRoot,
                runtimeRootIdentity: rootIdentity,
                runtimeLease: runtimeLease
            )
        } catch {
            if (try? Self.identity(of: runtimeRoot, followSymlink: false)) == rootIdentity {
                try? FileManager.default.removeItem(at: runtimeRoot)
            }
            try? runtimeLease.close()
            throw error
        }
    }

    private func verifySealedResources() throws -> SealedResources {
        guard let resources = Bundle.main.resourceURL else {
            throw CodexRuntimeLaunchError.runtimeUnavailable
        }
        let root = resources
            .appendingPathComponent("CodexRuntime", isDirectory: true)
            .appendingPathComponent(Self.runtimeVersion, isDirectory: true)
        let sealed = SealedResources(
            runtime: root.appendingPathComponent("codex", isDirectory: false),
            manifest: root.appendingPathComponent("manifest.json", isDirectory: false),
            legacySchema: root.appendingPathComponent(
                "codex_app_server_protocol.schemas.json",
                isDirectory: false
            ),
            v2Schema: root.appendingPathComponent(
                "codex_app_server_protocol.v2.schemas.json",
                isDirectory: false
            )
        )
        for url in [sealed.runtime, sealed.manifest, sealed.legacySchema, sealed.v2Schema] {
            try requireNoSymlinkComponents(url)
            try requireRegularUnsymlinkedFile(url)
        }
        try requireHash(
            sealed.manifest,
            expected: Self.manifestSHA256,
            maximumBytes: Self.maximumManifestBytes
        )
        try requireHash(
            sealed.legacySchema,
            expected: Self.legacySchemaSHA256,
            maximumBytes: Self.maximumSchemaBytes
        )
        try requireHash(
            sealed.v2Schema,
            expected: Self.v2SchemaSHA256,
            maximumBytes: Self.maximumSchemaBytes
        )
        try verifyRuntime(sealed.runtime)
        return sealed
    }

    private func verifyRuntime(_ runtime: URL) throws {
        try requireRegularUnsymlinkedFile(runtime)
        try requireHash(
            runtime,
            expected: Self.runtimeSHA256,
            maximumBytes: Self.maximumRuntimeBytes
        )
        var staticCode: SecStaticCode?
        guard SecStaticCodeCreateWithPath(runtime as CFURL, [], &staticCode) == errSecSuccess,
              let staticCode else {
            throw CodexRuntimeLaunchError.runtimeUnavailable
        }
        var requirement: SecRequirement?
        guard SecRequirementCreateWithString(
            Self.runtimeRequirement as CFString,
            [],
            &requirement
        ) == errSecSuccess,
            let requirement else {
            throw CodexRuntimeLaunchError.runtimeUnavailable
        }
        let flags = SecCSFlags(rawValue: kSecCSStrictValidate | kSecCSCheckAllArchitectures)
        guard SecStaticCodeCheckValidity(staticCode, flags, requirement) == errSecSuccess else {
            throw CodexRuntimeLaunchError.runtimeUnavailable
        }
    }

    private func preparePrivateDirectories() throws -> PrivateDirectories {
        let manager = FileManager.default
        let applicationSupport = try manager.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        )
        try requireNoSymlinkComponents(applicationSupport)
        let supportIdentity = try Self.identity(of: applicationSupport, followSymlink: false)
        guard supportIdentity.kind == .directory,
              supportIdentity.owner == geteuid(),
              supportIdentity.permissions & 0o022 == 0 else {
            throw CodexRuntimeLaunchError.unsafeStorage
        }
        let dayWeave = applicationSupport.appendingPathComponent("DayWeave", isDirectory: true)
        let codexHome = dayWeave.appendingPathComponent("CodexHome", isDirectory: true)
        let temporary = codexHome.appendingPathComponent("tmp", isDirectory: true)
        let runtimeParent = dayWeave.appendingPathComponent("CodexRuntime", isDirectory: true)
        for directory in [dayWeave, codexHome, temporary, runtimeParent] {
            try createPrivateDirectory(directory)
        }
        var homeValues = URLResourceValues()
        homeValues.isExcludedFromBackup = true
        var mutableHome = codexHome
        try? mutableHome.setResourceValues(homeValues)
        return PrivateDirectories(
            codexHome: codexHome,
            temporary: temporary,
            runtimeParent: runtimeParent
        )
    }

    private func createPrivateDirectory(_ url: URL) throws {
        let manager = FileManager.default
        if !manager.fileExists(atPath: url.path) {
            try requireNoSymlinkComponents(url.deletingLastPathComponent())
            try manager.createDirectory(
                at: url,
                withIntermediateDirectories: false,
                attributes: [.posixPermissions: 0o700]
            )
        }
        try requireNoSymlinkComponents(url)
        let identity = try Self.identity(of: url, followSymlink: false)
        guard identity.kind == .directory,
              identity.owner == geteuid(),
              identity.permissions & 0o077 == 0 else {
            throw CodexRuntimeLaunchError.unsafeStorage
        }
    }

    private func createRuntimeLease(in runtimeRoot: URL) throws -> FileHandle {
        let lease = runtimeRoot.appendingPathComponent(".lease", isDirectory: false)
        let descriptor: Int32 = lease.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(
                path,
                O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW,
                S_IRUSR | S_IWUSR
            )
        }
        guard descriptor >= 0 else { throw CodexRuntimeLaunchError.unsafeStorage }
        guard Darwin.fchmod(descriptor, S_IRUSR | S_IWUSR) == 0,
              Darwin.lockf(descriptor, F_TLOCK, 0) == 0 else {
            Darwin.close(descriptor)
            try? FileManager.default.removeItem(at: lease)
            throw CodexRuntimeLaunchError.unsafeStorage
        }
        return FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
    }

    private func removeAbandonedRuntimeCopies(in runtimeParent: URL) throws {
        let manager = FileManager.default
        let candidates = try manager.contentsOfDirectory(
            at: runtimeParent,
            includingPropertiesForKeys: nil,
            options: []
        )
        guard candidates.count <= 256 else { throw CodexRuntimeLaunchError.unsafeStorage }

        for candidate in candidates {
            let name = candidate.lastPathComponent
            guard let identifier = UUID(uuidString: name),
                  identifier.uuidString.lowercased() == name else {
                throw CodexRuntimeLaunchError.unsafeStorage
            }
            let rootIdentity = try Self.identity(of: candidate, followSymlink: false)
            guard rootIdentity.kind == .directory,
                  rootIdentity.owner == geteuid(),
                  rootIdentity.permissions & 0o077 == 0 else {
                throw CodexRuntimeLaunchError.unsafeStorage
            }

            let lease = candidate.appendingPathComponent(".lease", isDirectory: false)
            guard manager.fileExists(atPath: lease.path) else {
                // Runtime copies from the pre-lease launcher are left intact.
                // They can be removed after confirming no older app instance is
                // using them; all new copies are reclaimed automatically.
                continue
            }
            let leaseIdentity = try Self.identity(of: lease, followSymlink: false)
            guard leaseIdentity.kind == .regular,
                  leaseIdentity.owner == geteuid(),
                  leaseIdentity.permissions & 0o077 == 0 else {
                throw CodexRuntimeLaunchError.unsafeStorage
            }
            let descriptor: Int32 = lease.withUnsafeFileSystemRepresentation { path -> Int32 in
                guard let path else { return -1 }
                return Darwin.open(path, O_RDWR | O_NOFOLLOW)
            }
            guard descriptor >= 0 else { throw CodexRuntimeLaunchError.unsafeStorage }
            defer { Darwin.close(descriptor) }

            if Darwin.lockf(descriptor, F_TLOCK, 0) != 0 {
                guard errno == EACCES || errno == EAGAIN else {
                    throw CodexRuntimeLaunchError.unsafeStorage
                }
                continue
            }
            guard (try? Self.identity(of: candidate, followSymlink: false)) == rootIdentity else {
                throw CodexRuntimeLaunchError.unsafeStorage
            }
            try manager.removeItem(at: candidate)
        }
    }

    private func requireRegularUnsymlinkedFile(_ url: URL) throws {
        let identity = try Self.identity(of: url, followSymlink: false)
        guard identity.kind == .regular,
              identity.owner == geteuid(),
              identity.permissions & 0o022 == 0 else {
            throw CodexRuntimeLaunchError.runtimeUnavailable
        }
    }

    private func requireNoSymlinkComponents(_ url: URL) throws {
        let standardized = url.standardizedFileURL
        guard standardized.isFileURL, standardized.path.hasPrefix("/") else {
            throw CodexRuntimeLaunchError.unsafeStorage
        }
        let components = standardized.pathComponents
        var current = URL(fileURLWithPath: "/", isDirectory: true)
        for (index, component) in components.enumerated() where component != "/" {
            current.appendPathComponent(component)
            let identity = try Self.identity(of: current, followSymlink: false)
            if index < components.count - 1, identity.kind != .directory {
                throw CodexRuntimeLaunchError.unsafeStorage
            }
        }
    }

    private func requireHash(_ url: URL, expected: String, maximumBytes: UInt64) throws {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        guard let size = attributes[.size] as? NSNumber,
              size.uint64Value > 0,
              size.uint64Value <= maximumBytes,
              try sha256(url) == expected else {
            throw CodexRuntimeLaunchError.runtimeUnavailable
        }
    }

    private func sha256(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 1_048_576), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    private func sandboxProfile(codexHome: URL, runtimeRoot: URL, runtime: URL) throws -> String {
        let userHome = FileManager.default.homeDirectoryForCurrentUser
        let library = userHome.appendingPathComponent("Library", isDirectory: true)
        let applicationSupport = library.appendingPathComponent(
            "Application Support",
            isDirectory: true
        )
        let dayWeave = codexHome.deletingLastPathComponent()
        let runtimeParent = runtimeRoot.deletingLastPathComponent()
        let paths = try [
            userHome,
            library,
            applicationSupport,
            dayWeave,
            codexHome,
            runtimeParent,
            runtimeRoot,
            runtime,
        ]
            .map { try seatbeltLiteral($0.path) }
        let home = paths[0]
        let libraryPath = paths[1]
        let applicationSupportPath = paths[2]
        let dayWeavePath = paths[3]
        let codexHomePath = paths[4]
        let runtimeParentPath = paths[5]
        let runtimeRootPath = paths[6]
        let runtimePath = paths[7]
        return """
        (version 1)
        (deny default)
        (deny dynamic-code-generation)
        (import "system.sb")
        (import "com.apple.corefoundation.sb")
        (allow process-info* (target self))
        (allow process-info-codesignature)
        (allow file-read-metadata
          (literal "/")
          (literal "/Users")
          (literal \(home))
          (literal \(libraryPath))
          (literal \(applicationSupportPath))
          (literal \(dayWeavePath))
          (subpath \(codexHomePath))
          (literal \(runtimeParentPath))
          (subpath \(runtimeRootPath))
          (subpath "/System")
          (subpath "/usr/lib")
          (subpath "/usr/share")
          (subpath "/Library/Apple")
          (subpath "/private/etc/ssl")
          (literal "/private/etc/hosts")
          (literal "/private/etc/resolv.conf")
          (literal "/private/etc/services")
          (literal "/dev/null")
          (literal "/dev/random")
          (literal "/dev/urandom")
          (literal "/dev/zero"))
        (allow file-read*
          (subpath "/System")
          (subpath "/usr/lib")
          (subpath "/usr/share")
          (subpath "/Library/Apple")
          (subpath "/private/etc/ssl")
          (literal "/private/etc/hosts")
          (literal "/private/etc/resolv.conf")
          (literal "/private/etc/services")
          (literal "/dev/null")
          (literal "/dev/random")
          (literal "/dev/urandom")
          (literal "/dev/zero")
          (subpath \(codexHomePath))
          (subpath \(runtimeRootPath)))
        (allow file-write* (subpath \(codexHomePath)))
        (deny file-write* (subpath \(runtimeRootPath)))
        (allow process-exec (literal \(runtimePath)))
        (allow signal (target self))
        (allow system-socket)
        (allow sysctl-read)
        (allow network-outbound)
        """
    }

    private func seatbeltLiteral(_ value: String) throws -> String {
        guard !value.unicodeScalars.contains(where: {
            $0.value == 0 || $0.value == 10 || $0.value == 13
        }) else {
            throw CodexRuntimeLaunchError.unsafeStorage
        }
        let escaped = value
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        return "\"\(escaped)\""
    }

    static func identity(of url: URL, followSymlink: Bool) throws -> CodexFileIdentity {
        var information = stat()
        let result: Int32 = url.withUnsafeFileSystemRepresentation { path in
            guard let path else { return -1 }
            return Darwin.fstatat(
                AT_FDCWD,
                path,
                &information,
                followSymlink ? 0 : AT_SYMLINK_NOFOLLOW
            )
        }
        guard result == 0 else { throw CodexRuntimeLaunchError.runtimeUnavailable }
        let kind: CodexFileKind = switch information.st_mode & S_IFMT {
        case S_IFREG: .regular
        case S_IFDIR: .directory
        default: .other
        }
        return CodexFileIdentity(
            device: UInt64(information.st_dev),
            inode: UInt64(information.st_ino),
            owner: information.st_uid,
            permissions: information.st_mode & 0o777,
            kind: kind
        )
    }
}

private struct SealedResources {
    let runtime: URL
    let manifest: URL
    let legacySchema: URL
    let v2Schema: URL
}

private struct PrivateDirectories {
    let codexHome: URL
    let temporary: URL
    let runtimeParent: URL
}

enum CodexRuntimeLaunchError: Error, Equatable {
    case runtimeUnavailable
    case unsafeStorage
}

enum CodexFileKind: Equatable {
    case regular
    case directory
    case other
}

struct CodexFileIdentity: Equatable {
    let device: UInt64
    let inode: UInt64
    let owner: uid_t
    let permissions: mode_t
    let kind: CodexFileKind
}
