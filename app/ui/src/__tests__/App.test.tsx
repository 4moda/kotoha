import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../App';
import {
  SPEAKER_COLOR_PALETTE,
  createDefaultProviderConfig,
  getDeterministicSpeakerColor,
} from '../data/modelCatalog';

describe('Kotoha UI', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

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
      api_key: null,
      timeout_ms: 15_000,
    });
    expect(createDefaultProviderConfig('deepl')).toEqual({
      type: 'deepl',
      api_key: '',
      use_free_endpoint: true,
      timeout_ms: 10_000,
    });
    expect(createDefaultProviderConfig('google_cloud')).toEqual({
      type: 'google_cloud',
      api_key: '',
      project_id: null,
      timeout_ms: 10_000,
    });
    expect(createDefaultProviderConfig('gas_webhook')).toEqual({
      type: 'gas_webhook',
      endpoint_url: '',
      bearer_token: null,
      timeout_ms: 10_000,
    });
    expect(createDefaultProviderConfig('mock')).toEqual({
      type: 'mock',
      prefix: '[ja] ',
    });
  });

  it('renders 3-stage subtitle entries and deterministic speaker color badges', () => {
    render(<App />);

    const translatedEntry = screen.getByTestId('subtitle-entry-101');
    const finalPendingEntry = screen.getByTestId('subtitle-entry-102');
    const interimEntry = screen.getByTestId('subtitle-entry-103');

    expect(translatedEntry).toHaveAttribute('data-stage', 'translated');
    expect(finalPendingEntry).toHaveAttribute('data-stage', 'final_pending_translation');
    expect(interimEntry).toHaveAttribute('data-stage', 'interim');

    expect(
      screen.getByText('10秒後に一緒にミッドを押し上げよう、横からの奇襲に注意して。'),
    ).toBeInTheDocument();
    expect(
      screen.getByText('Push mid together in ten seconds, watch the flank.'),
    ).toBeInTheDocument();

    const expectedAlexColor = getDeterministicSpeakerColor({
      source: 'mumble',
      user_id: 12,
    });
    const alexBadge = screen.getByTestId('speaker-badge-101');
    expect(alexBadge).toHaveTextContent('Alex');
    expect(alexBadge).toHaveStyle({ borderColor: expectedAlexColor });
  });

  it('progresses simulated utterance through interim -> final_pending_translation -> translated', () => {
    vi.useFakeTimers();
    render(<App />);

    fireEvent.click(screen.getByTestId('simulate-utterance-btn'));

    const simulated = screen.getByTestId('subtitle-entry-104');
    expect(simulated).toHaveAttribute('data-stage', 'interim');
    expect(screen.getByText('One enemy is...')).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(360);
    });
    expect(screen.getByTestId('subtitle-entry-104')).toHaveAttribute(
      'data-stage',
      'final_pending_translation',
    );
    expect(
      screen.getByText('One enemy is rotating through upper tunnel.'),
    ).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(500);
    });
    expect(screen.getByTestId('subtitle-entry-104')).toHaveAttribute(
      'data-stage',
      'translated',
    );
    expect(
      screen.getByText('敵が1人アッパートンネルから回ってきている。'),
    ).toBeInTheDocument();
  });

  it('switches translation providers and selects local model profiles & compute devices', () => {
    render(<App />);

    fireEvent.click(screen.getByTestId('mode-settings-btn'));
    expect(screen.getByTestId('translation-settings')).toBeInTheDocument();

    // Default local model profile & license badge
    expect(screen.getByTestId('config-local-model')).toBeInTheDocument();
    expect(screen.getByTestId('model-license-badge')).toHaveTextContent('CC-BY-SA-4.0');

    // Switch to NLLB-200 1.3B (non-commercial) and CUDA device
    fireEvent.change(screen.getByTestId('local-model-select'), {
      target: { value: 'nllb-200-1.3b-fp16' },
    });
    fireEvent.change(screen.getByTestId('compute-device-select'), {
      target: { value: 'cuda' },
    });

    expect(screen.getByTestId('model-license-badge')).toHaveTextContent('CC-BY-NC-4.0');
    expect(screen.getByTestId('non-commercial-warning')).toBeInTheDocument();
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'NLLB-200 1.3B (High-Accuracy GPU FP16, Non-Commercial) · CUDA',
    );

    // Switch across all remaining providers
    fireEvent.click(screen.getByTestId('provider-tab-openai_compatible'));
    expect(screen.getByTestId('config-openai-compatible')).toBeInTheDocument();
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'OpenAI-Compatible',
    );

    fireEvent.click(screen.getByTestId('provider-tab-deepl'));
    expect(screen.getByTestId('config-deepl')).toBeInTheDocument();
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent('DeepL (FREE)');

    fireEvent.click(screen.getByTestId('provider-tab-google_cloud'));
    expect(screen.getByTestId('config-google-cloud')).toBeInTheDocument();
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent('Google Cloud (v2)');

    fireEvent.click(screen.getByTestId('provider-tab-gas_webhook'));
    expect(screen.getByTestId('config-gas-webhook')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-mock'));
    expect(screen.getByTestId('config-mock')).toBeInTheDocument();
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent('Mock Translator ([ja] )');
  });

  it('toggles overlay click-through lock state', () => {
    render(<App />);

    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Click-Through Locked',
    );
    fireEvent.click(screen.getByRole('button', { name: 'Unlock Position' }));
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Position Unlocked',
    );
  });
});
