use std::{
    collections::BTreeMap,
    fmt,
    path::Component,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(all(test, unix))]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

#[cfg(test)]
use secrecy::ExposeSecret;
use secrecy::SecretString;

use crate::{Error, Result};

const HARD_MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const HARD_MAX_REQUEST_BYTES: usize = 1024 * 1024;
const HARD_MAX_PROMPT_BYTES: usize = 256 * 1024;
const HARD_MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum EnvironmentKey {
    Lang,
    LcAll,
    TmpDir,
    SslCertFile,
    SslCertDir,
}

impl EnvironmentKey {
    #[cfg(test)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Lang => "LANG",
            Self::LcAll => "LC_ALL",
            Self::TmpDir => "TMPDIR",
            Self::SslCertFile => "SSL_CERT_FILE",
            Self::SslCertDir => "SSL_CERT_DIR",
        }
    }
}

#[derive(Clone, Default)]
pub struct AllowedEnvironment {
    values: BTreeMap<EnvironmentKey, SecretString>,
}

impl AllowedEnvironment {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: EnvironmentKey, value: impl Into<String>) {
        self.values.insert(key, SecretString::from(value.into()));
    }

    #[must_use]
    pub fn with(mut self, key: EnvironmentKey, value: impl Into<String>) -> Self {
        self.set(key, value);
        self
    }

    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_str(), value.expose_secret()))
    }
}

impl fmt::Debug for AllowedEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AllowedEnvironment")
            .field("keys", &self.values.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProtocolLimits {
    pub max_line_bytes: usize,
    pub max_request_bytes: usize,
    pub max_prompt_bytes: usize,
    pub request_timeout: Duration,
    pub turn_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_pending_notifications: usize,
    /// Aggregate memory retained by queued notifications or a pending turn output.
    pub max_queued_bytes: usize,
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: 1024 * 1024,
            max_request_bytes: 512 * 1024,
            max_prompt_bytes: 64 * 1024,
            request_timeout: Duration::from_secs(15),
            turn_timeout: Duration::from_mins(5),
            shutdown_timeout: Duration::from_secs(3),
            max_pending_notifications: 128,
            max_queued_bytes: 256 * 1024,
        }
    }
}

impl ProtocolLimits {
    pub(crate) fn validate(self) -> Result<Self> {
        if self.max_line_bytes == 0 || self.max_line_bytes > HARD_MAX_LINE_BYTES {
            return Err(Error::InvalidConfiguration("invalid line-size limit"));
        }
        if self.max_request_bytes == 0 || self.max_request_bytes > HARD_MAX_REQUEST_BYTES {
            return Err(Error::InvalidConfiguration("invalid request-size limit"));
        }
        if self.max_prompt_bytes == 0 || self.max_prompt_bytes > HARD_MAX_PROMPT_BYTES {
            return Err(Error::InvalidConfiguration("invalid prompt-size limit"));
        }
        if self.request_timeout.is_zero()
            || self.turn_timeout.is_zero()
            || self.shutdown_timeout.is_zero()
        {
            return Err(Error::InvalidConfiguration("timeouts must be non-zero"));
        }
        if self.max_pending_notifications == 0 || self.max_pending_notifications > 1024 {
            return Err(Error::InvalidConfiguration(
                "invalid pending-notification limit",
            ));
        }
        if self.max_queued_bytes == 0 || self.max_queued_bytes > HARD_MAX_QUEUED_BYTES {
            return Err(Error::InvalidConfiguration("invalid queued-byte limit"));
        }
        Ok(self)
    }
}

#[derive(Clone)]
pub struct CodexAppServerConfig {
    executable: PathBuf,
    codex_home: PathBuf,
    workspace_roots: Vec<PathBuf>,
    environment: AllowedEnvironment,
    limits: ProtocolLimits,
}

impl CodexAppServerConfig {
    #[must_use]
    pub fn new(
        executable: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        workspace_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        Self {
            executable: executable.into(),
            codex_home: codex_home.into(),
            workspace_roots: workspace_roots.into_iter().collect(),
            environment: AllowedEnvironment::new(),
            limits: ProtocolLimits::default(),
        }
    }

    #[must_use]
    pub fn with_environment(mut self, environment: AllowedEnvironment) -> Self {
        self.environment = environment;
        self
    }

    #[must_use]
    pub fn with_limits(mut self, limits: ProtocolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) fn validate_scaffold(&self) -> Result<()> {
        let PreparedPaths {
            executable,
            codex_home,
            workspace_roots,
        } = self.prepare_paths()?;
        drop((executable, codex_home, workspace_roots));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn prepare_test_runtime(&self) -> Result<PreparedConfig> {
        let paths = self.prepare_paths()?;
        let codex_home = OwnedCodexHome::create(paths.codex_home)?;
        Ok(PreparedConfig {
            executable: paths.executable,
            codex_home,
            workspace_roots: paths.workspace_roots,
            environment: self.environment.clone(),
            limits: self.limits.validate()?,
        })
    }

    fn prepare_paths(&self) -> Result<PreparedPaths> {
        if !self.executable.is_absolute() || !self.codex_home.is_absolute() {
            return Err(Error::InvalidConfiguration("paths must be absolute"));
        }
        let executable = canonical_file(&self.executable, "executable is not a file")?;
        let codex_home = canonical_new_directory_path(&self.codex_home)?;
        let workspace_roots = canonical_workspace_roots(&self.workspace_roots)?;
        for root in &workspace_roots {
            if paths_overlap(root, &codex_home) {
                return Err(Error::InvalidConfiguration(
                    "workspace roots and CODEX_HOME must not overlap",
                ));
            }
        }
        self.limits.validate()?;
        Ok(PreparedPaths {
            executable,
            codex_home,
            workspace_roots,
        })
    }
}

impl fmt::Debug for CodexAppServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexAppServerConfig")
            .field("executable", &"<absolute path>")
            .field("codex_home", &"<isolated path>")
            .field("workspace_roots", &"<canonical allowlist>")
            .field("environment", &self.environment)
            .field("limits", &self.limits)
            .finish()
    }
}

#[cfg(test)]
pub(crate) struct PreparedConfig {
    pub(crate) executable: PathBuf,
    pub(crate) codex_home: OwnedCodexHome,
    pub(crate) workspace_roots: Vec<PathBuf>,
    pub(crate) environment: AllowedEnvironment,
    pub(crate) limits: ProtocolLimits,
}

struct PreparedPaths {
    executable: PathBuf,
    codex_home: PathBuf,
    workspace_roots: Vec<PathBuf>,
}

#[cfg(test)]
pub(crate) struct OwnedCodexHome {
    path: PathBuf,
}

#[cfg(test)]
impl OwnedCodexHome {
    fn create(path: PathBuf) -> Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(&path)
            .map_err(|_| Error::InvalidConfiguration("CODEX_HOME must be newly created"))?;

        let valid = (|| {
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
                Error::InvalidConfiguration("CODEX_HOME metadata could not be verified")
            })?;
            if !metadata.file_type().is_dir() {
                return Err(Error::InvalidConfiguration("CODEX_HOME is not a directory"));
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(Error::InvalidConfiguration(
                    "CODEX_HOME permissions must be 0700",
                ));
            }
            if std::fs::read_dir(&path)
                .map_err(|_| Error::InvalidConfiguration("CODEX_HOME cannot be inspected"))?
                .next()
                .is_some()
            {
                return Err(Error::InvalidConfiguration("CODEX_HOME must be empty"));
            }
            Ok(())
        })();
        if let Err(error) = valid {
            let _ = std::fs::remove_dir(&path);
            return Err(error);
        }
        Ok(Self { path })
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
impl Drop for OwnedCodexHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn canonical_file(path: &Path, message: &'static str) -> Result<PathBuf> {
    let canonical =
        std::fs::canonicalize(path).map_err(|_| Error::InvalidConfiguration(message))?;
    if !canonical.is_file() {
        return Err(Error::InvalidConfiguration(message));
    }
    Ok(canonical)
}

pub(crate) fn canonical_directory(path: &Path, message: &'static str) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::InvalidConfiguration("paths must be absolute"));
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| Error::InvalidConfiguration(message))?;
    if !canonical.is_dir() {
        return Err(Error::InvalidConfiguration(message));
    }
    if canonical.to_str().is_none() {
        return Err(Error::InvalidConfiguration("path is not valid UTF-8"));
    }
    Ok(canonical)
}

fn canonical_new_directory_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::InvalidConfiguration("paths must be absolute"));
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(Error::InvalidConfiguration(
                "CODEX_HOME must not already exist",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(Error::InvalidConfiguration(
                "CODEX_HOME existence could not be verified",
            ));
        }
    }
    if path.to_str().is_none() {
        return Err(Error::InvalidConfiguration("path is not valid UTF-8"));
    }
    let Some(parent) = path.parent() else {
        return Err(Error::InvalidConfiguration("CODEX_HOME has no parent"));
    };
    let Some(name) = path.file_name() else {
        return Err(Error::InvalidConfiguration("CODEX_HOME has no name"));
    };
    if !matches!(
        Path::new(name).components().next(),
        Some(Component::Normal(_))
    ) {
        return Err(Error::InvalidConfiguration("CODEX_HOME name is invalid"));
    }
    let canonical_parent = canonical_directory(parent, "CODEX_HOME parent is not a directory")?;
    Ok(canonical_parent.join(name))
}

fn canonical_workspace_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if roots.is_empty() {
        return Err(Error::InvalidConfiguration(
            "workspace allowlist must not be empty",
        ));
    }
    let mut canonical: Vec<PathBuf> = Vec::with_capacity(roots.len());
    for root in roots {
        let root = canonical_directory(root, "workspace root is not a directory")?;
        if canonical
            .iter()
            .any(|existing| paths_overlap(existing, &root))
        {
            return Err(Error::InvalidConfiguration(
                "workspace roots must not overlap",
            ));
        }
        canonical.push(root);
    }
    Ok(canonical)
}

fn paths_overlap(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}
