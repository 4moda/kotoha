//! TOML-backed [`GlossaryRepository`] adapter reading `glossaries/*.toml` profiles.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::{CoreError, GlossaryProfile, GlossaryRepository, LanguageCode};

/// Maximum permitted size of a single `.toml` glossary file (1 MiB).
pub const MAX_GLOSSARY_FILE_BYTES: u64 = 1024 * 1024;

/// Serde representation matching `DESIGN.md` Section 4.2 TOML glossary schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTomlGlossary {
    name: String,
    src_lang: String,
    tgt_lang: String,
    #[serde(default)]
    terms: BTreeMap<String, String>,
}

/// [`GlossaryRepository`] adapter reading and validating `.toml` glossary profiles from disk.
#[derive(Debug, Default, Clone, Copy)]
pub struct TomlGlossaryRepository;

impl TomlGlossaryRepository {
    /// Creates a new [`TomlGlossaryRepository`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Parses and validates a [`GlossaryProfile`] directly from TOML text.
    ///
    /// Validation rules:
    /// - `name` must be non-empty after trimming and free of control characters.
    /// - `src_lang` and `tgt_lang` must be valid [`LanguageCode`] values.
    /// - Every term key and value must be non-empty and free of control characters
    ///   (internal spaces such as `"M W D"` are preserved verbatim).
    pub fn parse_str(contents: &str) -> Result<GlossaryProfile, CoreError> {
        if contents.len() as u64 > MAX_GLOSSARY_FILE_BYTES {
            return Err(CoreError::Glossary(format!(
                "glossary payload exceeds maximum size of {MAX_GLOSSARY_FILE_BYTES} bytes"
            )));
        }

        let raw: RawTomlGlossary = toml::from_str(contents)
            .map_err(|err| CoreError::Glossary(format!("invalid TOML glossary syntax: {err}")))?;

        let name = raw.name.trim();
        if name.is_empty() {
            return Err(CoreError::Glossary(
                "glossary profile 'name' must not be empty".to_owned(),
            ));
        }
        if name.chars().any(char::is_control) {
            return Err(CoreError::Glossary(
                "glossary profile 'name' must not contain control characters".to_owned(),
            ));
        }

        let src_lang = LanguageCode::new(raw.src_lang)?;
        let tgt_lang = LanguageCode::new(raw.tgt_lang)?;

        let mut terms = BTreeMap::new();
        for (key, value) in raw.terms {
            if key.trim().is_empty() {
                return Err(CoreError::Glossary(
                    "glossary term key must not be empty or whitespace-only".to_owned(),
                ));
            }
            if key.chars().any(char::is_control) {
                return Err(CoreError::Glossary(format!(
                    "glossary term key '{key}' must not contain control characters"
                )));
            }
            if value.trim().is_empty() {
                return Err(CoreError::Glossary(format!(
                    "glossary translation value for key '{key}' must not be empty or whitespace-only"
                )));
            }
            if value.chars().any(char::is_control) {
                return Err(CoreError::Glossary(format!(
                    "glossary translation value for key '{key}' must not contain control characters"
                )));
            }
            // Preserve internal spaces (e.g. "M W D") and exact casing verbatim.
            terms.insert(key, value);
        }

        Ok(GlossaryProfile::new(
            name.to_owned(),
            src_lang,
            tgt_lang,
            terms,
        ))
    }
}

impl GlossaryRepository for TomlGlossaryRepository {
    fn load_file(&self, path: &Path) -> Result<GlossaryProfile, CoreError> {
        let meta = fs::metadata(path).map_err(|err| {
            CoreError::Glossary(format!(
                "failed to stat glossary file '{}': {err}",
                path.display()
            ))
        })?;
        if !meta.is_file() {
            return Err(CoreError::Glossary(format!(
                "glossary path '{}' is not a regular file",
                path.display()
            )));
        }
        if meta.len() > MAX_GLOSSARY_FILE_BYTES {
            return Err(CoreError::Glossary(format!(
                "glossary file '{}' ({} bytes) exceeds maximum size of {MAX_GLOSSARY_FILE_BYTES} bytes",
                path.display(),
                meta.len()
            )));
        }

        let contents = fs::read_to_string(path).map_err(|err| {
            CoreError::Glossary(format!(
                "failed to read glossary file '{}': {err}",
                path.display()
            ))
        })?;
        Self::parse_str(&contents).map_err(|err| match err {
            CoreError::InvalidLanguageCode(code) => CoreError::Glossary(format!(
                "invalid language code '{code}' in glossary file '{}'",
                path.display()
            )),
            CoreError::Glossary(msg) => CoreError::Glossary(format!(
                "failed to load glossary file '{}': {msg}",
                path.display()
            )),
            other => other,
        })
    }

    fn load_dir(&self, dir: &Path) -> Result<Vec<GlossaryProfile>, CoreError> {
        if !dir.is_dir() {
            return Err(CoreError::Glossary(format!(
                "glossary directory does not exist or is not a directory: '{}'",
                dir.display()
            )));
        }
        let read_dir = fs::read_dir(dir).map_err(|err| {
            CoreError::Glossary(format!(
                "failed to read glossary directory '{}': {err}",
                dir.display()
            ))
        })?;

        let mut toml_paths: Vec<(String, PathBuf)> = Vec::new();
        for entry_res in read_dir {
            let entry = entry_res.map_err(|err| {
                CoreError::Glossary(format!(
                    "failed to read entry in glossary directory '{}': {err}",
                    dir.display()
                ))
            })?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let is_toml = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
            if !is_toml {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            toml_paths.push((file_name, path));
        }

        toml_paths.sort_by(|(a_name, a_path), (b_name, b_path)| {
            a_name.cmp(b_name).then_with(|| a_path.cmp(b_path))
        });

        let mut profiles = Vec::with_capacity(toml_paths.len());
        for (_, path) in toml_paths {
            profiles.push(self.load_file(&path)?);
        }
        Ok(profiles)
    }
}
