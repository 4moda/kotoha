import { describe, expect, it } from 'vitest';
import {
  COMPUTE_DEVICES,
  MODEL_CATALOG,
  PROVIDER_METADATA,
  SPEAKER_COLOR_PALETTE,
  createDefaultProviderConfig,
  findModelProfile,
  getDeterministicSpeakerColor,
} from './modelCatalog';

describe('modelCatalog', () => {
  it('computes deterministic 64-bit FNV-1a speaker colors matching Rust SpeakerKey::color_hex()', () => {
    expect(SPEAKER_COLOR_PALETTE).toHaveLength(12);
    expect(getDeterministicSpeakerColor({ source: 'mumble', user_id: 42 })).toBe('#F472B6');
    expect(getDeterministicSpeakerColor({ source: 'ts3', user_id: 42 })).toBe('#38BDF8');
    expect(getDeterministicSpeakerColor({ source: 'mumble', user_id: 12 })).toBe('#38BDF8');
    expect(getDeterministicSpeakerColor({ source: 'mumble', user_id: 27 })).toBe('#FBBF24');
    expect(getDeterministicSpeakerColor({ source: 'mumble', user_id: 41 })).toBe('#34D399');
    expect(
      getDeterministicSpeakerColor({ source: 'discord-jp日本語', user_id: 0xdeadbeef }),
    ).toBe('#60A5FA');
  });

  it('produces TranslationProviderConfig objects matching Rust serde schema', () => {
    expect(createDefaultProviderConfig('local_model')).toEqual({
      type: 'local_model',
      model_id: 'fugumt-en-ja-int8',
      device: 'auto',
      threads: null,
    });
    expect(createDefaultProviderConfig('openai_compatible')).toEqual({
      type: 'openai_compatible',
      base_url: 'http://localhost:11434/v1',
      model: 'qwen2.5:7b',
      timeout_ms: 15_000,
    });
    expect(createDefaultProviderConfig('deepl')).toEqual({
      type: 'deepl',
      use_free_endpoint: true,
      timeout_ms: 10_000,
    });
    expect(createDefaultProviderConfig('google_cloud')).toEqual({
      type: 'google_cloud',
      project_id: null,
      timeout_ms: 10_000,
    });
    expect(createDefaultProviderConfig('gas_webhook')).toEqual({
      type: 'gas_webhook',
      timeout_ms: 10_000,
    });
    expect(createDefaultProviderConfig('mock')).toEqual({
      type: 'mock',
      prefix: '[ja] ',
    });
  });

  it('exposes complete MODEL_CATALOG profiles with license caution metadata and fallback lookup', () => {
    expect(MODEL_CATALOG.map((profile) => profile.model_id)).toEqual([
      'fugumt-en-ja-int8',
      'opus-mt-en-ja-int8',
      'nllb-200-distilled-600m-int8',
      'nllb-200-1.3b-fp16',
    ]);

    for (const profile of MODEL_CATALOG) {
      expect(profile.backend).toBe('c_translate2');
      expect(profile.size_mb).toBeGreaterThan(0);
      expect(profile.supported_devices.length).toBeGreaterThan(0);
      expect(profile.download_url).toMatch(/^https:\/\//);
    }

    expect(findModelProfile('fugumt-en-ja-int8').license_caution).toBe('commercial_ok');
    expect(findModelProfile('opus-mt-en-ja-int8').license_caution).toBe('commercial_ok');
    expect(findModelProfile('nllb-200-distilled-600m-int8').license_caution).toBe(
      'non_commercial',
    );
    expect(findModelProfile('nllb-200-1.3b-fp16').license_caution).toBe('non_commercial');

    // Unknown model ID falls back to default first profile
    expect(findModelProfile('unknown-model-id').model_id).toBe('fugumt-en-ja-int8');
  });

  it('defines all compute devices and all 6 translation providers', () => {
    expect(COMPUTE_DEVICES.map((d) => d.value)).toEqual(['auto', 'cpu', 'cuda', 'metal']);
    expect(PROVIDER_METADATA.map((p) => p.type)).toEqual([
      'local_model',
      'openai_compatible',
      'deepl',
      'google_cloud',
      'gas_webhook',
      'mock',
    ]);
  });
});
