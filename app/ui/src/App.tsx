import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { SetupWizard } from './components/SetupWizard';
import { SubtitleOverlay } from './components/SubtitleOverlay';
import { TranslationSettings } from './components/TranslationSettings';
import {
  createDefaultProviderConfig,
  findModelProfile,
  getDeterministicSpeakerColor,
} from './data/modelCatalog';
import {
  DEFAULT_GLOSSARY_PROFILES,
  DEFAULT_OVERLAY_OPTIONS,
  EMPTY_SECRET_STATUS,
  INITIAL_SUBTITLES,
  getKotohaBridge,
  isTauriRuntime,
  type KotohaBridge,
} from './services/tauriBridge';
import type {
  ConnectionStatus,
  DownloadProgressEvent,
  GlossarySettingsState,
  HostOsPlatform,
  OverlayDisplayOptions,
  ProviderSecretKind,
  SecretStatus,
  SubtitleEntry,
  TranslationProviderConfig,
  TranslatorStatus,
} from './types/kotoha';

export type AppViewMode = 'overlay' | 'wizard' | 'settings';
export type WindowKind = 'overlay' | 'settings';

export interface AppProps {
  bridge?: KotohaBridge;
}

const SETTINGS_DEBOUNCE_MS = 250;

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

function resolveWindowKind(): WindowKind {
  if (typeof window === 'undefined') {
    return 'settings';
  }
  const param = new URLSearchParams(window.location.search).get('window');
  return param === 'overlay' ? 'overlay' : 'settings';
}

function upsertSubtitle(entries: SubtitleEntry[], incoming: SubtitleEntry): SubtitleEntry[] {
  const idx = entries.findIndex((item) => item.utterance_id === incoming.utterance_id);
  if (idx === -1) {
    return [...entries, incoming];
  }
  const next = [...entries];
  next[idx] = incoming;
  return next;
}

function formatErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.trim().length > 0) {
    return error.message;
  }
  if (typeof error === 'string' && error.trim().length > 0) {
    return error;
  }
  return fallback;
}

export function App({ bridge: providedBridge }: AppProps = {}) {
  const bridge = useMemo(() => providedBridge ?? getKotohaBridge(), [providedBridge]);
  const windowKind = resolveWindowKind();
  const isPureOverlayWindow = windowKind === 'overlay';
  const defaultPreviewSubtitles = useMemo(
    () => (isPureOverlayWindow || isTauriRuntime() ? [] : INITIAL_SUBTITLES),
    [isPureOverlayWindow],
  );

  const [mode, setMode] = useState<AppViewMode>('overlay');
  const [srcLang, setSrcLang] = useState('en');
  const [tgtLang, setTgtLang] = useState('ja');
  const [provider, setProvider] = useState<TranslationProviderConfig>(() =>
    createDefaultProviderConfig('local_model'),
  );
  const [secrets, setSecrets] = useState<SecretStatus>(() => ({ ...EMPTY_SECRET_STATUS }));
  const [translatorStatus, setTranslatorStatus] = useState<TranslatorStatus>({ state: 'ok' });
  const [settingsNotice, setSettingsNotice] = useState<string | null>(null);
  const [overlayOptions, setOverlayOptions] = useState<OverlayDisplayOptions>(
    DEFAULT_OVERLAY_OPTIONS,
  );
  const [glossarySettings, setGlossarySettings] = useState<GlossarySettingsState>({
    profiles: DEFAULT_GLOSSARY_PROFILES,
    enabled_profiles: DEFAULT_GLOSSARY_PROFILES.filter((p) => p.enabled).map((p) => p.name),
    hotwords_score: 1.5,
    glossary_dir: 'glossaries',
  });
  const [subtitles, setSubtitles] = useState<SubtitleEntry[]>(() => defaultPreviewSubtitles);
  const [connection, setConnection] = useState<ConnectionStatus>({
    state: 'connected',
    source: 'mumble',
    plugin_version: '0.1.0',
    endpoint: '$XDG_RUNTIME_DIR/kotoha_v1.sock',
    active_speakers: 3,
    dropped_frames: 0,
    last_heartbeat_ms: 120,
  });
  const [wizardCompleted, setWizardCompleted] = useState(false);
  const [platform, setPlatform] = useState<HostOsPlatform>('linux');
  const [detectedMumblePaths, setDetectedMumblePaths] = useState<string[]>([
    '~/.local/share/Mumble/Mumble/Plugins',
    '~/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins',
  ]);
  const [isFlatpakMumble, setIsFlatpakMumble] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState<DownloadProgressEvent | null>(null);
  const [demoRunning, setDemoRunning] = useState(false);
  const [statusError, setStatusError] = useState<string | null>(null);

  const nextUtteranceRef = useRef(104);
  const demoStepRef = useRef(0);
  const timersRef = useRef<number[]>([]);
  const userNavigatedRef = useRef(false);
  const isMountedRef = useRef(true);
  const providerDebounceTimerRef = useRef<number | null>(null);
  const overlayStyleDebounceTimerRef = useRef<number | null>(null);
  const glossaryDebounceTimerRef = useRef<number | null>(null);
  const latestProviderRequestRef = useRef(0);
  const pendingProviderSyncRef = useRef<{
    config: TranslationProviderConfig;
    src: string;
    tgt: string;
  } | null>(null);
  const inflightProviderCommitRef = useRef<Promise<void>>(Promise.resolve());
  const latestOverlayStyleRequestRef = useRef(0);
  const latestGlossaryRequestRef = useRef(0);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  const reportIpcError = useCallback((error: unknown, fallback: string) => {
    if (!isMountedRef.current) return;
    setStatusError(formatErrorMessage(error, fallback));
  }, []);

  const navigateMode = useCallback((nextMode: AppViewMode) => {
    userNavigatedRef.current = true;
    setMode(nextMode);
  }, []);

  useEffect(() => {
    if (typeof document === 'undefined') {
      return;
    }
    if (isPureOverlayWindow) {
      document.documentElement.classList.add('overlay-window-root');
      document.body.classList.add('overlay-window-root');
    } else {
      document.documentElement.classList.remove('overlay-window-root');
      document.body.classList.remove('overlay-window-root');
    }
    return () => {
      document.documentElement.classList.remove('overlay-window-root');
      document.body.classList.remove('overlay-window-root');
    };
  }, [isPureOverlayWindow]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | null = null;

    void bridge
      .getAppState()
      .then((state) => {
        if (!active) return;
        setSrcLang(state.src_lang);
        setTgtLang(state.tgt_lang);
        setProvider(state.provider);
        setSecrets(state.secrets);
        setTranslatorStatus(state.translator_status ?? { state: 'ok' });
        setSettingsNotice(state.settings_notice ?? null);
        setOverlayOptions(state.overlay_options);
        setGlossarySettings(state.glossary);
        setConnection(state.connection);
        setSubtitles(
          isPureOverlayWindow
            ? bridge.isTauri
              ? state.subtitles
              : state.subtitles.filter(
                  (entry) => !INITIAL_SUBTITLES.some((demo) => demo.utterance_id === entry.utterance_id),
                )
            : !bridge.isTauri && !isTauriRuntime() && state.subtitles.length === 0
              ? INITIAL_SUBTITLES
              : state.subtitles,
        );
        setWizardCompleted(state.wizard_completed);
        setPlatform(state.platform);
        setDetectedMumblePaths(state.detected_mumble_paths);
        setIsFlatpakMumble(state.is_flatpak_mumble);

        if (!isPureOverlayWindow && state.wizard_completed === false && !userNavigatedRef.current) {
          setMode('wizard');
        }
      })
      .catch((error: unknown) => {
        reportIpcError(error, 'アプリ状態の読み込みに失敗しました。');
      });

    void bridge
      .subscribeEvents({
        onSubtitle: (entry) => {
          if (!active) return;
          setSubtitles((prev) => upsertSubtitle(prev, entry));
        },
        onConnectionStatus: (status) => {
          if (!active) return;
          setConnection(status);
        },
        onDownloadProgress: (progress) => {
          if (!active) return;
          setDownloadProgress(progress);
        },
        onOverlayLockChanged: (locked) => {
          if (!active) return;
          setOverlayOptions((prev) => ({ ...prev, locked }));
        },
        onOverlayStyleChanged: (style) => {
          if (!active) return;
          setOverlayOptions(style);
        },
      })
      .then((fn) => {
        if (!active) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error: unknown) => {
        reportIpcError(error, 'リアルタイムイベントの購読に失敗しました。');
      });

    return () => {
      active = false;
      unlisten?.();
    };
  }, [bridge, isPureOverlayWindow, reportIpcError]);

  useEffect(() => {
    return () => {
      for (const id of timersRef.current) {
        window.clearTimeout(id);
      }
      timersRef.current = [];

      if (providerDebounceTimerRef.current !== null) {
        window.clearTimeout(providerDebounceTimerRef.current);
        providerDebounceTimerRef.current = null;
      }
      if (overlayStyleDebounceTimerRef.current !== null) {
        window.clearTimeout(overlayStyleDebounceTimerRef.current);
        overlayStyleDebounceTimerRef.current = null;
      }
      if (glossaryDebounceTimerRef.current !== null) {
        window.clearTimeout(glossaryDebounceTimerRef.current);
        glossaryDebounceTimerRef.current = null;
      }
    };
  }, []);

  const commitTranslationProvider = useCallback(
    (nextConfig: TranslationProviderConfig, nextSrc: string, nextTgt: string) => {
      const requestId = ++latestProviderRequestRef.current;
      const request = bridge
        .switchTranslationProvider(nextConfig, nextSrc, nextTgt)
        .then((state) => {
          if (isMountedRef.current && latestProviderRequestRef.current === requestId) {
            // Switching provider / origin may have deleted stored secrets on the Rust side.
            setSecrets(state.secrets);
            setTranslatorStatus(state.translator_status ?? { state: 'ok' });
            setStatusError(null);
          }
        })
        .catch((error: unknown) => {
          if (latestProviderRequestRef.current === requestId) {
            reportIpcError(error, '翻訳エンジン設定の保存に失敗しました。');
          }
        });
      inflightProviderCommitRef.current = request;
    },
    [bridge, reportIpcError],
  );

  const scheduleTranslationProviderSync = useCallback(
    (
      nextConfig: TranslationProviderConfig,
      nextSrc: string,
      nextTgt: string,
      immediate: boolean,
    ) => {
      if (providerDebounceTimerRef.current !== null) {
        window.clearTimeout(providerDebounceTimerRef.current);
        providerDebounceTimerRef.current = null;
      }
      if (immediate) {
        pendingProviderSyncRef.current = null;
        commitTranslationProvider(nextConfig, nextSrc, nextTgt);
        return;
      }
      pendingProviderSyncRef.current = { config: nextConfig, src: nextSrc, tgt: nextTgt };
      providerDebounceTimerRef.current = window.setTimeout(() => {
        providerDebounceTimerRef.current = null;
        pendingProviderSyncRef.current = null;
        commitTranslationProvider(nextConfig, nextSrc, nextTgt);
      }, SETTINGS_DEBOUNCE_MS);
    },
    [commitTranslationProvider],
  );

  /** Saves any debounced provider edit now and waits for in-flight saves to finish. */
  const flushProviderSync = useCallback(async () => {
    if (providerDebounceTimerRef.current !== null) {
      window.clearTimeout(providerDebounceTimerRef.current);
      providerDebounceTimerRef.current = null;
    }
    const pending = pendingProviderSyncRef.current;
    pendingProviderSyncRef.current = null;
    if (pending) {
      commitTranslationProvider(pending.config, pending.src, pending.tgt);
    }
    await inflightProviderCommitRef.current;
  }, [commitTranslationProvider]);

  /** Re-reads the backend translator status (it changes when a secret rebuilds the provider). */
  const refreshTranslatorStatus = useCallback(async () => {
    try {
      const state = await bridge.getAppState();
      if (isMountedRef.current) {
        setTranslatorStatus(state.translator_status ?? { state: 'ok' });
      }
    } catch {
      // Best effort: the previous status stays until the next successful refresh.
    }
  }, [bridge]);

  const handleSetSecret = useCallback(
    async (kind: ProviderSecretKind, value: string) => {
      try {
        // Save the provider (and base_url) first so the key is not invalidated by a later sync.
        await flushProviderSync();
        const status = await bridge.setProviderSecret(kind, value);
        if (isMountedRef.current) {
          setSecrets(status);
          setStatusError(null);
        }
        await refreshTranslatorStatus();
      } catch (error) {
        reportIpcError(error, 'シークレットの保存に失敗しました。');
        throw error;
      }
    },
    [bridge, flushProviderSync, refreshTranslatorStatus, reportIpcError],
  );

  const handleClearSecret = useCallback(
    async (kind: ProviderSecretKind) => {
      try {
        await flushProviderSync();
        const status = await bridge.clearProviderSecret(kind);
        if (isMountedRef.current) {
          setSecrets(status);
          setStatusError(null);
        }
        await refreshTranslatorStatus();
      } catch (error) {
        reportIpcError(error, 'シークレットの削除に失敗しました。');
        throw error;
      }
    },
    [bridge, flushProviderSync, refreshTranslatorStatus, reportIpcError],
  );

  const commitOverlayStyle = useCallback(
    (nextOptions: OverlayDisplayOptions) => {
      const requestId = ++latestOverlayStyleRequestRef.current;
      void bridge
        .updateOverlayStyle(nextOptions)
        .then((updated) => {
          if (!isMountedRef.current || latestOverlayStyleRequestRef.current !== requestId) {
            return;
          }
          setOverlayOptions(updated);
          setStatusError(null);
        })
        .catch((error: unknown) => {
          if (latestOverlayStyleRequestRef.current === requestId) {
            reportIpcError(error, 'オーバーレイ表示設定の更新に失敗しました。');
          }
        });
    },
    [bridge, reportIpcError],
  );

  const commitGlossarySettings = useCallback(
    (next: { enabled_profiles: string[]; hotwords_score: number }, glossaryDir: string | null) => {
      const requestId = ++latestGlossaryRequestRef.current;
      void bridge
        .updateGlossarySettings({
          enabled_profiles: next.enabled_profiles,
          hotwords_score: next.hotwords_score,
          glossary_dir: glossaryDir,
        })
        .then((updated) => {
          if (!isMountedRef.current || latestGlossaryRequestRef.current !== requestId) {
            return;
          }
          setGlossarySettings(updated);
          setStatusError(null);
        })
        .catch((error: unknown) => {
          if (latestGlossaryRequestRef.current === requestId) {
            reportIpcError(error, 'カスタム辞書 / Hotwords 設定の更新に失敗しました。');
          }
        });
    },
    [bridge, reportIpcError],
  );

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

  const handleToggleLock = () => {
    const nextLocked = !overlayOptions.locked;
    setOverlayOptions((prev) => ({ ...prev, locked: nextLocked }));
    void bridge
      .setOverlayLocked(nextLocked)
      .then((updated) => {
        if (!isMountedRef.current) return;
        setOverlayOptions(updated);
        setStatusError(null);
      })
      .catch((error: unknown) => {
        reportIpcError(error, 'オーバーレイのロック切り替えに失敗しました。');
      });
  };

  const handleUpdateLanguages = (nextSrc: string, nextTgt: string) => {
    setSrcLang(nextSrc);
    setTgtLang(nextTgt);
    scheduleTranslationProviderSync(provider, nextSrc, nextTgt, true);
  };

  const handleUpdateProvider = (nextConfig: TranslationProviderConfig) => {
    const isDiscreteSwitch = nextConfig.type !== provider.type;
    const isLocalDiscreteChange =
      nextConfig.type === 'local_model' &&
      provider.type === 'local_model' &&
      (nextConfig.model_id !== provider.model_id || nextConfig.device !== provider.device);
    const isDeeplTierChange =
      nextConfig.type === 'deepl' &&
      provider.type === 'deepl' &&
      nextConfig.use_free_endpoint !== provider.use_free_endpoint;
    const immediate = isDiscreteSwitch || isLocalDiscreteChange || isDeeplTierChange;

    setProvider(nextConfig);
    scheduleTranslationProviderSync(nextConfig, srcLang, tgtLang, immediate);
  };

  const handleUpdateOverlayOptions = (nextOptions: OverlayDisplayOptions) => {
    const isDiscreteToggle =
      nextOptions.locked !== overlayOptions.locked ||
      nextOptions.show_source_text !== overlayOptions.show_source_text;

    setOverlayOptions(nextOptions);
    if (overlayStyleDebounceTimerRef.current !== null) {
      window.clearTimeout(overlayStyleDebounceTimerRef.current);
      overlayStyleDebounceTimerRef.current = null;
    }
    if (isDiscreteToggle) {
      commitOverlayStyle(nextOptions);
      return;
    }
    overlayStyleDebounceTimerRef.current = window.setTimeout(() => {
      overlayStyleDebounceTimerRef.current = null;
      commitOverlayStyle(nextOptions);
    }, SETTINGS_DEBOUNCE_MS);
  };

  const handleUpdateGlossarySettings = (next: {
    enabled_profiles: string[];
    hotwords_score: number;
  }) => {
    const profilesChanged =
      next.enabled_profiles.length !== glossarySettings.enabled_profiles.length ||
      next.enabled_profiles.some(
        (profile, index) => profile !== glossarySettings.enabled_profiles[index],
      );
    const enabledSet = new Set(next.enabled_profiles);

    setGlossarySettings((prev) => ({
      ...prev,
      enabled_profiles: next.enabled_profiles,
      hotwords_score: next.hotwords_score,
      profiles: prev.profiles.map((p) => ({
        ...p,
        enabled: enabledSet.has(p.name) || enabledSet.has(p.id),
      })),
    }));

    if (glossaryDebounceTimerRef.current !== null) {
      window.clearTimeout(glossaryDebounceTimerRef.current);
      glossaryDebounceTimerRef.current = null;
    }
    if (profilesChanged) {
      commitGlossarySettings(next, glossarySettings.glossary_dir);
      return;
    }
    glossaryDebounceTimerRef.current = window.setTimeout(() => {
      glossaryDebounceTimerRef.current = null;
      commitGlossarySettings(next, glossarySettings.glossary_dir);
    }, SETTINGS_DEBOUNCE_MS);
  };

  const handleDownloadModel = async (modelId: string) => {
    const result = await bridge.downloadModelProfile(modelId);
    setDownloadProgress(result);
    if (provider.type === 'local_model') {
      const nextConfig: TranslationProviderConfig = {
        ...provider,
        model_id: modelId,
      };
      setProvider(nextConfig);
      const switched = await bridge.switchTranslationProvider(nextConfig, srcLang, tgtLang);
      if (isMountedRef.current) {
        // The downloaded model may now be loadable: drop a stale "not active" banner.
        setSecrets(switched.secrets);
        setTranslatorStatus(switched.translator_status ?? { state: 'ok' });
      }
    }
    return result;
  };

  const handleSkipToApiTranslation = (config?: TranslationProviderConfig) => {
    const nextConfig = config ?? createDefaultProviderConfig('openai_compatible');
    setProvider(nextConfig);
    scheduleTranslationProviderSync(nextConfig, srcLang, tgtLang, true);
  };

  const handleCompleteWizard = async () => {
    const updated = await bridge.completeFirstRunWizard();
    setWizardCompleted(updated.wizard_completed);
    setConnection(updated.connection);
    setStatusError(null);
    userNavigatedRef.current = true;
    setMode('overlay');
  };

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
    setSubtitles(defaultPreviewSubtitles);
  };

  if (isPureOverlayWindow) {
    return (
      <main className="pure-overlay-shell" data-testid="pure-overlay-window">
        <SubtitleOverlay
          entries={subtitles}
          options={overlayOptions}
          frameless={true}
        />
      </main>
    );
  }

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
              onClick={() => navigateMode('overlay')}
            >
              Overlay Preview
            </button>
            <button
              type="button"
              id="mode-wizard-btn"
              data-testid="mode-wizard-btn"
              className={`mode-btn ${mode === 'wizard' ? 'is-active' : ''}`}
              onClick={() => navigateMode('wizard')}
            >
              Setup Wizard
            </button>
            <button
              type="button"
              id="mode-settings-btn"
              data-testid="mode-settings-btn"
              className={`mode-btn ${mode === 'settings' ? 'is-active' : ''}`}
              onClick={() => navigateMode('settings')}
            >
              Settings
            </button>
          </nav>
        </div>
      </header>

      <main className="app-main">
        {statusError && (
          <div
            className="app-error-banner"
            data-testid="app-error-banner"
            role="alert"
          >
            <span className="app-error-message">{statusError}</span>
            <button
              type="button"
              className="btn btn-subtle btn-compact"
              data-testid="dismiss-error-banner-btn"
              onClick={() => setStatusError(null)}
            >
              Dismiss
            </button>
          </div>
        )}

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

        {mode === 'overlay' && (
          <div className="overlay-stage" data-testid="overlay-preview-stage">
            <div className="overlay-backdrop-caption">
              Simulated Game / Voice Chat Surface (Tauri v2 Overlay Preview —{' '}
              {bridge.isTauri ? 'Tauri IPC Connected' : 'Browser Preview Mode'})
            </div>
            <SubtitleOverlay
              entries={subtitles}
              options={overlayOptions}
              onToggleLock={handleToggleLock}
            />
          </div>
        )}

        {mode === 'wizard' && (
          <SetupWizard
            platform={platform}
            detectedMumblePaths={detectedMumblePaths}
            isFlatpakMumble={isFlatpakMumble}
            connection={connection}
            selectedModelId={
              provider.type === 'local_model' ? provider.model_id : 'fugumt-en-ja-int8'
            }
            downloadProgress={downloadProgress}
            wizardCompleted={wizardCompleted}
            onInstallPlugin={(customDir) => bridge.installMumblePlugin(customDir)}
            onDownloadModel={handleDownloadModel}
            onSkipToApiTranslation={handleSkipToApiTranslation}
            onCompleteWizard={handleCompleteWizard}
          />
        )}

        {mode === 'settings' && (
          <TranslationSettings
            srcLang={srcLang}
            tgtLang={tgtLang}
            provider={provider}
            secrets={secrets}
            translatorStatus={translatorStatus}
            settingsNotice={settingsNotice}
            onDismissSettingsNotice={() => setSettingsNotice(null)}
            overlayOptions={overlayOptions}
            glossarySettings={glossarySettings}
            onChangeLanguages={handleUpdateLanguages}
            onChangeProvider={handleUpdateProvider}
            onSetSecret={handleSetSecret}
            onClearSecret={handleClearSecret}
            onChangeOverlayOptions={handleUpdateOverlayOptions}
            onChangeGlossarySettings={handleUpdateGlossarySettings}
          />
        )}
      </main>
    </div>
  );
}

export default App;
