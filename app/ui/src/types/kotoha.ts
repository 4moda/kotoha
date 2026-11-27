export interface SpeakerKey {
  source: string;
  user_id: number;
}

export type SubtitleStage = 'interim' | 'final_pending' | 'translated';

export interface SubtitleEntry {
  utterance_id: number;
  speaker_key: SpeakerKey;
  speaker_name: string;
  speaker_color: string;
  src_text: string;
  translated_text: string | null;
  is_final_stt: boolean;
  timestamp_ms: number;
}

export function getSubtitleStage(entry: SubtitleEntry): SubtitleStage {
  if (!entry.is_final_stt) {
    return 'interim';
  }
  if (entry.translated_text === null) {
    return 'final_pending';
  }
  return 'translated';
}

export type ComputeDevice = 'auto' | 'cpu' | 'cuda' | 'metal';

export type ModelLicenseCaution = 'commercial_ok' | 'non_commercial';

export interface ModelProfile {
  model_id: string;
  display_name: string;
  backend: string;
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

export type DeepLPlan = 'free' | 'pro';
export type GoogleTranslateApiVersion = 'v2' | 'v3';

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
  compute_type: string;
  num_threads: number;
}

export interface OpenAiCompatibleProviderConfig {
  type: 'openai_compatible';
  base_url: string;
  model: string;
  api_key: string;
  context_window: number;
}

export interface DeepLProviderConfig {
  type: 'deepl';
  plan: DeepLPlan;
  api_key: string;
  formality: string;
}

export interface GoogleCloudProviderConfig {
  type: 'google_cloud';
  api_version: GoogleTranslateApiVersion;
  project_id: string;
  api_key: string;
}

export interface GasWebhookProviderConfig {
  type: 'gas_webhook';
  endpoint_url: string;
  bearer_token: string;
}

export interface MockProviderConfig {
  type: 'mock';
  latency_ms: number;
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
