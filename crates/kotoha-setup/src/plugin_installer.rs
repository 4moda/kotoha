//! Cross-platform Mumble plugin directory discovery and atomic SHA-256-aware installation.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::error::SetupError;

/// Target operating system kind for deterministic plugin path discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPlatform {
    /// Microsoft Windows (`kotoha_mumble.dll`).
    Windows,
    /// Apple macOS (`libkotoha_mumble.dylib`).
    MacOs,
    /// Linux (`libkotoha_mumble.so`), including native XDG and Flatpak Mumble locations.
    Linux,
}

impl TargetPlatform {
    /// Returns the compile-time target platform, or `None` on unsupported OSes.
    #[must_use]
    pub const fn current() -> Option<Self> {
        if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Self::MacOs)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else {
            None
        }
    }

    /// Resolves the compile-time target platform or returns [`SetupError::UnsupportedPlatform`].
    pub fn current_checked() -> Result<Self, SetupError> {
        Self::current().ok_or_else(|| {
            SetupError::UnsupportedPlatform(format!(
                "unsupported OS '{}' for Mumble plugin installation",
                std::env::consts::OS
            ))
        })
    }
}

/// Classification of a discovered candidate Mumble plugin directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MumblePluginDirKind {
    /// Windows `%AppData%\Mumble\Mumble\Plugins` (standard installer location).
    WindowsAppData,
    /// Windows `%LocalAppData%\Mumble\Mumble\Plugins` (per-user fallback location).
    WindowsLocalAppData,
    /// macOS `~/Library/Application Support/Mumble/Mumble/Plugins`.
    MacOsApplicationSupport,
    /// Linux `$XDG_DATA_HOME/Mumble/Mumble/Plugins` or `~/.local/share/Mumble/Mumble/Plugins`.
    LinuxXdgData,
    /// Linux Flatpak `~/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins`.
    LinuxFlatpak,
    /// Explicitly configured custom directory (`custom_mumble_plugin_dir`).
    Custom,
}

/// Discovered Mumble plugin directory candidate together with existence hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MumblePluginDirCandidate {
    /// Candidate plugin directory path (`.../Mumble/Mumble/Plugins` or custom path).
    pub path: PathBuf,
    /// Origin / installation flavor of this candidate.
    pub kind: MumblePluginDirKind,
    /// Whether the directory (or its parent Mumble data directory) currently exists on disk.
    pub detected_on_disk: bool,
}

/// Outcome of [`install_plugin`] or [`install_plugin_for_os`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginInstallOutcome {
    /// Plugin binary was written (or updated) atomically at `path`.
    Installed {
        /// Installed plugin file path.
        path: PathBuf,
        /// Lowercase hex SHA-256 digest of the installed bytes.
        sha256: String,
        /// `true` if an older file existed and was replaced; `false` for fresh install.
        replaced_existing: bool,
    },
    /// Existing target file already had the exact same SHA-256 digest; disk write skipped.
    AlreadyUpToDate {
        /// Existing plugin file path.
        path: PathBuf,
        /// Lowercase hex SHA-256 digest matching `plugin_bytes`.
        sha256: String,
    },
    /// Existing target file is locked by a running host application (e.g., Mumble).
    LockedByRunningApp {
        /// Locked plugin file path.
        path: PathBuf,
        /// Clear Japanese/English guidance prompting the user to close Mumble and retry.
        guidance: String,
    },
}

impl PluginInstallOutcome {
    /// Returns the target plugin file path regardless of outcome variant.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Installed { path, .. }
            | Self::AlreadyUpToDate { path, .. }
            | Self::LockedByRunningApp { path, .. } => path,
        }
    }
}

/// Returns the expected shared library filename for `os`.
#[must_use]
pub const fn plugin_filename_for(os: TargetPlatform) -> &'static str {
    match os {
        TargetPlatform::Windows => "kotoha_mumble.dll",
        TargetPlatform::MacOs => "libkotoha_mumble.dylib",
        TargetPlatform::Linux => "libkotoha_mumble.so",
    }
}

/// Returns the expected shared library filename for the current host OS.
pub fn plugin_filename() -> Result<&'static str, SetupError> {
    TargetPlatform::current_checked().map(plugin_filename_for)
}

/// Discovers candidate Mumble plugin directories for `os` using explicit environment values (`env`),
/// appending an optional custom directory at the end (or deduplicating if identical).
///
/// Recognized `env` keys:
/// - Windows: `APPDATA`, `LOCALAPPDATA` (case-insensitive fallback check supported via helper)
/// - macOS: `HOME`
/// - Linux: `XDG_DATA_HOME`, `HOME`
#[must_use]
pub fn discover_mumble_plugin_dirs_for(
    os: TargetPlatform,
    env: &BTreeMap<String, String>,
) -> Vec<MumblePluginDirCandidate> {
    discover_mumble_plugin_dirs_with_custom(os, env, None)
}

/// Discovers candidate Mumble plugin directories for `os`, including an optional manual
/// `custom_dir` override/addition.
#[must_use]
pub fn discover_mumble_plugin_dirs_with_custom(
    os: TargetPlatform,
    env: &BTreeMap<String, String>,
    custom_dir: Option<&Path>,
) -> Vec<MumblePluginDirCandidate> {
    let mut out = Vec::new();

    match os {
        TargetPlatform::Windows => {
            if let Some(app_data) = lookup_env(env, &["APPDATA", "AppData"]) {
                let path = join_windows_mumble_plugins(app_data);
                push_unique_candidate(&mut out, path, MumblePluginDirKind::WindowsAppData);
            }
            if let Some(local_app_data) = lookup_env(env, &["LOCALAPPDATA", "LocalAppData"]) {
                let path = join_windows_mumble_plugins(local_app_data);
                push_unique_candidate(&mut out, path, MumblePluginDirKind::WindowsLocalAppData);
            }
        }
        TargetPlatform::MacOs => {
            if let Some(home) = lookup_env(env, &["HOME"]) {
                let path = join_unix_segments(
                    home,
                    &[
                        "Library",
                        "Application Support",
                        "Mumble",
                        "Mumble",
                        "Plugins",
                    ],
                );
                push_unique_candidate(&mut out, path, MumblePluginDirKind::MacOsApplicationSupport);
            }
        }
        TargetPlatform::Linux => {
            let xdg_base = lookup_env(env, &["XDG_DATA_HOME"]).map(str::to_owned);
            let home = lookup_env(env, &["HOME"]);

            if let Some(xdg) = xdg_base {
                let path = join_unix_segments(&xdg, &["Mumble", "Mumble", "Plugins"]);
                push_unique_candidate(&mut out, path, MumblePluginDirKind::LinuxXdgData);
            } else if let Some(home_dir) = home {
                let path = join_unix_segments(
                    home_dir,
                    &[".local", "share", "Mumble", "Mumble", "Plugins"],
                );
                push_unique_candidate(&mut out, path, MumblePluginDirKind::LinuxXdgData);
            }

            if let Some(home_dir) = home {
                let flatpak = join_unix_segments(
                    home_dir,
                    &[
                        ".var",
                        "app",
                        "info.mumble.Mumble",
                        "data",
                        "Mumble",
                        "Mumble",
                        "Plugins",
                    ],
                );
                push_unique_candidate(&mut out, flatpak, MumblePluginDirKind::LinuxFlatpak);
            }
        }
    }

    if let Some(custom) = custom_dir {
        let raw = custom.to_string_lossy();
        if !raw.trim().is_empty() {
            push_unique_candidate(&mut out, custom.to_path_buf(), MumblePluginDirKind::Custom);
        }
    }

    out
}

/// Discovers candidate Mumble plugin directories for the current OS using [`std::env::vars`].
pub fn discover_mumble_plugin_dirs(
    custom_dir: Option<&Path>,
) -> Result<Vec<MumblePluginDirCandidate>, SetupError> {
    let os = TargetPlatform::current_checked()?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    Ok(discover_mumble_plugin_dirs_with_custom(
        os, &env, custom_dir,
    ))
}

/// Installs `plugin_bytes` as the current OS's Mumble plugin filename inside `target_dir`.
///
/// - Computes SHA-256 of `plugin_bytes`; if target exists and matches, skips writing and returns
///   [`PluginInstallOutcome::AlreadyUpToDate`].
/// - Writes atomically via a unique sibling `.tmp` file + rename.
/// - Converts locked-file / sharing-violation / permission-denied errors on an existing target
///   into [`PluginInstallOutcome::LockedByRunningApp`] with bilingual (Japanese/English) guidance.
pub fn install_plugin(
    target_dir: &Path,
    plugin_bytes: &[u8],
) -> Result<PluginInstallOutcome, SetupError> {
    let os = TargetPlatform::current_checked()?;
    install_plugin_for_os(target_dir, plugin_bytes, os)
}

/// Installs `plugin_bytes` inside `target_dir` using the filename for `os`.
pub fn install_plugin_for_os(
    target_dir: &Path,
    plugin_bytes: &[u8],
    os: TargetPlatform,
) -> Result<PluginInstallOutcome, SetupError> {
    if target_dir.as_os_str().is_empty() {
        return Err(SetupError::InvalidConfig(
            "plugin target_dir must not be empty".to_owned(),
        ));
    }
    if plugin_bytes.is_empty() {
        return Err(SetupError::InvalidConfig(
            "plugin_bytes must not be empty".to_owned(),
        ));
    }

    let filename = plugin_filename_for(os);
    let target_path = target_dir.join(filename);
    let desired_sha256 = sha256_hex(plugin_bytes);

    let target_existed = match fs::metadata(&target_path) {
        Ok(meta) => {
            if !meta.is_file() {
                return Err(SetupError::InvalidConfig(format!(
                    "plugin target path '{}' exists but is not a regular file",
                    target_path.display()
                )));
            }
            match compute_file_sha256_hex(&target_path) {
                Ok(existing_sha256) if existing_sha256 == desired_sha256 => {
                    return Ok(PluginInstallOutcome::AlreadyUpToDate {
                        path: target_path,
                        sha256: desired_sha256,
                    });
                }
                Ok(_) => true,
                Err(err) if is_locked_file_io_error(&err) => {
                    return Ok(PluginInstallOutcome::LockedByRunningApp {
                        guidance: locked_plugin_guidance(&target_path),
                        path: target_path,
                    });
                }
                Err(err) => return Err(SetupError::Io(err)),
            }
        }
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) if is_locked_file_io_error(&err) => {
            return Ok(PluginInstallOutcome::LockedByRunningApp {
                guidance: locked_plugin_guidance(&target_path),
                path: target_path,
            });
        }
        Err(err) => return Err(SetupError::Io(err)),
    };

    fs::create_dir_all(target_dir)?;

    let unique_id = next_temp_id();
    let tmp_path = target_dir.join(format!(
        ".{filename}.{}.{unique_id}.tmp",
        std::process::id()
    ));
    let mut tmp_guard = TempFileCleanupGuard::new(tmp_path.clone());

    {
        let mut tmp_file = File::create(&tmp_path)?;
        tmp_file.write_all(plugin_bytes)?;
        tmp_file.sync_all()?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&tmp_path, perms)?;
    }

    if let Err(err) = fs::rename(&tmp_path, &target_path) {
        if target_existed && is_locked_file_io_error(&err) {
            return Ok(PluginInstallOutcome::LockedByRunningApp {
                guidance: locked_plugin_guidance(&target_path),
                path: target_path,
            });
        }
        return Err(SetupError::Io(err));
    }

    tmp_guard.disarm();

    Ok(PluginInstallOutcome::Installed {
        path: target_path,
        sha256: desired_sha256,
        replaced_existing: target_existed,
    })
}

/// Returns `true` when an [`io::Error`] indicates a locked or permission-protected file.
///
/// Recognizes Windows sharing violations (`ERROR_SHARING_VIOLATION = 32`,
/// `ERROR_ACCESS_DENIED = 5`, lock violation `33`) as well as Unix `PermissionDenied` and
/// `ETXTBSY` (`26`).
#[must_use]
pub fn is_locked_file_io_error(err: &io::Error) -> bool {
    if err.kind() == ErrorKind::PermissionDenied {
        return true;
    }
    matches!(err.raw_os_error(), Some(5 | 26 | 32 | 33))
}

/// Builds a clear bilingual (Japanese & English) recovery message for locked plugin files.
#[must_use]
pub fn locked_plugin_guidance(path: &Path) -> String {
    format!(
        "プラグインファイルが Mumble によって使用中のため更新できませんでした ({})。\
         Mumble を完全に終了してから再試行してください。\n\
         The plugin file is currently locked by a running Mumble process ({}). \
         Please close Mumble completely and retry.",
        path.display(),
        path.display()
    )
}

fn lookup_env<'a>(env: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(val) = env.get(*key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    for (k, v) in env {
        if keys
            .iter()
            .any(|candidate| k.eq_ignore_ascii_case(candidate))
        {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn join_windows_mumble_plugins(base: &str) -> PathBuf {
    let trimmed = base.trim_end_matches(['\\', '/']);
    PathBuf::from(format!(r"{trimmed}\Mumble\Mumble\Plugins"))
}

fn join_unix_segments(base: &str, segments: &[&str]) -> PathBuf {
    let trimmed = base.trim_end_matches('/');
    let mut path = if trimmed.is_empty() {
        String::from("/")
    } else {
        trimmed.to_owned()
    };
    for seg in segments {
        if !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(seg);
    }
    PathBuf::from(path)
}

fn push_unique_candidate(
    out: &mut Vec<MumblePluginDirCandidate>,
    path: PathBuf,
    kind: MumblePluginDirKind,
) {
    if out.iter().any(|existing| existing.path == path) {
        return;
    }
    let detected_on_disk = path.is_dir() || path.parent().is_some_and(Path::is_dir);
    out.push(MumblePluginDirCandidate {
        path,
        kind,
        detected_on_disk,
    });
}

fn compute_file_sha256_hex(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(bytes_to_lower_hex(&hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    bytes_to_lower_hex(&hasher.finalize())
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn next_temp_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

struct TempFileCleanupGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileCleanupGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
