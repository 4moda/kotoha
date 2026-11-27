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
    display_name: 'FuguMT EN->JA (CPU Fast INT8)',
    backend: 'c_translate2',
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
    display_name: 'OPUS-MT EN->JA (CPU Permissive INT8)',
    backend: 'c_translate2',
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
    display_name: 'NLLB-200 Distilled 600M (Multilingual INT8, Non-Commercial)',
    backend: 'c_translate2',
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
    display_name: 'NLLB-200 1.3B (High-Accuracy GPU FP16, Non-Commercial)',
    backend: 'c_translate2',
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
    description: 'Official DeepL Free or Pro API.',
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
        threads: null,
      };
    case 'openai_compatible':
      return {
        type: 'openai_compatible',
        base_url: 'http://localhost:11434/v1',
        model: 'qwen2.5:7b',
        timeout_ms: 15_000,
      };
    case 'deepl':
      return {
        type: 'deepl',
        use_free_endpoint: true,
        timeout_ms: 10_000,
      };
    case 'google_cloud':
      return {
        type: 'google_cloud',
        project_id: null,
        timeout_ms: 10_000,
      };
    case 'gas_webhook':
      return {
        type: 'gas_webhook',
        timeout_ms: 10_000,
      };
    case 'mock':
      return {
        type: 'mock',
        prefix: '[ja] ',
      };
  }
}

export const SPEAKER_COLOR_PALETTE = [
  '#38BDF8', // sky
  '#34D399', // emerald
  '#F472B6', // pink
  '#FBBF24', // amber
  '#A78BFA', // violet
  '#FB7185', // rose
  '#2DD4BF', // teal
  '#60A5FA', // blue
  '#F97316', // orange
  '#4ADE80', // green
  '#E879F9', // fuchsia
  '#FACC15', // yellow
] as const;

const FNV1A_64_OFFSET_BASIS = 0xcbf29ce484222325n;
const FNV1A_64_PRIME = 0x0100000001b3n;
const UINT64_MASK = 0xffffffffffffffffn;
const utf8Encoder = new TextEncoder();

/**
 * Computes the exact deterministic speaker color (`#RRGGBB`) matching
 * `SpeakerKey::color_hex()` in `crates/kotoha-core/src/domain/entities.rs`.
 */
export function getDeterministicSpeakerColor(speakerKey: SpeakerKey): string {
  let hash = FNV1A_64_OFFSET_BASIS;
  const sourceBytes = utf8Encoder.encode(speakerKey.source);

  for (const byte of sourceBytes) {
    hash ^= BigInt(byte);
    hash = (hash * FNV1A_64_PRIME) & UINT64_MASK;
  }

  hash ^= 0xffn;
  hash = (hash * FNV1A_64_PRIME) & UINT64_MASK;

  const userId = speakerKey.user_id >>> 0;
  for (let shift = 0; shift < 32; shift += 8) {
    const byte = (userId >>> shift) & 0xff;
    hash ^= BigInt(byte);
    hash = (hash * FNV1A_64_PRIME) & UINT64_MASK;
  }

  const index = Number(hash % BigInt(SPEAKER_COLOR_PALETTE.length));
  return SPEAKER_COLOR_PALETTE[index] ?? SPEAKER_COLOR_PALETTE[0];
}
