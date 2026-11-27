import { useState } from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { createDefaultProviderConfig } from '../data/modelCatalog';
import type {
  GlossarySettingsState,
  OverlayDisplayOptions,
  TranslationProviderConfig,
} from '../types/kotoha';
import { TranslationSettings } from './TranslationSettings';

const defaultOverlayOptions: OverlayDisplayOptions = {
  max_lines: 4,
  show_source_text: true,
  locked: true,
  font_size_px: 18,
  background_opacity: 0.78,
};

const defaultGlossarySettings: GlossarySettingsState = {
  profiles: [
    {
      id: 'sample',
      name: 'My Profile',
      src_lang: 'en',
      tgt_lang: 'ja',
      term_count: 4,
      enabled: true,
      file_path: 'glossaries/sample.toml',
    },
    {
      id: 'fps-callouts',
      name: 'FPS Callouts',
      src_lang: 'en',
      tgt_lang: 'ja',
      term_count: 12,
      enabled: false,
      file_path: 'glossaries/fps-callouts.toml',
    },
  ],
  enabled_profiles: ['My Profile'],
  hotwords_score: 1.5,
  glossary_dir: 'glossaries',
};

function StatefulTranslationSettings({
  initialProvider = createDefaultProviderConfig('local_model'),
}: {
  initialProvider?: TranslationProviderConfig;
}) {
  const [srcLang, setSrcLang] = useState('en');
  const [tgtLang, setTgtLang] = useState('ja');
  const [provider, setProvider] = useState<TranslationProviderConfig>(initialProvider);
  const [overlayOptions, setOverlayOptions] =
    useState<OverlayDisplayOptions>(defaultOverlayOptions);
  const [glossarySettings, setGlossarySettings] =
    useState<GlossarySettingsState>(defaultGlossarySettings);

  return (
    <TranslationSettings
      srcLang={srcLang}
      tgtLang={tgtLang}
      provider={provider}
      overlayOptions={overlayOptions}
      glossarySettings={glossarySettings}
      onChangeLanguages={(nextSrc, nextTgt) => {
        setSrcLang(nextSrc);
        setTgtLang(nextTgt);
      }}
      onChangeProvider={setProvider}
      onChangeOverlayOptions={setOverlayOptions}
      onChangeGlossarySettings={(next) =>
        setGlossarySettings((prev) => ({
          ...prev,
          enabled_profiles: next.enabled_profiles,
          hotwords_score: next.hotwords_score,
          profiles: prev.profiles.map((p) => ({
            ...p,
            enabled: next.enabled_profiles.includes(p.name),
          })),
        }))
      }
    />
  );
}

describe('TranslationSettings', () => {
  it('switches across all 6 translation providers and renders matching config forms', () => {
    render(<StatefulTranslationSettings />);

    expect(screen.getByTestId('provider-tab-local_model')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    expect(screen.getByTestId('config-local-model')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-openai_compatible'));
    expect(screen.getByTestId('provider-tab-openai_compatible')).toHaveAttribute(
      'aria-checked',
      'true',
    );
    expect(screen.getByTestId('config-openai-compatible')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-deepl'));
    expect(screen.getByTestId('config-deepl')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-google_cloud'));
    expect(screen.getByTestId('config-google-cloud')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-gas_webhook'));
    expect(screen.getByTestId('config-gas-webhook')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-mock'));
    expect(screen.getByTestId('config-mock')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-local_model'));
    expect(screen.getByTestId('config-local-model')).toBeInTheDocument();
  });

  it('selects local model profiles, compute devices, and displays license notices', () => {
    const onChangeProvider = vi.fn();
    const { rerender } = render(
      <TranslationSettings
        srcLang="en"
        tgtLang="ja"
        provider={createDefaultProviderConfig('local_model')}
        overlayOptions={defaultOverlayOptions}
        glossarySettings={defaultGlossarySettings}
        onChangeLanguages={vi.fn()}
        onChangeProvider={onChangeProvider}
        onChangeOverlayOptions={vi.fn()}
        onChangeGlossarySettings={vi.fn()}
      />,
    );

    expect(screen.getByTestId('model-license-badge')).toHaveTextContent('CC-BY-SA-4.0');
    expect(screen.getByTestId('model-size-value')).toHaveTextContent('200 MB');
    expect(screen.queryByTestId('non-commercial-warning')).not.toBeInTheDocument();

    fireEvent.change(screen.getByTestId('local-model-select'), {
      target: { value: 'nllb-200-1.3b-fp16' },
    });
    expect(onChangeProvider).toHaveBeenLastCalledWith({
      type: 'local_model',
      model_id: 'nllb-200-1.3b-fp16',
      device: 'auto',
      threads: null,
    });

    rerender(
      <TranslationSettings
        srcLang="en"
        tgtLang="ja"
        provider={{
          type: 'local_model',
          model_id: 'nllb-200-1.3b-fp16',
          device: 'cuda',
          threads: null,
        }}
        overlayOptions={defaultOverlayOptions}
        glossarySettings={defaultGlossarySettings}
        onChangeLanguages={vi.fn()}
        onChangeProvider={onChangeProvider}
        onChangeOverlayOptions={vi.fn()}
        onChangeGlossarySettings={vi.fn()}
      />,
    );

    expect(screen.getByTestId('model-license-badge')).toHaveTextContent('CC-BY-NC-4.0');
    expect(screen.getByTestId('model-size-value')).toHaveTextContent('2.6 GB');
    expect(screen.getByTestId('non-commercial-warning')).toHaveTextContent(
      'Non-Commercial Notice: CC-BY-NC-4.0 prohibits commercial use.',
    );

    fireEvent.change(screen.getByTestId('compute-device-select'), {
      target: { value: 'metal' },
    });
    expect(onChangeProvider).toHaveBeenLastCalledWith({
      type: 'local_model',
      model_id: 'nllb-200-1.3b-fp16',
      device: 'metal',
      threads: null,
    });
  });

  it('updates language pair and overlay appearance controls via callbacks', () => {
    const onChangeLanguages = vi.fn();
    const onChangeOverlayOptions = vi.fn();

    render(
      <TranslationSettings
        srcLang="en"
        tgtLang="ja"
        provider={createDefaultProviderConfig('local_model')}
        overlayOptions={defaultOverlayOptions}
        glossarySettings={defaultGlossarySettings}
        onChangeLanguages={onChangeLanguages}
        onChangeProvider={vi.fn()}
        onChangeOverlayOptions={onChangeOverlayOptions}
        onChangeGlossarySettings={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText(/Source Language/i), {
      target: { value: 'ko' },
    });
    expect(onChangeLanguages).toHaveBeenLastCalledWith('ko', 'ja');

    fireEvent.change(screen.getByLabelText(/Target Language/i), {
      target: { value: 'zh' },
    });
    expect(onChangeLanguages).toHaveBeenLastCalledWith('en', 'zh');

    fireEvent.click(
      screen.getByLabelText(/Show original source text beneath translated subtitle/i),
    );
    expect(onChangeOverlayOptions).toHaveBeenLastCalledWith({
      ...defaultOverlayOptions,
      show_source_text: false,
    });

    fireEvent.click(screen.getByLabelText(/Lock overlay for mouse click-through/i));
    expect(onChangeOverlayOptions).toHaveBeenLastCalledWith({
      ...defaultOverlayOptions,
      locked: false,
    });
  });

  it('renders TOML glossary profiles, toggles enabled profiles, and adjusts hotwords_score', () => {
    const onChangeGlossarySettings = vi.fn();

    render(
      <TranslationSettings
        srcLang="en"
        tgtLang="ja"
        provider={createDefaultProviderConfig('local_model')}
        overlayOptions={defaultOverlayOptions}
        glossarySettings={defaultGlossarySettings}
        onChangeLanguages={vi.fn()}
        onChangeProvider={vi.fn()}
        onChangeOverlayOptions={vi.fn()}
        onChangeGlossarySettings={onChangeGlossarySettings}
      />,
    );

    expect(screen.getByTestId('glossary-hotwords-section')).toBeInTheDocument();
    expect(screen.getByTestId('glossary-profile-sample')).toHaveTextContent('My Profile');
    expect(screen.getByTestId('glossary-profile-sample')).toHaveTextContent('en -> ja');
    expect(screen.getByTestId('glossary-profile-sample')).toHaveTextContent('4 terms');

    const sampleToggle = screen.getByTestId('glossary-toggle-sample') as HTMLInputElement;
    const fpsToggle = screen.getByTestId('glossary-toggle-fps-callouts') as HTMLInputElement;
    expect(sampleToggle.checked).toBe(true);
    expect(fpsToggle.checked).toBe(false);

    // Enable second glossary profile
    fireEvent.click(fpsToggle);
    expect(onChangeGlossarySettings).toHaveBeenLastCalledWith({
      enabled_profiles: ['My Profile', 'FPS Callouts'],
      hotwords_score: 1.5,
    });

    // Disable first glossary profile
    fireEvent.click(sampleToggle);
    expect(onChangeGlossarySettings).toHaveBeenLastCalledWith({
      enabled_profiles: [],
      hotwords_score: 1.5,
    });

    // Update hotwords_score slider
    const slider = screen.getByTestId('hotwords-score-slider') as HTMLInputElement;
    expect(slider.min).toBe('0.5');
    expect(slider.max).toBe('3');
    fireEvent.change(slider, { target: { value: '2.4' } });
    expect(onChangeGlossarySettings).toHaveBeenLastCalledWith({
      enabled_profiles: ['My Profile'],
      hotwords_score: 2.4,
    });
  });
});
