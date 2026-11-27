import { useEffect, useMemo, useRef, useState } from 'react';
import { SubtitleOverlay } from './components/SubtitleOverlay';
import { TranslationSettings } from './components/TranslationSettings';
import {
  createDefaultProviderConfig,
  findModelProfile,
  getDeterministicSpeakerColor,
} from './data/modelCatalog';
import type {
  ConnectionStatus,
  OverlayDisplayOptions,
  SubtitleEntry,
  TranslationProviderConfig,
} from './types/kotoha';

type AppViewMode = 'overlay' | 'settings';

const INITIAL_SUBTITLES: SubtitleEntry[] = [
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
    speaker_color: getDeterministicSpeakerColor({ source: 'mumble', user_id: 41 }),
    src_text: 'Hold on, reloading my...',
    translated_text: null,
    stage: 'interim',
    timestamp_ms: 1700000003100,
  },
];

const DEMO_SEQUENCE: ReadonlyArray<{
  speaker_id: number;
  speaker_name: string;
  interim: string;
  final: string;
  ja: string;
}> = [
  {
    speaker_id: 12,
    speaker_name: 'Alex',
    interim: 'One enemy is...',
    final: 'One enemy is rotating through upper tunnel.',
    ja: '敵が1人アッパートンネルから回ってきている。',
  },
  {
    speaker_id: 27,
    speaker_name: 'Mina',
    interim: 'Nice callout, I got...',
    final: 'Nice callout, I got your crossfire covered.',
    ja: 'ナイス報告、クロスファイアは任せて。',
  },
];

export function App() {
  const [mode, setMode] = useState<AppViewMode>('overlay');
  const [srcLang, setSrcLang] = useState('en');
  const [tgtLang, setTgtLang] = useState('ja');
  const [provider, setProvider] = useState<TranslationProviderConfig>(() =>
    createDefaultProviderConfig('local_model'),
  );
  const [overlayOptions, setOverlayOptions] = useState<OverlayDisplayOptions>({
    max_lines: 4,
    show_source_text: true,
    locked: true,
    font_size_px: 18,
    background_opacity: 0.78,
  });
  const [subtitles, setSubtitles] = useState<SubtitleEntry[]>(INITIAL_SUBTITLES);
  const [connection] = useState<ConnectionStatus>({
    state: 'connected',
    source: 'mumble',
    endpoint: '$XDG_RUNTIME_DIR/kotoha_v1.sock',
    active_speakers: 3,
    dropped_frames: 0,
    last_heartbeat_ms: 120,
  });
  const [demoRunning, setDemoRunning] = useState(false);

  const nextUtteranceRef = useRef(104);
  const demoStepRef = useRef(0);
  const timersRef = useRef<number[]>([]);

  useEffect(() => {
    return () => {
      for (const id of timersRef.current) {
        window.clearTimeout(id);
      }
      timersRef.current = [];
    };
  }, []);

  const activeEngineSummary = useMemo(() => {
    if (provider.type === 'local_model') {
      const profile = findModelProfile(provider.model_id);
      return `${profile.display_name} · ${provider.device.toUpperCase()}`;
    }
    if (provider.type === 'openai_compatible') {
      return `OpenAI-Compatible (${provider.model})`;
    }
    if (provider.type === 'deepl') {
      return `DeepL (${provider.use_free_endpoint ? 'FREE' : 'PRO'})`;
    }
    if (provider.type === 'google_cloud') {
      return `Google Cloud (${provider.project_id ? 'v3' : 'v2'})`;
    }
    if (provider.type === 'gas_webhook') {
      return 'GAS / Custom HTTP Proxy';
    }
    return `Mock Translator (${provider.prefix})`;
  }, [provider]);

  const handleRunLifecycleDemo = () => {
    if (demoRunning) return;
    setDemoRunning(true);

    const item = DEMO_SEQUENCE[demoStepRef.current % DEMO_SEQUENCE.length]!;
    demoStepRef.current += 1;

    const utteranceId = nextUtteranceRef.current;
    nextUtteranceRef.current += 1;
    const speakerKey = { source: 'mumble', user_id: item.speaker_id };
    const speakerColor = getDeterministicSpeakerColor(speakerKey);
    const now = Date.now();

    setSubtitles((prev) => [
      ...prev,
      {
        utterance_id: utteranceId,
        speaker_key: speakerKey,
        speaker_name: item.speaker_name,
        speaker_color: speakerColor,
        src_text: item.interim,
        translated_text: null,
        stage: 'interim',
        timestamp_ms: now,
      },
    ]);

    const t1 = window.setTimeout(() => {
      setSubtitles((prev) =>
        prev.map((entry) =>
          entry.utterance_id === utteranceId
            ? {
                ...entry,
                src_text: item.final,
                stage: 'final_pending_translation',
              }
            : entry,
        ),
      );
    }, 350);

    const t2 = window.setTimeout(() => {
      setSubtitles((prev) =>
        prev.map((entry) =>
          entry.utterance_id === utteranceId
            ? {
                ...entry,
                translated_text:
                  provider.type === 'mock'
                    ? `${provider.prefix}${item.final}`
                    : item.ja,
                stage: 'translated',
              }
            : entry,
        ),
      );
      setDemoRunning(false);
    }, 800);

    timersRef.current.push(t1, t2);
  };

  const handleResetDemo = () => {
    for (const id of timersRef.current) {
      window.clearTimeout(id);
    }
    timersRef.current = [];
    setDemoRunning(false);
    setSubtitles(INITIAL_SUBTITLES);
  };

  return (
    <div className="app-shell">
      <header className="app-header">
        <div className="brand-block">
          <div className="brand-mark" aria-hidden="true">
            言
          </div>
          <div>
            <h1 className="app-title">Kotoha</h1>
            <p className="app-subtitle">
              Real-Time Voice Chat Transcription &amp; Translation Overlay
            </p>
          </div>
        </div>

        <div className="header-controls">
          <div className="ipc-pill" data-testid="ipc-status-pill">
            <span className={`ipc-dot status-${connection.state}`} aria-hidden="true" />
            <span>
              IPC {connection.state} ({connection.source})
            </span>
            <span className="ipc-meta">
              {connection.active_speakers} speakers · {connection.dropped_frames} drops
            </span>
          </div>

          <nav className="mode-switcher" aria-label="Workspace View">
            <button
              type="button"
              id="mode-overlay-btn"
              data-testid="mode-overlay-btn"
              className={`mode-btn ${mode === 'overlay' ? 'is-active' : ''}`}
              onClick={() => setMode('overlay')}
            >
              Overlay Preview
            </button>
            <button
              type="button"
              id="mode-settings-btn"
              data-testid="mode-settings-btn"
              className={`mode-btn ${mode === 'settings' ? 'is-active' : ''}`}
              onClick={() => setMode('settings')}
            >
              Settings &amp; Setup
            </button>
          </nav>
        </div>
      </header>

      <main className="app-main">
        <div className="status-strip">
          <span>
            Pipeline: <strong>{srcLang.toUpperCase()} → {tgtLang.toUpperCase()}</strong>
          </span>
          <span>
            Translator: <strong data-testid="active-engine-summary">{activeEngineSummary}</strong>
          </span>
          <div className="demo-actions">
            <button
              type="button"
              id="simulate-utterance-btn"
              data-testid="simulate-utterance-btn"
              className="btn btn-primary"
              onClick={handleRunLifecycleDemo}
              disabled={demoRunning}
            >
              {demoRunning ? 'Streaming 3-Stage Utterance…' : 'Simulate 3-Stage Utterance'}
            </button>
            <button
              type="button"
              id="reset-demo-btn"
              data-testid="reset-demo-btn"
              className="btn btn-subtle"
              onClick={handleResetDemo}
            >
              Reset Demo
            </button>
          </div>
        </div>

        {mode === 'overlay' ? (
          <div className="overlay-stage" data-testid="overlay-preview-stage">
            <div className="overlay-backdrop-caption">
              Simulated Game / Voice Chat Surface (Phase 1 Browser Preview — Tauri click-through
              host integrates in Phase 3)
            </div>
            <SubtitleOverlay
              entries={subtitles}
              options={overlayOptions}
              onToggleLock={() =>
                setOverlayOptions((prev) => ({ ...prev, locked: !prev.locked }))
              }
            />
          </div>
        ) : (
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
        )}
      </main>
    </div>
  );
}

export default App;
