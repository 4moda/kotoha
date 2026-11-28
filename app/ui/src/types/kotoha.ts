export interface SpeakerKey {
  source: string;
  user_id: number;
}

export type SubtitleStage =
  | 'interim'
  | 'final_pending_translation'
  | 'translated'
  | 'translation_failed';

export interface SubtitleEntry {
  utterance_id: number;
  speaker_key: SpeakerKey;
  speaker_name: string;
  speaker_color: string;
  src_text: string;
  translated_text: string | null;
  stage: SubtitleStage;
  timestamp_ms: number;
  failure_reason?: string;
}

export type ComputeDevice = 'auto' | 'cpu' | 'cuda' | 'metal';

export type ModelBackend = 'c_translate2' | 'onnx' | 'open_ai_compatible';

export type ModelLicenseCaution = 'commercial_ok' | 'non_commercial';

export interface ModelProfile {
  model_id: string;
  display_name: string;
  backend: ModelBackend;
  supported_langs: string[];
  recommended_device: ComputeDevice;
  supported_devices: ComputeDevice[];
  compute_type: string;
  size_mb: number;
  license: string;
  license_caution: ModelLicenseCaution;
  summary: string;
  sha256: string;
  download_url: string;
}

export type TranslationProviderType =
  | 'local_model'
  | 'openai_compatible'
  | 'deepl'
  | 'google_cloud'
  | 'gas_webhook'
  | 'mock';

export interface LocalModelProviderConfig {
  type: 'local_model';
  model_id: string;
  device: ComputeDevice;
  threads: number | null;
}

export interface OpenAiCompatibleProviderConfig {
  type: 'openai_compatible';
  base_url: string;
  model: string;
  timeout_ms: number;
}

export interface DeepLProviderConfig {
  type: 'deepl';
  use_free_endpoint: boolean;
  timeout_ms: number;
}

export interface GoogleCloudProviderConfig {
  type: 'google_cloud';
  project_id: string | null;
  timeout_ms: number;
}

export interface GasWebhookProviderConfig {
  type: 'gas_webhook';
  timeout_ms: number;
}

export interface MockProviderConfig {
  type: 'mock';
  prefix: string;
}

export type TranslationProviderConfig =
  | LocalModelProviderConfig
  | OpenAiCompatibleProviderConfig
  | DeepLProviderConfig
  | GoogleCloudProviderConfig
  | GasWebhookProviderConfig
  | MockProviderConfig;

export type ConnectionState = 'connected' | 'reconnecting' | 'disconnected';

export interface ConnectionStatus {
  state: ConnectionState;
  source: string;
  plugin_version: string | null;
  endpoint: string;
  active_speakers: number;
  dropped_frames: number;
  last_heartbeat_ms: number | null;
}

export interface OverlayDisplayOptions {
  max_lines: number;
  show_source_text: boolean;
  locked: boolean;
  font_size_px: number;
  background_opacity: number;
}

export interface GlossaryProfileSummary {
  id: string;
  name: string;
  src_lang: string;
  tgt_lang: string;
  term_count: number;
  enabled: boolean;
  file_path?: string;
}

export interface GlossarySettingsState {
  profiles: GlossaryProfileSummary[];
  enabled_profiles: string[];
  hotwords_score: number;
  glossary_dir: string | null;
}

export type PluginInstallStatus =
  | 'installed'
  | 'already_up_to_date'
  | 'locked_by_running_app'
  | 'not_found'
  | 'error';

export type HostOsPlatform = 'windows' | 'macos' | 'linux';

export interface PluginInstallResult {
  status: PluginInstallStatus;
  platform: HostOsPlatform;
  target_path: string;
  detected_paths: string[];
  is_flatpak: boolean;
  flatpak_hint?: string;
  message: string;
}

export type ModelDownloadStatus =
  | 'idle'
  | 'downloading'
  | 'verifying_sha256'
  | 'verified'
  | 'failed';

export interface DownloadProgressEvent {
  model_id: string;
  bytes_downloaded: number;
  total_bytes: number;
  status: ModelDownloadStatus;
  sha256_verified: boolean;
  error_message?: string;
}

export type ProviderSecretKind =
  | 'open_ai_api_key'
  | 'deep_l_api_key'
  | 'google_cloud_api_key'
  | 'gas_endpoint_url'
  | 'gas_bearer_token';

/** Which provider secrets are stored. Booleans only: secret values are write-only. */
export interface SecretStatus {
  openai_api_key: boolean;
  deepl_api_key: boolean;
  google_cloud_api_key: boolean;
  gas_endpoint_url: boolean;
  gas_bearer_token: boolean;
}

/** Whether the configured translation provider is active (`error` = placeholder translator in use). */
export type TranslatorStatus = { state: 'ok' } | { state: 'error'; reason: string };

export interface AppRuntimeState {
  src_lang: string;
  tgt_lang: string;
  provider: TranslationProviderConfig;
  secrets: SecretStatus;
  translator_status: TranslatorStatus;
  /** Set when the settings file was unusable and defaults were loaded instead. */
  settings_notice: string | null;
  overlay_options: OverlayDisplayOptions;
  overlay_visible: boolean;
  glossary: GlossarySettingsState;
  connection: ConnectionStatus;
  subtitles: SubtitleEntry[];
  wizard_completed: boolean;
  platform: HostOsPlatform;
  detected_mumble_paths: string[];
  is_flatpak_mumble: boolean;
}
