import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { createMockBridge, resetDefaultMockBridgeForTesting } from './services/tauriBridge';

describe('App', () => {
  afterEach(() => {
    vi.useRealTimers();
    resetDefaultMockBridgeForTesting();
    window.history.replaceState({}, '', '/');
    document.documentElement.classList.remove('overlay-window-root');
    document.body.classList.remove('overlay-window-root');
  });

  it('automatically switches to setup wizard when wizard_completed is false and allows switching across views', async () => {
    const bridge = createMockBridge({ wizard_completed: false });
    render(<App bridge={bridge} />);

    await waitFor(() => {
      expect(screen.getByTestId('ipc-status-pill')).toHaveTextContent('IPC connected (mumble)');
      expect(screen.getByTestId('setup-wizard')).toBeInTheDocument();
    });
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'FuguMT EN->JA (CPU Fast INT8) · AUTO',
    );
    expect(screen.queryByTestId('overlay-preview-stage')).not.toBeInTheDocument();
    expect(screen.queryByTestId('translation-settings')).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId('mode-overlay-btn'));
    expect(screen.getByTestId('overlay-preview-stage')).toBeInTheDocument();
    expect(screen.queryByTestId('setup-wizard')).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId('mode-settings-btn'));
    expect(screen.getByTestId('translation-settings')).toBeInTheDocument();
    expect(screen.queryByTestId('overlay-preview-stage')).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId('mode-wizard-btn'));
    expect(screen.getByTestId('setup-wizard')).toBeInTheDocument();
  });

  it('does not hijack mode back to wizard if user manually navigates before getAppState resolves', async () => {
    const baseState = await createMockBridge().getAppState();
    let resolveState!: (value: typeof baseState) => void;
    const pendingState = new Promise<typeof baseState>((resolve) => {
      resolveState = resolve;
    });

    const bridge = createMockBridge({ wizard_completed: false });
    vi.spyOn(bridge, 'getAppState').mockReturnValue(pendingState);

    render(<App bridge={bridge} />);
    fireEvent.click(screen.getByTestId('mode-settings-btn'));
    expect(screen.getByTestId('translation-settings')).toBeInTheDocument();

    await act(async () => {
      resolveState({ ...baseState, wizard_completed: false });
      await Promise.resolve();
    });

    expect(screen.getByTestId('translation-settings')).toBeInTheDocument();
    expect(screen.queryByTestId('setup-wizard')).not.toBeInTheDocument();
  });

  it('progresses simulated utterance through 3 stages and resets demo state', async () => {
    vi.useFakeTimers();
    const bridge = createMockBridge({ wizard_completed: true });
    render(<App bridge={bridge} />);
    await act(async () => {
      await Promise.resolve();
    });

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

  it('integrates settings changes with overlay rendering, debounced IPC calls, glossary reconciliation, and error banners', async () => {
    vi.useFakeTimers();
    const bridge = createMockBridge({ wizard_completed: true });
    const switchSpy = vi.spyOn(bridge, 'switchTranslationProvider');
    const styleSpy = vi.spyOn(bridge, 'updateOverlayStyle');
    const glossarySpy = vi.spyOn(bridge, 'updateGlossarySettings');

    render(<App bridge={bridge} />);
    await act(async () => {
      await Promise.resolve();
    });

    // Toggle overlay lock directly on overlay and verify in settings
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Click-Through Locked',
    );
    fireEvent.click(screen.getByRole('button', { name: 'Unlock Position' }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Position Unlocked',
    );

    // Switch to settings, update provider to NLLB CUDA, debounce rapid slider changes, then switch to Mock
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

    // Rapid slider updates should be debounced into one IPC call per category
    fireEvent.change(screen.getByTestId('hotwords-score-slider'), {
      target: { value: '1.8' },
    });
    fireEvent.change(screen.getByTestId('hotwords-score-slider'), {
      target: { value: '2.2' },
    });
    expect(screen.getByTestId('hotwords-score-label')).toHaveTextContent('2.2');
    expect(glossarySpy).not.toHaveBeenCalled();

    const fontSlider = screen.getByLabelText(/Font Size/i);
    fireEvent.change(fontSlider, { target: { value: '20' } });
    fireEvent.change(fontSlider, { target: { value: '24' } });
    expect(styleSpy).not.toHaveBeenCalled();

    // Ensure returned GlossarySettingsState from backend is reconciled into UI state
    glossarySpy.mockResolvedValueOnce({
      profiles: [
        {
          id: 'sample',
          name: 'My Profile',
          src_lang: 'en',
          tgt_lang: 'ja',
          term_count: 4,
          enabled: true,
        },
      ],
      enabled_profiles: ['My Profile'],
      hotwords_score: 2.5,
      glossary_dir: 'reconciled-dir',
    });

    await act(async () => {
      vi.advanceTimersByTime(260);
      await Promise.resolve();
    });
    expect(glossarySpy).toHaveBeenCalledTimes(1);
    expect(styleSpy).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('hotwords-score-label')).toHaveTextContent('2.5');

    // Text input on provider config is debounced
    fireEvent.click(screen.getByTestId('provider-tab-mock'));
    expect(screen.getByTestId('active-engine-summary')).toHaveTextContent(
      'Mock Translator ([ja] )',
    );
    const callCountAfterTabSwitch = switchSpy.mock.calls.length;

    const prefixInput = screen.getByLabelText('Output Prefix');
    fireEvent.change(prefixInput, { target: { value: '[JP1] ' } });
    fireEvent.change(prefixInput, { target: { value: '[JP] ' } });
    expect(switchSpy).toHaveBeenCalledTimes(callCountAfterTabSwitch);

    // Surface IPC error banner on failed debounced call and allow dismissal
    switchSpy.mockRejectedValueOnce(new Error('IPC translation switch failed'));
    await act(async () => {
      vi.advanceTimersByTime(260);
      await Promise.resolve();
    });
    expect(screen.getByTestId('app-error-banner')).toHaveTextContent(
      'IPC translation switch failed',
    );
    fireEvent.click(screen.getByTestId('dismiss-error-banner-btn'));
    expect(screen.queryByTestId('app-error-banner')).not.toBeInTheDocument();

    // Switch back to overlay and verify mock prefix is used in simulated translation
    fireEvent.click(screen.getByTestId('mode-overlay-btn'));
    fireEvent.click(screen.getByTestId('simulate-utterance-btn'));

    act(() => {
      vi.advanceTimersByTime(860);
    });

    expect(
      screen.getByText('[JP] One enemy is rotating through upper tunnel.'),
    ).toBeInTheDocument();
  });

  it('renders pure transparent frameless overlay initialized with empty subtitles and syncs live style updates when ?window=overlay', async () => {
    window.history.replaceState({}, '', '/?window=overlay');
    const bridge = createMockBridge();

    render(<App bridge={bridge} />);
    // Immediately on first render before hydration completes, ?window=overlay must NOT flash dummy subtitles
    expect(screen.getByTestId('overlay-empty-state')).toBeInTheDocument();
    expect(screen.queryByTestId('subtitle-entry-101')).not.toBeInTheDocument();

    await act(async () => {
      await Promise.resolve();
    });

    // Even after mock bridge hydration resolves, ?window=overlay stays clean until real subtitles arrive
    expect(screen.getByTestId('overlay-empty-state')).toBeInTheDocument();
    expect(screen.queryByTestId('subtitle-entry-101')).not.toBeInTheDocument();

    expect(screen.getByTestId('pure-overlay-window')).toBeInTheDocument();
    expect(screen.getByTestId('subtitle-overlay')).toHaveClass('is-frameless');
    expect(screen.queryByTestId('mode-switcher')).not.toBeInTheDocument();
    expect(screen.queryByTestId('mode-settings-btn')).not.toBeInTheDocument();
    expect(screen.queryByTestId('overlay-drag-bar')).not.toBeInTheDocument();
    expect(document.body).toHaveClass('overlay-window-root');

    // Live style updates emitted from settings window immediately update overlay styles
    await act(async () => {
      await bridge.updateOverlayStyle({
        max_lines: 2,
        show_source_text: false,
        font_size_px: 24,
        background_opacity: 0.6,
      });
    });

    const overlayEl = screen.getByTestId('subtitle-overlay');
    expect(overlayEl.style.getPropertyValue('--subtitle-font-size')).toBe('24px');
    expect(overlayEl.style.getPropertyValue('--overlay-opacity')).toBe('0.6');

    // Unlocking via backend event reveals the drag handle with data-tauri-drag-region
    act(() => {
      bridge.emitOverlayLockChanged(false);
    });
    await waitFor(() => {
      expect(screen.getByTestId('overlay-drag-bar')).toHaveAttribute(
        'data-tauri-drag-region',
      );
    });
  });
});
