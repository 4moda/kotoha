import { useState } from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { createDefaultProviderConfig } from '../data/modelCatalog';
import type {
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

  return (
    <TranslationSettings
      srcLang={srcLang}
      tgtLang={tgtLang}
      provider={provider}
      overlayOptions={overlayOptions}
      onChangeLanguages={(nextSrc, nextTgt) => {
        setSrcLang(nextSrc);
        setTgtLang(nextTgt);
      }}
      onChangeProvider={setProvider}
      onChangeOverlayOptions={setOverlayOptions}
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
        onChangeLanguages={vi.fn()}
        onChangeProvider={onChangeProvider}
        onChangeOverlayOptions={vi.fn()}
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
        onChangeLanguages={vi.fn()}
        onChangeProvider={onChangeProvider}
        onChangeOverlayOptions={vi.fn()}
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
        onChangeLanguages={onChangeLanguages}
        onChangeProvider={vi.fn()}
        onChangeOverlayOptions={onChangeOverlayOptions}
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
});
