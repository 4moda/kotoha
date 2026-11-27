export interface SpeakerKey {
  source: string;
  user_id: number;
}

export type SubtitleStage = 'interim' | 'final_pending_translation' | 'translated';

export interface SubtitleEntry {
  utterance_id: number;
  speaker_key: SpeakerKey;
  speaker_name: string;
  speaker_color: string;
  src_text: string;
  translated_text: string | null;
  stage: SubtitleStage;
  timestamp_ms: number;
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
  api_key: string | null;
  timeout_ms: number;
}

export interface DeepLProviderConfig {
  type: 'deepl';
  api_key: string;
  use_free_endpoint: boolean;
  timeout_ms: number;
}

export interface GoogleCloudProviderConfig {
  type: 'google_cloud';
  api_key: string;
  project_id: string | null;
  timeout_ms: number;
}

export interface GasWebhookProviderConfig {
  type: 'gas_webhook';
  endpoint_url: string;
  bearer_token: string | null;
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
