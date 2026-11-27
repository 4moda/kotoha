import type {
  OverlayDisplayOptions,
  SubtitleEntry,
} from '../types/kotoha';
import { getDeterministicSpeakerColor } from '../data/modelCatalog';

export interface SubtitleOverlayProps {
  entries: readonly SubtitleEntry[];
  options: OverlayDisplayOptions;
  onToggleLock?: () => void;
  frameless?: boolean;
}

export function SubtitleOverlay({
  entries,
  options,
  onToggleLock,
  frameless = false,
}: SubtitleOverlayProps) {
  const visibleEntries = entries.slice(-Math.max(1, options.max_lines));
  const showUnlockedDragHandle = !frameless || !options.locked;

  return (
    <section
      className={`subtitle-overlay ${options.locked ? 'is-locked' : 'is-unlocked'} ${
        frameless ? 'is-frameless' : ''
      }`}
      aria-label="Live subtitle overlay"
      data-testid="subtitle-overlay"
      style={{
        ['--overlay-opacity' as string]: String(options.background_opacity),
        ['--subtitle-font-size' as string]: `${options.font_size_px}px`,
      }}
    >
      {showUnlockedDragHandle && (
        <header
          className="overlay-bar"
          data-testid="overlay-drag-bar"
          {...(!options.locked ? { 'data-tauri-drag-region': '' } : {})}
        >
          <div
            className="overlay-status-group"
            {...(!options.locked ? { 'data-tauri-drag-region': '' } : {})}
          >
            <span
              className={`lock-badge ${options.locked ? 'locked' : 'unlocked'}`}
              data-testid="overlay-lock-indicator"
            >
              {options.locked ? 'Click-Through Locked' : 'Position Unlocked (Drag / Resize)'}
            </span>
            <span className="overlay-line-count" data-testid="overlay-line-count">
              {visibleEntries.length}/{options.max_lines} lines
            </span>
          </div>
          {onToggleLock && (
            <button
              type="button"
              id="toggle-overlay-lock-btn"
              className="btn btn-subtle btn-compact"
              onClick={onToggleLock}
            >
              {options.locked ? 'Unlock Position' : 'Lock Click-Through'}
            </button>
          )}
        </header>
      )}

      {visibleEntries.length === 0 ? (
        <div className="overlay-empty" data-testid="overlay-empty-state">
          Waiting for voice chat utterances…
        </div>
      ) : (
        <ul className="subtitle-list" data-testid="subtitle-list">
          {visibleEntries.map((entry) => {
            const { stage } = entry;
            const badgeColor =
              entry.speaker_color || getDeterministicSpeakerColor(entry.speaker_key);

            return (
              <li
                key={entry.utterance_id}
                className={`subtitle-card stage-${stage}`}
                data-testid={`subtitle-entry-${entry.utterance_id}`}
                data-stage={stage}
              >
                <div className="subtitle-meta">
                  <span
                    className="speaker-badge"
                    data-testid={`speaker-badge-${entry.utterance_id}`}
                    style={{
                      backgroundColor: `${badgeColor}24`,
                      borderColor: badgeColor,
                      color: badgeColor,
                    }}
                  >
                    <span
                      className="speaker-dot"
                      style={{ backgroundColor: badgeColor }}
                      aria-hidden="true"
                    />
                    {entry.speaker_name}
                  </span>
                  <span className="speaker-source">
                    {entry.speaker_key.source}#{entry.speaker_key.user_id}
                  </span>
                  <span className={`stage-tag stage-tag-${stage}`}>
                    {stage === 'interim' && 'Interim STT'}
                    {stage === 'final_pending_translation' && 'Translating…'}
                    {stage === 'translated' && 'Translated'}
                  </span>
                </div>

                {stage === 'interim' && (
                  <p className="subtitle-primary subtitle-interim">
                    {entry.src_text}
                  </p>
                )}

                {stage === 'final_pending_translation' && (
                  <p className="subtitle-primary subtitle-final-pending">
                    {entry.src_text}
                  </p>
                )}

                {stage === 'translated' && (
                  <>
                    <p className="subtitle-primary subtitle-translated">
                      {entry.translated_text}
                    </p>
                    {options.show_source_text && (
                      <p className="subtitle-secondary">{entry.src_text}</p>
                    )}
                  </>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
