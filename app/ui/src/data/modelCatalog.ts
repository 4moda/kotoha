import type {
  ComputeDevice,
  ModelProfile,
  SpeakerKey,
  TranslationProviderConfig,
  TranslationProviderType,
} from '../types/kotoha';

export const MODEL_CATALOG: readonly ModelProfile[] = [
  {
    model_id: 'fugumt-en-ja-int8',
    display_name: 'FuguMT EN→JA (int8)',
    backend: 'ctranslate2',
    supported_langs: ['en->ja'],
    recommended_device: 'cpu',
    supported_devices: ['auto', 'cpu', 'cuda'],
    compute_type: 'int8',
    size_mb: 200,
    license: 'CC-BY-SA-4.0',
    license_caution: 'commercial_ok',
    summary:
      'Ultra-low-latency CPU-friendly English-to-Japanese default NMT model (tens of ms per utterance).',
    sha256: 'pending-release-manifest',
    download_url: 'https://huggingface.co/kotoha-models/fugumt-en-ja-ct2-int8',
  },
  {
    model_id: 'opus-mt-en-ja-int8',
    display_name: 'OPUS-MT EN→JA (int8)',
    backend: 'ctranslate2',
    supported_langs: ['en->ja'],
    recommended_device: 'cpu',
    supported_devices: ['auto', 'cpu', 'cuda'],
    compute_type: 'int8',
    size_mb: 150,
    license: 'CC-BY-4.0',
    license_caution: 'commercial_ok',
    summary:
      'Compact Helsinki-NLP model with permissive CC-BY-4.0 attribution terms for CPU execution.',
    sha256: 'pending-release-manifest',
    download_url: 'https://huggingface.co/kotoha-models/opus-mt-en-ja-ct2-int8',
  },
  {
    model_id: 'nllb-200-distilled-600m-int8',
    display_name: 'NLLB-200 Distilled 600M (int8)',
    backend: 'ctranslate2',
    supported_langs: ['multilingual'],
    recommended_device: 'auto',
    supported_devices: ['auto', 'cpu', 'cuda'],
    compute_type: 'int8',
    size_mb: 650,
    license: 'CC-BY-NC-4.0',
    license_caution: 'non_commercial',
    summary:
      '200-language distilled NMT model balanced for CPU or CUDA GPU. Non-commercial license only.',
    sha256: 'pending-release-manifest',
    download_url: 'https://huggingface.co/kotoha-models/nllb-200-distilled-600m-ct2-int8',
  },
  {
    model_id: 'nllb-200-1.3b-fp16',
    display_name: 'NLLB-200 1.3B (fp16)',
    backend: 'ctranslate2',
    supported_langs: ['multilingual'],
    recommended_device: 'cuda',
    supported_devices: ['auto', 'cuda', 'cpu'],
    compute_type: 'float16',
    size_mb: 2600,
    license: 'CC-BY-NC-4.0',
    license_caution: 'non_commercial',
    summary:
      'High-accuracy 200-language NMT for CUDA GPUs (4GB+ VRAM recommended). Non-commercial license only.',
    sha256: 'pending-release-manifest',
    download_url: 'https://huggingface.co/kotoha-models/nllb-200-1.3b-ct2-fp16',
  },
] as const;

export const COMPUTE_DEVICES: ReadonlyArray<{
  value: ComputeDevice;
  label: string;
  hint: string;
}> = [
  { value: 'auto', label: 'Auto', hint: 'Prefer CUDA/Metal when detected, fallback to CPU' },
  { value: 'cpu', label: 'CPU', hint: 'Cross-platform int8 inference without GPU' },
  { value: 'cuda', label: 'CUDA', hint: 'NVIDIA GPU acceleration (Windows / Linux)' },
  { value: 'metal', label: 'Metal', hint: 'Apple Silicon GPU acceleration (macOS)' },
];

export const PROVIDER_METADATA: ReadonlyArray<{
  type: TranslationProviderType;
  label: string;
  badge: string;
  description: string;
}> = [
  {
    type: 'local_model',
    label: 'Local Model (NMT)',
    badge: 'Offline Default',
    description: 'Zero-cost offline CTranslate2 neural machine translation with SHA-256 verified models.',
  },
  {
    type: 'openai_compatible',
    label: 'OpenAI-Compatible LLM',
    badge: 'Context-Aware',
    description: 'Supports OpenAI, Gemini OpenAI endpoint, or local Ollama / LM Studio with conversation context.',
  },
  {
    type: 'deepl',
    label: 'DeepL API',
    badge: 'Cloud API',
    description: 'Official DeepL Free or Pro API with optional formality preference.',
  },
  {
    type: 'google_cloud',
    label: 'Google Cloud Translation',
    badge: 'Cloud API',
    description: 'Official Google Cloud Translation REST API v2/v3 (no scraping endpoints).',
  },
  {
    type: 'gas_webhook',
    label: 'GAS / Custom HTTP Proxy',
    badge: 'BYO Endpoint',
    description: 'User-deployed Google Apps Script LanguageApp webhook or custom JSON proxy.',
  },
  {
    type: 'mock',
    label: 'Mock Translator',
    badge: 'Dev / QA',
    description: 'Deterministic offline echo translator for pipeline and UI verification.',
  },
];

export function findModelProfile(modelId: string): ModelProfile {
  return (
    MODEL_CATALOG.find((profile) => profile.model_id === modelId) ?? MODEL_CATALOG[0]!
  );
}

export function createDefaultProviderConfig(
  type: TranslationProviderType,
): TranslationProviderConfig {
  switch (type) {
    case 'local_model':
      return {
        type: 'local_model',
        model_id: 'fugumt-en-ja-int8',
        device: 'auto',
        compute_type: 'int8',
        num_threads: 0,
      };
    case 'openai_compatible':
      return {
        type: 'openai_compatible',
        base_url: 'http://localhost:11434/v1',
        model: 'qwen2.5:7b',
        api_key: '',
        context_window: 8,
      };
    case 'deepl':
      return {
        type: 'deepl',
        plan: 'free',
        api_key: '',
        formality: 'default',
      };
    case 'google_cloud':
      return {
        type: 'google_cloud',
        api_version: 'v2',
        project_id: '',
        api_key: '',
      };
    case 'gas_webhook':
      return {
        type: 'gas_webhook',
        endpoint_url: '',
        bearer_token: '',
      };
    case 'mock':
      return {
        type: 'mock',
        latency_ms: 40,
      };
  }
}

const SPEAKER_PALETTE = [
  '#38BDF8', // sky
  '#34D399', // emerald
  '#FBBF24', // amber
  '#FB7185', // rose
  '#2DD4BF', // teal
  '#F97316', // orange
  '#60A5FA', // blue
  '#A3E635', // lime
] as const;

export function getDeterministicSpeakerColor(speakerKey: SpeakerKey): string {
  let hash = 2166136261;
  const input = `${speakerKey.source}:${speakerKey.user_id}`;
  for (let i = 0; i < input.length; i += 1) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  const index = (hash >>> 0) % SPEAKER_PALETTE.length;
  return SPEAKER_PALETTE[index] ?? SPEAKER_PALETTE[0];
}
