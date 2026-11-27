//! Application use case managing custom glossary profiles, priority merging, and hot-reload.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::PipelineService;
use crate::domain::{
    CoreError, GlossaryProfile, GlossaryRepository, GlossarySettings, LanguageCode, MergedGlossary,
    SttEngine, SttHotwordsConfig, DEFAULT_HOTWORDS_SCORE,
};

/// Loads, selects, priority-merges, and hot-reloads custom glossary profiles across STT & translation.
pub struct GlossaryManager {
    src_lang: LanguageCode,
    tgt_lang: LanguageCode,
    profiles: Vec<GlossaryProfile>,
    enabled_profiles: Vec<String>,
    explicit_selection: bool,
    hotwords_score: f32,
    active_merged: MergedGlossary,
}

impl GlossaryManager {
    /// Creates an empty [`GlossaryManager`] targeting `(src_lang, tgt_lang)` with
    /// [`DEFAULT_HOTWORDS_SCORE`].
    #[must_use]
    pub fn new(src_lang: LanguageCode, tgt_lang: LanguageCode) -> Self {
        Self {
            src_lang,
            tgt_lang,
            profiles: Vec::new(),
            enabled_profiles: Vec::new(),
            explicit_selection: false,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            active_merged: MergedGlossary::default(),
        }
    }

    /// Creates a [`GlossaryManager`] pre-populated with in-memory `profiles`.
    ///
    /// Rejects duplicate profile names (with the same `(src_lang, tgt_lang)` pair or name collision)
    /// via [`Self::from_profiles_checked`]; by default all loaded profiles are enabled in slice order
    /// unless overridden via [`Self::set_enabled_profiles`] or [`Self::apply_settings`].
    pub fn from_profiles_checked(
        src_lang: LanguageCode,
        tgt_lang: LanguageCode,
        profiles: Vec<GlossaryProfile>,
    ) -> Result<Self, CoreError> {
        ensure_unique_profile_names(&profiles)?;
        let enabled_profiles = default_enabled_names(&profiles);
        let mut mgr = Self {
            src_lang,
            tgt_lang,
            profiles,
            enabled_profiles,
            explicit_selection: false,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            active_merged: MergedGlossary::default(),
        };
        mgr.recompute_active();
        Ok(mgr)
    }

    /// Creates a [`GlossaryManager`] pre-populated with in-memory `profiles` (ignoring duplicate
    /// checks when infallible construction is required; prefer [`Self::from_profiles_checked`]).
    #[must_use]
    pub fn from_profiles(
        src_lang: LanguageCode,
        tgt_lang: LanguageCode,
        profiles: Vec<GlossaryProfile>,
    ) -> Self {
        let enabled_profiles = default_enabled_names(&profiles);
        let mut mgr = Self {
            src_lang,
            tgt_lang,
            profiles,
            enabled_profiles,
            explicit_selection: false,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            active_merged: MergedGlossary::default(),
        };
        mgr.recompute_active();
        mgr
    }

    /// Loads all profiles from `dir` using `repo` (defaulting `enabled_profiles` to all loaded
    /// profiles in deterministic sorted filename order).
    pub fn from_dir(
        repo: &dyn GlossaryRepository,
        dir: &Path,
        src_lang: LanguageCode,
        tgt_lang: LanguageCode,
    ) -> Result<Self, CoreError> {
        let mut mgr = Self::new(src_lang, tgt_lang);
        mgr.load_from_dir(repo, dir)?;
        Ok(mgr)
    }

    /// Loads all profiles from `dir` using `repo` and applies explicit [`GlossarySettings`].
    pub fn from_settings(
        repo: &dyn GlossaryRepository,
        default_dir: &Path,
        settings: &GlossarySettings,
        src_lang: LanguageCode,
        tgt_lang: LanguageCode,
    ) -> Result<Self, CoreError> {
        let target_dir = settings
            .glossary_dir
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map_or(default_dir, Path::new);
        let profiles = repo.load_dir(target_dir)?;
        ensure_unique_profile_names(&profiles)?;
        let mut mgr = Self {
            src_lang,
            tgt_lang,
            profiles,
            enabled_profiles: settings.enabled_profiles.clone(),
            explicit_selection: true,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            active_merged: MergedGlossary::default(),
        };
        mgr.recompute_active();
        Ok(mgr)
    }

    /// Reloads all profiles from `dir` via `repo`.
    ///
    /// Rejects duplicate profile names cleanly before mutating state.
    /// If an explicit `enabled_profiles` selection was configured (including intentionally empty
    /// `vec![]` disabling all profiles), it is preserved across reloads; otherwise all newly loaded
    /// profiles become enabled in deterministic sorted filename order.
    pub fn load_from_dir(
        &mut self,
        repo: &dyn GlossaryRepository,
        dir: &Path,
    ) -> Result<(), CoreError> {
        let profiles = repo.load_dir(dir)?;
        ensure_unique_profile_names(&profiles)?;
        if !self.explicit_selection {
            self.enabled_profiles = default_enabled_names(&profiles);
        }
        self.profiles = profiles;
        self.recompute_active();
        Ok(())
    }

    /// Loads a single profile file at `path` via `repo` and inserts/replaces it by `profile.name`.
    pub fn load_file(
        &mut self,
        repo: &dyn GlossaryRepository,
        path: &Path,
    ) -> Result<GlossaryProfile, CoreError> {
        let profile = repo.load_file(path)?;
        self.upsert_profile(profile.clone());
        Ok(profile)
    }

    /// Replaces the in-memory profile list (rejecting duplicate names) and recomputes
    /// [`Self::active_merged`].
    pub fn set_profiles(&mut self, profiles: Vec<GlossaryProfile>) -> Result<(), CoreError> {
        ensure_unique_profile_names(&profiles)?;
        if !self.explicit_selection {
            self.enabled_profiles = default_enabled_names(&profiles);
        }
        self.profiles = profiles;
        self.recompute_active();
        Ok(())
    }

    /// Inserts or replaces an in-memory [`GlossaryProfile`] (matching by `name` + language pair).
    ///
    /// If no explicit selection is active and `profile.name` is not already present in
    /// `enabled_profiles`, appends it with highest priority and recomputes [`Self::active_merged`].
    pub fn upsert_profile(&mut self, profile: GlossaryProfile) {
        if let Some(existing) = self.profiles.iter_mut().find(|p| {
            p.name == profile.name
                && p.src_lang == profile.src_lang
                && p.tgt_lang == profile.tgt_lang
        }) {
            *existing = profile;
        } else {
            let name = profile.name.clone();
            self.profiles.push(profile);
            if !self.explicit_selection && !self.enabled_profiles.iter().any(|n| n == &name) {
                self.enabled_profiles.push(name);
            }
        }
        self.recompute_active();
    }

    /// Sets the enabled profile names in priority order (later entries override earlier ones) and
    /// recomputes [`Self::active_merged`]. Passing `vec![]` explicitly disables all profiles.
    pub fn set_enabled_profiles(&mut self, enabled_profiles: Vec<String>) {
        self.enabled_profiles = enabled_profiles;
        self.explicit_selection = true;
        self.recompute_active();
    }

    /// Clears explicit profile selection so subsequent reloads auto-enable all loaded profiles.
    pub fn clear_explicit_selection(&mut self) {
        self.explicit_selection = false;
        self.enabled_profiles = default_enabled_names(&self.profiles);
        self.recompute_active();
    }

    /// Applies `settings.enabled_profiles` (and reloads from `settings.glossary_dir` or falls back
    /// to `default_dir` when `settings.glossary_dir` is `None` / blank) and recomputes
    /// [`Self::active_merged`].
    pub fn apply_settings(
        &mut self,
        settings: &GlossarySettings,
        repo: Option<&dyn GlossaryRepository>,
        default_dir: Option<&Path>,
    ) -> Result<(), CoreError> {
        let mut next_profiles = None;
        if let Some(repository) = repo {
            let custom_dir = settings
                .glossary_dir
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(Path::new);
            if let Some(target_dir) = custom_dir.or(default_dir) {
                let loaded = repository.load_dir(target_dir)?;
                ensure_unique_profile_names(&loaded)?;
                next_profiles = Some(loaded);
            }
        }

        if let Some(loaded) = next_profiles {
            self.profiles = loaded;
        }
        self.enabled_profiles.clone_from(&settings.enabled_profiles);
        self.explicit_selection = true;
        self.recompute_active();
        Ok(())
    }

    /// Updates the target `(src_lang, tgt_lang)` language pair and recomputes [`Self::active_merged`].
    pub fn set_language_pair(&mut self, src_lang: LanguageCode, tgt_lang: LanguageCode) {
        self.src_lang = src_lang;
        self.tgt_lang = tgt_lang;
        self.recompute_active();
    }

    /// Updates the STT hotword boosting score used when pushing updates to [`SttEngine`]/[`PipelineService`].
    pub fn set_hotwords_score(&mut self, score: f32) -> Result<(), CoreError> {
        if !score.is_finite() || score <= 0.0 {
            return Err(CoreError::Glossary(format!(
                "hotwords_score must be finite and > 0.0, got {score}"
            )));
        }
        self.hotwords_score = score;
        Ok(())
    }

    /// Returns the currently configured `(src_lang, tgt_lang)` language pair.
    #[must_use]
    pub const fn language_pair(&self) -> (&LanguageCode, &LanguageCode) {
        (&self.src_lang, &self.tgt_lang)
    }

    /// Returns all loaded [`GlossaryProfile`] entries.
    #[must_use]
    pub fn profiles(&self) -> &[GlossaryProfile] {
        &self.profiles
    }

    /// Returns the enabled profile names in priority order (later overrides earlier).
    #[must_use]
    pub fn enabled_profiles(&self) -> &[String] {
        &self.enabled_profiles
    }

    /// Returns `true` when an explicit `enabled_profiles` selection is active.
    #[must_use]
    pub const fn has_explicit_selection(&self) -> bool {
        self.explicit_selection
    }

    /// Returns the configured STT hotwords score.
    #[must_use]
    pub const fn hotwords_score(&self) -> f32 {
        self.hotwords_score
    }

    /// Returns the merged active glossary for the configured `(src_lang, tgt_lang)` pair.
    #[must_use]
    pub const fn active_merged(&self) -> &MergedGlossary {
        &self.active_merged
    }

    /// Builds an [`SttHotwordsConfig`] from [`Self::active_merged`] and [`Self::hotwords_score`].
    #[must_use]
    pub fn to_stt_hotwords_config(&self) -> SttHotwordsConfig {
        self.active_merged
            .to_stt_hotwords_config(self.hotwords_score)
    }

    /// Computes a [`MergedGlossary`] for an explicit `(src_lang, tgt_lang)` pair without mutating
    /// `self`.
    #[must_use]
    pub fn merged_for(&self, src_lang: &LanguageCode, tgt_lang: &LanguageCode) -> MergedGlossary {
        MergedGlossary::from_enabled_profiles(
            &self.profiles,
            &self.enabled_profiles,
            src_lang,
            tgt_lang,
        )
    }

    /// Pushes the active merged glossary into [`PipelineService`] (which propagates it to both
    /// active/future `ContextWindow`s and [`SttEngine::update_hotwords`]).
    pub async fn sync_pipeline(&self, pipeline: &mut PipelineService) -> Result<(), CoreError> {
        pipeline
            .set_glossary_with_score(self.active_merged.clone(), self.hotwords_score)
            .await
    }

    /// Pushes only STT hotwords derived from [`Self::active_merged`] into `stt_engine`.
    pub fn sync_stt_engine(&self, stt_engine: &dyn SttEngine) -> Result<(), CoreError> {
        let cfg = self.to_stt_hotwords_config();
        stt_engine.update_hotwords(&cfg)
    }

    /// Transactionally reloads profiles from `dir`, pushes into `pipeline`, and commits the new
    /// state onto `self` only if `sync_pipeline` succeeds (rolling back cleanly on error).
    pub async fn reload_dir_and_sync_pipeline(
        &mut self,
        repo: &dyn GlossaryRepository,
        dir: &Path,
        pipeline: &mut PipelineService,
    ) -> Result<(), CoreError> {
        let candidate_profiles = repo.load_dir(dir)?;
        ensure_unique_profile_names(&candidate_profiles)?;
        let candidate_enabled = if self.explicit_selection {
            self.enabled_profiles.clone()
        } else {
            default_enabled_names(&candidate_profiles)
        };
        let candidate_merged = MergedGlossary::from_enabled_profiles(
            &candidate_profiles,
            &candidate_enabled,
            &self.src_lang,
            &self.tgt_lang,
        );

        pipeline
            .set_glossary_with_score(candidate_merged.clone(), self.hotwords_score)
            .await?;

        self.profiles = candidate_profiles;
        self.enabled_profiles = candidate_enabled;
        self.active_merged = candidate_merged;
        Ok(())
    }

    /// Transactionally updates `enabled_profiles`, pushes into `pipeline`, and commits the new
    /// selection only if `sync_pipeline` succeeds (rolling back cleanly on error).
    pub async fn update_enabled_and_sync_pipeline(
        &mut self,
        enabled_profiles: Vec<String>,
        pipeline: &mut PipelineService,
    ) -> Result<(), CoreError> {
        let candidate_merged = MergedGlossary::from_enabled_profiles(
            &self.profiles,
            &enabled_profiles,
            &self.src_lang,
            &self.tgt_lang,
        );

        pipeline
            .set_glossary_with_score(candidate_merged.clone(), self.hotwords_score)
            .await?;

        self.enabled_profiles = enabled_profiles;
        self.explicit_selection = true;
        self.active_merged = candidate_merged;
        Ok(())
    }

    /// Transactionally reloads `dir` from `repo` and updates STT hotwords onto `stt_engine`,
    /// committing the new state onto `self` only if `update_hotwords` succeeds.
    pub fn reload_dir_and_sync_stt(
        &mut self,
        repo: &dyn GlossaryRepository,
        dir: &Path,
        stt_engine: &Arc<dyn SttEngine>,
    ) -> Result<(), CoreError> {
        let candidate_profiles = repo.load_dir(dir)?;
        ensure_unique_profile_names(&candidate_profiles)?;
        let candidate_enabled = if self.explicit_selection {
            self.enabled_profiles.clone()
        } else {
            default_enabled_names(&candidate_profiles)
        };
        let candidate_merged = MergedGlossary::from_enabled_profiles(
            &candidate_profiles,
            &candidate_enabled,
            &self.src_lang,
            &self.tgt_lang,
        );

        let stt_cfg = candidate_merged.to_stt_hotwords_config(self.hotwords_score);
        stt_engine.update_hotwords(&stt_cfg)?;

        self.profiles = candidate_profiles;
        self.enabled_profiles = candidate_enabled;
        self.active_merged = candidate_merged;
        Ok(())
    }

    fn recompute_active(&mut self) {
        self.active_merged = MergedGlossary::from_enabled_profiles(
            &self.profiles,
            &self.enabled_profiles,
            &self.src_lang,
            &self.tgt_lang,
        );
    }
}

fn ensure_unique_profile_names(profiles: &[GlossaryProfile]) -> Result<(), CoreError> {
    let mut seen = HashSet::new();
    for profile in profiles {
        if !seen.insert(&profile.name) {
            return Err(CoreError::Glossary(format!(
                "duplicate glossary profile name '{}' found",
                profile.name
            )));
        }
    }
    Ok(())
}

fn default_enabled_names(profiles: &[GlossaryProfile]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for profile in profiles {
        if seen.insert(profile.name.clone()) {
            names.push(profile.name.clone());
        }
    }
    names
}
