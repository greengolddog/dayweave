#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use dayweave_codex::{CodexAppServer, CodexAppServerConfig, Error};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dayweave-codex-disabled-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated test directory");
        Self(fs::canonicalize(path).expect("canonical test directory"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated test directory");
    }
}

fn inert_candidate(base: &Path) -> PathBuf {
    let executable = base.join("candidate.sh");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf executed > \"$0.executed\"\n",
    )
    .expect("write candidate");
    let mut permissions = fs::metadata(&executable)
        .expect("candidate metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).expect("candidate permissions");
    executable
}

fn config(base: &TempDirectory) -> CodexAppServerConfig {
    let project = base.path().join("project");
    fs::create_dir(&project).expect("project directory");
    CodexAppServerConfig::new(
        inert_candidate(base.path()),
        base.path().join("codex-home"),
        [project],
    )
}

#[tokio::test]
async fn valid_configuration_returns_no_supported_runtime_without_execution() {
    let base = TempDirectory::new("no-runtime");
    let candidate_marker = base.path().join("candidate.sh.executed");
    let result = CodexAppServer::spawn(config(&base)).await;
    assert!(matches!(result, Err(Error::NoSupportedRuntime)));
    assert!(!candidate_marker.exists(), "candidate must never execute");
    assert!(
        !base.path().join("codex-home").exists(),
        "disabled production startup must not create credential storage"
    );
}

#[tokio::test]
async fn preexisting_codex_home_is_rejected_even_when_empty() {
    let base = TempDirectory::new("preexisting-home");
    fs::create_dir(base.path().join("codex-home")).expect("preexisting home");
    assert!(matches!(
        CodexAppServer::spawn(config(&base)).await,
        Err(Error::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn preexisting_nonempty_codex_home_is_rejected() {
    let base = TempDirectory::new("nonempty-home");
    let home = base.path().join("codex-home");
    fs::create_dir(&home).expect("preexisting home");
    fs::write(home.join("foreign-config.toml"), b"untrusted = true").expect("foreign home content");
    assert!(matches!(
        CodexAppServer::spawn(config(&base)).await,
        Err(Error::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn dangling_symlink_codex_home_is_rejected_as_preexisting() {
    let base = TempDirectory::new("symlink-home");
    symlink(
        base.path().join("missing-target"),
        base.path().join("codex-home"),
    )
    .expect("dangling home symlink");
    assert!(matches!(
        CodexAppServer::spawn(config(&base)).await,
        Err(Error::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn workspace_and_codex_home_overlap_is_rejected() {
    let base = TempDirectory::new("overlap");
    let project = base.path().join("project");
    fs::create_dir(&project).expect("project directory");
    let config = CodexAppServerConfig::new(
        inert_candidate(base.path()),
        project.join("codex-home"),
        [project],
    );
    assert!(matches!(
        CodexAppServer::spawn(config).await,
        Err(Error::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn empty_workspace_allowlist_is_rejected() {
    let base = TempDirectory::new("empty-allowlist");
    let config = CodexAppServerConfig::new(
        inert_candidate(base.path()),
        base.path().join("codex-home"),
        [],
    );
    assert!(matches!(
        CodexAppServer::spawn(config).await,
        Err(Error::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn overlapping_workspace_roots_are_rejected() {
    let base = TempDirectory::new("overlapping-roots");
    let project = base.path().join("project");
    let nested = project.join("nested");
    fs::create_dir_all(&nested).expect("nested project directory");
    let config = CodexAppServerConfig::new(
        inert_candidate(base.path()),
        base.path().join("codex-home"),
        [project, nested],
    );
    assert!(matches!(
        CodexAppServer::spawn(config).await,
        Err(Error::InvalidConfiguration(_))
    ));
}
