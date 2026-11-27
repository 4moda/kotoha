import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';

describe('App', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('renders overlay preview mode by default and switches between overlay and settings', () => {
    render(<App />);

    expect(screen.getByTestId('ipc-status-pill')).toHaveTextContent('IPC connected (mumble)');
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'FuguMT EN->JA (CPU Fast INT8) · AUTO',
    );
    expect(screen.getByTestId('overlay-preview-stage')).toBeInTheDocument();
    expect(screen.queryByTestId('translation-settings')).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId('mode-settings-btn'));
    expect(screen.getByTestId('translation-settings')).toBeInTheDocument();
    expect(screen.queryByTestId('overlay-preview-stage')).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId('mode-overlay-btn'));
    expect(screen.getByTestId('overlay-preview-stage')).toBeInTheDocument();
  });

  it('progresses simulated utterance through 3 stages and resets demo state', () => {
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

    fireEvent.click(screen.getByTestId('reset-demo-btn'));
    expect(screen.queryByTestId('subtitle-entry-104')).not.toBeInTheDocument();
    expect(screen.getByTestId('subtitle-entry-101')).toBeInTheDocument();
  });

  it('integrates settings changes with overlay rendering and active engine summary', () => {
    vi.useFakeTimers();
    render(<App />);

    // Toggle overlay lock directly on overlay and verify in settings
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Click-Through Locked',
    );
    fireEvent.click(screen.getByRole('button', { name: 'Unlock Position' }));
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Position Unlocked',
    );

    // Switch to settings, update provider to NLLB CUDA, then to Mock
    fireEvent.click(screen.getByTestId('mode-settings-btn'));
    const lockCheckbox = screen.getByLabelText(
      /Lock overlay for mouse click-through/i,
    ) as HTMLInputElement;
    expect(lockCheckbox.checked).toBe(false);

    fireEvent.change(screen.getByTestId('local-model-select'), {
      target: { value: 'nllb-200-1.3b-fp16' },
    });
    fireEvent.change(screen.getByTestId('compute-device-select'), {
      target: { value: 'cuda' },
    });
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'NLLB-200 1.3B (High-Accuracy GPU FP16, Non-Commercial) · CUDA',
    );

    fireEvent.click(screen.getByTestId('provider-tab-mock'));
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'Mock Translator ([ja] )',
    );

    // Switch back to overlay and verify mock prefix is used in simulated translation
    fireEvent.click(screen.getByTestId('mode-overlay-btn'));
    fireEvent.click(screen.getByTestId('simulate-utterance-btn'));

    act(() => {
      vi.advanceTimersByTime(860);
    });

    expect(
      screen.getByText('[ja] One enemy is rotating through upper tunnel.'),
    ).toBeInTheDocument();
  });
});
