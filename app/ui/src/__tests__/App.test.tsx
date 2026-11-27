import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../App';
import { getDeterministicSpeakerColor } from '../data/modelCatalog';

describe('Kotoha UI', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('renders 3-stage subtitle entries and deterministic speaker color badges', () => {
    render(<App />);

    const translatedEntry = screen.getByTestId('subtitle-entry-101');
    const finalPendingEntry = screen.getByTestId('subtitle-entry-102');
    const interimEntry = screen.getByTestId('subtitle-entry-103');

    expect(translatedEntry).toHaveAttribute('data-stage', 'translated');
    expect(finalPendingEntry).toHaveAttribute('data-stage', 'final_pending');
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

  it('progresses simulated utterance through interim -> final_pending -> translated', () => {
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
      'final_pending',
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
      'NLLB-200 1.3B (fp16) · CUDA',
    );

    // Switch across all remaining providers
    fireEvent.click(screen.getByTestId('provider-tab-openai_compatible'));
    expect(screen.getByTestId('config-openai-compatible')).toBeInTheDocument();
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'OpenAI-Compatible',
    );

    fireEvent.click(screen.getByTestId('provider-tab-deepl'));
    expect(screen.getByTestId('config-deepl')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-google_cloud'));
    expect(screen.getByTestId('config-google-cloud')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('provider-tab-gas_webhook'));
    expect(screen.getByTestId('config-gas-webhook')).toBeInTheDocument();
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
