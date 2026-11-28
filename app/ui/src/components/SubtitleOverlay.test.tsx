import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { getDeterministicSpeakerColor } from '../data/modelCatalog';
import type { OverlayDisplayOptions, SubtitleEntry } from '../types/kotoha';
import { SubtitleOverlay } from './SubtitleOverlay';

const baseOptions: OverlayDisplayOptions = {
  max_lines: 4,
  show_source_text: true,
  locked: true,
  font_size_px: 18,
  background_opacity: 0.78,
};

const sampleEntries: SubtitleEntry[] = [
  {
    utterance_id: 101,
    speaker_key: { source: 'mumble', user_id: 12 },
    speaker_name: 'Alex',
    speaker_color: getDeterministicSpeakerColor({ source: 'mumble', user_id: 12 }),
    src_text: 'Push mid together in ten seconds, watch the flank.',
    translated_text: '10秒後に一緒にミッドを押し上げよう、横からの奇襲に注意して。',
    stage: 'translated',
    timestamp_ms: 1700000001000,
  },
  {
    utterance_id: 102,
    speaker_key: { source: 'mumble', user_id: 27 },
    speaker_name: 'Mina',
    speaker_color: getDeterministicSpeakerColor({ source: 'mumble', user_id: 27 }),
    src_text: 'Copy that, smoking stairs now and rotating behind you.',
    translated_text: null,
    stage: 'final_pending_translation',
    timestamp_ms: 1700000002400,
  },
  {
    utterance_id: 103,
    speaker_key: { source: 'mumble', user_id: 41 },
    speaker_name: 'Ren',
    speaker_color: '',
    src_text: 'Hold on, reloading my...',
    translated_text: null,
    stage: 'interim',
    timestamp_ms: 1700000003100,
  },
];

describe('SubtitleOverlay', () => {
  it('renders 3-stage subtitle entries and deterministic speaker color badges', () => {
    render(<SubtitleOverlay entries={sampleEntries} options={baseOptions} />);

    const translatedEntry = screen.getByTestId('subtitle-entry-101');
    const finalPendingEntry = screen.getByTestId('subtitle-entry-102');
    const interimEntry = screen.getByTestId('subtitle-entry-103');

    expect(translatedEntry).toHaveAttribute('data-stage', 'translated');
    expect(within(translatedEntry).getByText('Translated')).toBeInTheDocument();
    expect(
      within(translatedEntry).getByText(
        '10秒後に一緒にミッドを押し上げよう、横からの奇襲に注意して。',
      ),
    ).toBeInTheDocument();
    expect(
      within(translatedEntry).getByText(
        'Push mid together in ten seconds, watch the flank.',
      ),
    ).toBeInTheDocument();

    expect(finalPendingEntry).toHaveAttribute('data-stage', 'final_pending_translation');
    expect(within(finalPendingEntry).getByText('Translating…')).toBeInTheDocument();
    expect(
      within(finalPendingEntry).getByText(
        'Copy that, smoking stairs now and rotating behind you.',
      ),
    ).toBeInTheDocument();

    expect(interimEntry).toHaveAttribute('data-stage', 'interim');
    expect(within(interimEntry).getByText('Interim STT')).toBeInTheDocument();
    expect(within(interimEntry).getByText('Hold on, reloading my...')).toBeInTheDocument();

    const expectedAlexColor = getDeterministicSpeakerColor({ source: 'mumble', user_id: 12 });
    const alexBadge = screen.getByTestId('speaker-badge-101');
    expect(alexBadge).toHaveTextContent('Alex');
    expect(alexBadge).toHaveStyle({ borderColor: expectedAlexColor });

    // Falls back to deterministic color when entry.speaker_color is empty
    const expectedRenColor = getDeterministicSpeakerColor({ source: 'mumble', user_id: 41 });
    const renBadge = screen.getByTestId('speaker-badge-103');
    expect(renBadge).toHaveTextContent('Ren');
    expect(renBadge).toHaveStyle({ borderColor: expectedRenColor });
  });

  it('renders translation_failed cards with source text and 翻訳失敗 label', () => {
    const failedEntry: SubtitleEntry = {
      utterance_id: 104,
      speaker_key: { source: 'mumble', user_id: 55 },
      speaker_name: 'Sora',
      speaker_color: '',
      src_text: 'Pull back to the tower, they have numbers.',
      translated_text: null,
      stage: 'translation_failed',
      timestamp_ms: 1700000004000,
      failure_reason: 'translation provider error',
    };

    render(<SubtitleOverlay entries={[failedEntry]} options={baseOptions} />);

    const card = screen.getByTestId('subtitle-entry-104');
    expect(card).toHaveAttribute('data-stage', 'translation_failed');
    expect(within(card).getByText('翻訳失敗')).toHaveClass('stage-tag');
    expect(
      within(card).getByText('Pull back to the tower, they have numbers.'),
    ).toBeInTheDocument();
    expect(
      within(card).getByText('翻訳失敗: translation provider error'),
    ).toBeInTheDocument();
  });

  it('renders translation_failed cards without a reason label suffix when reason is absent', () => {
    const failedEntry: SubtitleEntry = {
      utterance_id: 105,
      speaker_key: { source: 'mumble', user_id: 56 },
      speaker_name: 'Kai',
      speaker_color: '',
      src_text: 'Regroup at spawn.',
      translated_text: null,
      stage: 'translation_failed',
      timestamp_ms: 1700000005000,
    };

    render(<SubtitleOverlay entries={[failedEntry]} options={baseOptions} />);

    const card = screen.getByTestId('subtitle-entry-105');
    expect(within(card).getByText('Regroup at spawn.')).toBeInTheDocument();
    expect(within(card).getAllByText('翻訳失敗').length).toBeGreaterThan(0);
    expect(within(card).queryByText(/翻訳失敗:/)).not.toBeInTheDocument();
  });

  it('hides source text beneath translated subtitle when show_source_text is false', () => {
    render(
      <SubtitleOverlay
        entries={[sampleEntries[0]!]}
        options={{ ...baseOptions, show_source_text: false }}
      />,
    );

    expect(
      screen.getByText('10秒後に一緒にミッドを押し上げよう、横からの奇襲に注意して。'),
    ).toBeInTheDocument();
    expect(
      screen.queryByText('Push mid together in ten seconds, watch the flank.'),
    ).not.toBeInTheDocument();
  });

  it('slices visible entries to max_lines and shows empty state when no entries exist', () => {
    const { rerender } = render(
      <SubtitleOverlay
        entries={sampleEntries}
        options={{ ...baseOptions, max_lines: 2 }}
      />,
    );

    expect(screen.getByTestId('overlay-line-count')).toHaveTextContent('2/2 lines');
    expect(screen.queryByTestId('subtitle-entry-101')).not.toBeInTheDocument();
    expect(screen.getByTestId('subtitle-entry-102')).toBeInTheDocument();
    expect(screen.getByTestId('subtitle-entry-103')).toBeInTheDocument();

    rerender(<SubtitleOverlay entries={[]} options={baseOptions} />);
    expect(screen.getByTestId('overlay-line-count')).toHaveTextContent('0/4 lines');
    expect(screen.getByTestId('overlay-empty-state')).toHaveTextContent(
      'Waiting for voice chat utterances…',
    );
  });

  it('renders lock/unlock indicator and invokes onToggleLock when clicked', () => {
    const onToggleLock = vi.fn();
    const { rerender } = render(
      <SubtitleOverlay
        entries={sampleEntries}
        options={baseOptions}
        onToggleLock={onToggleLock}
      />,
    );

    expect(screen.getByTestId('subtitle-overlay')).toHaveClass('is-locked');
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Click-Through Locked',
    );

    fireEvent.click(screen.getByRole('button', { name: 'Unlock Position' }));
    expect(onToggleLock).toHaveBeenCalledTimes(1);

    rerender(
      <SubtitleOverlay
        entries={sampleEntries}
        options={{ ...baseOptions, locked: false }}
        onToggleLock={onToggleLock}
      />,
    );

    expect(screen.getByTestId('subtitle-overlay')).toHaveClass('is-unlocked');
    expect(screen.getByTestId('overlay-lock-indicator')).toHaveTextContent(
      'Position Unlocked (Drag / Resize)',
    );
    expect(screen.getByRole('button', { name: 'Lock Click-Through' })).toBeInTheDocument();
  });

  it('hides drag bar in frameless mode while locked and exposes data-tauri-drag-region when unlocked', () => {
    const { rerender } = render(
      <SubtitleOverlay
        entries={sampleEntries}
        options={{ ...baseOptions, locked: true }}
        frameless={true}
      />,
    );

    expect(screen.getByTestId('subtitle-overlay')).toHaveClass('is-frameless');
    expect(screen.queryByTestId('overlay-drag-bar')).not.toBeInTheDocument();

    rerender(
      <SubtitleOverlay
        entries={sampleEntries}
        options={{ ...baseOptions, locked: false }}
        frameless={true}
      />,
    );

    const dragBar = screen.getByTestId('overlay-drag-bar');
    expect(dragBar).toBeInTheDocument();
    expect(dragBar).toHaveAttribute('data-tauri-drag-region');
  });
});
