import {
  createDefaultProviderConfig,
  findModelProfile,
  getDeterministicSpeakerColor,
} from '../data/modelCatalog';
import type {
  AppRuntimeState,
  ConnectionStatus,
  DownloadProgressEvent,
  GlossaryProfileSummary,
  GlossarySettingsState,
  OverlayDisplayOptions,
  PluginInstallResult,
  SubtitleEntry,
  TranslationProviderConfig,
} from '../types/kotoha';

export const TAURI_EVENTS = {
  SUBTITLE: 'kotoha://subtitle',
  CONNECTION_STATUS: 'kotoha://connection-status',
  DOWNLOAD_PROGRESS: 'kotoha://download-progress',
  OVERLAY_LOCK_CHANGED: 'kotoha://overlay-lock-changed',
  OVERLAY_STYLE_CHANGED: 'kotoha://overlay-style-changed',
} as const;

export const INITIAL_SUBTITLES: SubtitleEntry[] = [
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

export const DEFAULT_GLOSSARY_PROFILES: GlossaryProfileSummary[] = [
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
];

export const DEFAULT_OVERLAY_OPTIONS: OverlayDisplayOptions = {
  max_lines: 4,
  show_source_text: true,
  locked: true,
  font_size_px: 18,
  background_opacity: 0.78,
};

export interface GlossaryUpdateInput {
  enabled_profiles: string[];
  hotwords_score: number;
  glossary_dir?: string | null;
}

export interface OverlayStyleUpdateInput {
  max_lines?: number;
  show_source_text?: boolean;
  locked?: boolean;
  font_size_px?: number;
  background_opacity?: number;
}

export interface KotohaEventHandlers {
  onSubtitle?: (entry: SubtitleEntry) => void;
  onConnectionStatus?: (status: ConnectionStatus) => void;
  onDownloadProgress?: (progress: DownloadProgressEvent) => void;
  onOverlayLockChanged?: (locked: boolean) => void;
  onOverlayStyleChanged?: (style: OverlayDisplayOptions) => void;
}

export interface KotohaBridge {
  readonly isTauri: boolean;
  getAppState(): Promise<AppRuntimeState>;
  setOverlayLocked(locked: boolean): Promise<OverlayDisplayOptions>;
  toggleOverlayVisible(visible?: boolean): Promise<boolean>;
  installMumblePlugin(customDir?: string): Promise<PluginInstallResult>;
  downloadModelProfile(modelId: string): Promise<DownloadProgressEvent>;
  switchTranslationProvider(
    config: TranslationProviderConfig,
    srcLang?: string,
    tgtLang?: string,
  ): Promise<AppRuntimeState>;
  updateGlossarySettings(input: GlossaryUpdateInput): Promise<GlossarySettingsState>;
  updateOverlayStyle(style: OverlayStyleUpdateInput): Promise<OverlayDisplayOptions>;
  completeFirstRunWizard(): Promise<AppRuntimeState>;
  subscribeEvents(handlers: KotohaEventHandlers): Promise<() => void>;
}

export function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function cloneState<T>(value: T): T {
  if (typeof structuredClone === 'function') {
    return structuredClone(value);
  }
  return JSON.parse(JSON.stringify(value)) as T;
}

function createInitialMockState(): AppRuntimeState {
  return {
    src_lang: 'en',
    tgt_lang: 'ja',
    provider: createDefaultProviderConfig('local_model'),
    overlay_options: { ...DEFAULT_OVERLAY_OPTIONS },
    overlay_visible: true,
    glossary: {
      profiles: cloneState(DEFAULT_GLOSSARY_PROFILES),
      enabled_profiles: DEFAULT_GLOSSARY_PROFILES.filter((p) => p.enabled).map((p) => p.name),
      hotwords_score: 1.5,
      glossary_dir: 'glossaries',
    },
    connection: {
      state: 'connected',
      source: 'mumble',
      plugin_version: '0.1.0',
      endpoint: '$XDG_RUNTIME_DIR/kotoha_v1.sock',
      active_speakers: 3,
      dropped_frames: 0,
      last_heartbeat_ms: 120,
    },
    subtitles: cloneState(INITIAL_SUBTITLES),
    wizard_completed: false,
    platform: 'linux',
    detected_mumble_paths: [
      '~/.local/share/Mumble/Mumble/Plugins',
      '~/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins',
    ],
    is_flatpak_mumble: false,
  };
}

export function createMockBridge(
  initialOverrides?: Partial<AppRuntimeState>,
): KotohaBridge & {
  emitSubtitle(entry: SubtitleEntry): void;
  emitConnectionStatus(status: ConnectionStatus): void;
  emitDownloadProgress(progress: DownloadProgressEvent): void;
  emitOverlayLockChanged(locked: boolean): void;
  emitOverlayStyleChanged(style: OverlayDisplayOptions): void;
} {
  let state: AppRuntimeState = {
    ...createInitialMockState(),
    ...initialOverrides,
  };
  let pluginInstalledOnce = false;
  const listeners = new Set<KotohaEventHandlers>();
  const activeTimers = new Set<ReturnType<typeof setTimeout>>();

  const notifyConnection = () => {
    const snapshot = cloneState(state.connection);
    for (const handler of listeners) {
      handler.onConnectionStatus?.(snapshot);
    }
  };

  const notifyDownload = (progress: DownloadProgressEvent) => {
    const snapshot = cloneState(progress);
    for (const handler of listeners) {
      handler.onDownloadProgress?.(snapshot);
    }
  };

  const notifyOverlayLock = (locked: boolean) => {
    for (const handler of listeners) {
      handler.onOverlayLockChanged?.(locked);
    }
  };

  const notifyOverlayStyle = (style: OverlayDisplayOptions) => {
    const snapshot = cloneState(style);
    for (const handler of listeners) {
      handler.onOverlayStyleChanged?.(snapshot);
    }
  };

  return {
    isTauri: false,

    async getAppState() {
      return cloneState(state);
    },

    async setOverlayLocked(locked: boolean) {
      state = {
        ...state,
        overlay_options: {
          ...state.overlay_options,
          locked,
        },
      };
      notifyOverlayLock(locked);
      notifyOverlayStyle(state.overlay_options);
      return cloneState(state.overlay_options);
    },

    async toggleOverlayVisible(visible?: boolean) {
      const nextVisible = visible ?? !state.overlay_visible;
      state = {
        ...state,
        overlay_visible: nextVisible,
      };
      return nextVisible;
    },

    async installMumblePlugin(customDir?: string): Promise<PluginInstallResult> {
      const trimmedCustom = customDir?.trim();
      const targetPath =
        trimmedCustom && trimmedCustom.length > 0
          ? trimmedCustom
          : (state.detected_mumble_paths[0] ?? '~/.local/share/Mumble/Mumble/Plugins');

      if (trimmedCustom && trimmedCustom.toLowerCase().includes('locked')) {
        return {
          status: 'locked_by_running_app',
          platform: state.platform,
          target_path: targetPath,
          detected_paths: state.detected_mumble_paths,
          is_flatpak: state.is_flatpak_mumble,
          message:
            'kotoha_mumble.dll is currently locked by a running Mumble process. Close Mumble and retry.',
        };
      }

      const status = pluginInstalledOnce && !trimmedCustom ? 'already_up_to_date' : 'installed';
      pluginInstalledOnce = true;

      if (trimmedCustom && !state.detected_mumble_paths.includes(trimmedCustom)) {
        state = {
          ...state,
          detected_mumble_paths: [trimmedCustom, ...state.detected_mumble_paths],
        };
      }

      const isFlatpak = state.is_flatpak_mumble || targetPath.includes('info.mumble.Mumble');

      return {
        status,
        platform: state.platform,
        target_path: targetPath,
        detected_paths: state.detected_mumble_paths,
        is_flatpak: isFlatpak,
        flatpak_hint: isFlatpak
          ? 'Flatpak Mumble detected: grant runtime IPC socket access via flatpak override --user --filesystem=xdg-run/kotoha_v1.sock info.mumble.Mumble'
          : undefined,
        message:
          status === 'already_up_to_date'
            ? `Mumble plugin is already up to date at ${targetPath}.`
            : `Installed Kotoha Mumble plugin to ${targetPath}.`,
      };
    },

    async downloadModelProfile(modelId: string): Promise<DownloadProgressEvent> {
      const profile = findModelProfile(modelId);
      const totalBytes = profile.size_mb * 1024 * 1024;

      const stage1: DownloadProgressEvent = {
        model_id: profile.model_id,
        bytes_downloaded: Math.round(totalBytes * 0.45),
        total_bytes: totalBytes,
        status: 'downloading',
        sha256_verified: false,
      };
      notifyDownload(stage1);

      await new Promise<void>((resolve) => {
        const t = setTimeout(() => {
          activeTimers.delete(t);
          resolve();
        }, 60);
        activeTimers.add(t);
      });

      const stage2: DownloadProgressEvent = {
        model_id: profile.model_id,
        bytes_downloaded: totalBytes,
        total_bytes: totalBytes,
        status: 'verifying_sha256',
        sha256_verified: false,
      };
      notifyDownload(stage2);

      await new Promise<void>((resolve) => {
        const t = setTimeout(() => {
          activeTimers.delete(t);
          resolve();
        }, 40);
        activeTimers.add(t);
      });

      const verified: DownloadProgressEvent = {
        model_id: profile.model_id,
        bytes_downloaded: totalBytes,
        total_bytes: totalBytes,
        status: 'verified',
        sha256_verified: true,
      };

      if (state.provider.type === 'local_model') {
        state = {
          ...state,
          provider: {
            ...state.provider,
            model_id: profile.model_id,
          },
        };
      }

      notifyDownload(verified);
      return verified;
    },

    async switchTranslationProvider(
      config: TranslationProviderConfig,
      srcLang?: string,
      tgtLang?: string,
    ): Promise<AppRuntimeState> {
      state = {
        ...state,
        provider: cloneState(config),
        src_lang: srcLang ?? state.src_lang,
        tgt_lang: tgtLang ?? state.tgt_lang,
      };
      return cloneState(state);
    },

    async updateGlossarySettings(input: GlossaryUpdateInput): Promise<GlossarySettingsState> {
      const clampedScore = Math.min(3.0, Math.max(0.5, Number(input.hotwords_score) || 1.5));
      const enabledSet = new Set(input.enabled_profiles);
      state = {
        ...state,
        glossary: {
          ...state.glossary,
          enabled_profiles: [...input.enabled_profiles],
          hotwords_score: Math.round(clampedScore * 10) / 10,
          glossary_dir:
            input.glossary_dir !== undefined ? input.glossary_dir : state.glossary.glossary_dir,
          profiles: state.glossary.profiles.map((profile) => ({
            ...profile,
            enabled: enabledSet.has(profile.name) || enabledSet.has(profile.id),
          })),
        },
      };
      return cloneState(state.glossary);
    },

    async updateOverlayStyle(style: OverlayStyleUpdateInput): Promise<OverlayDisplayOptions> {
      const previousLocked = state.overlay_options.locked;
      state = {
        ...state,
        overlay_options: {
          ...state.overlay_options,
          ...style,
        },
      };
      if (style.locked !== undefined && style.locked !== previousLocked) {
        notifyOverlayLock(style.locked);
      }
      notifyOverlayStyle(state.overlay_options);
      return cloneState(state.overlay_options);
    },

    async completeFirstRunWizard(): Promise<AppRuntimeState> {
      state = {
        ...state,
        wizard_completed: true,
        connection: {
          ...state.connection,
          state: 'connected',
          source: state.connection.source || 'mumble',
          plugin_version: state.connection.plugin_version ?? '0.1.0',
          last_heartbeat_ms: state.connection.last_heartbeat_ms ?? 180,
        },
      };
      notifyConnection();
      return cloneState(state);
    },

    async subscribeEvents(handlers: KotohaEventHandlers): Promise<() => void> {
      listeners.add(handlers);
      return () => {
        listeners.delete(handlers);
        for (const t of activeTimers) {
          clearTimeout(t);
        }
        activeTimers.clear();
      };
    },

    emitSubtitle(entry: SubtitleEntry) {
      const snapshot = cloneState(entry);
      for (const handler of listeners) {
        handler.onSubtitle?.(snapshot);
      }
    },

    emitConnectionStatus(status: ConnectionStatus) {
      state = {
        ...state,
        connection: cloneState(status),
      };
      notifyConnection();
    },

    emitDownloadProgress(progress: DownloadProgressEvent) {
      notifyDownload(progress);
    },

    emitOverlayLockChanged(locked: boolean) {
      state = {
        ...state,
        overlay_options: {
          ...state.overlay_options,
          locked,
        },
      };
      notifyOverlayLock(locked);
      notifyOverlayStyle(state.overlay_options);
    },

    emitOverlayStyleChanged(style: OverlayDisplayOptions) {
      state = {
        ...state,
        overlay_options: cloneState(style),
      };
      notifyOverlayStyle(state.overlay_options);
    },
  };
}

export function createTauriBridge(): KotohaBridge {
  const invokeCommand = async <T>(
    command: string,
    args?: Record<string, unknown>,
  ): Promise<T> => {
    const { invoke } = await import('@tauri-apps/api/core');
    return invoke<T>(command, args);
  };

  return {
    isTauri: true,

    getAppState() {
      return invokeCommand<AppRuntimeState>('get_app_state');
    },

    setOverlayLocked(locked: boolean) {
      return invokeCommand<OverlayDisplayOptions>('set_overlay_locked', { locked });
    },

    toggleOverlayVisible(visible?: boolean) {
      return invokeCommand<boolean>('toggle_overlay_visible', { visible: visible ?? null });
    },

    installMumblePlugin(customDir?: string) {
      return invokeCommand<PluginInstallResult>('install_mumble_plugin', {
        customDir: customDir ?? null,
      });
    },

    downloadModelProfile(modelId: string) {
      return invokeCommand<DownloadProgressEvent>('download_model_profile', { modelId });
    },

    switchTranslationProvider(
      config: TranslationProviderConfig,
      srcLang?: string,
      tgtLang?: string,
    ) {
      return invokeCommand<AppRuntimeState>('switch_translation_provider', {
        config,
        srcLang: srcLang ?? null,
        tgtLang: tgtLang ?? null,
      });
    },

    updateGlossarySettings(input: GlossaryUpdateInput) {
      return invokeCommand<GlossarySettingsState>('update_glossary_settings', {
        enabledProfiles: input.enabled_profiles,
        hotwordsScore: input.hotwords_score,
        glossaryDir: input.glossary_dir ?? null,
      });
    },

    updateOverlayStyle(style: OverlayStyleUpdateInput) {
      return invokeCommand<OverlayDisplayOptions>('update_overlay_style', { style });
    },

    completeFirstRunWizard() {
      return invokeCommand<AppRuntimeState>('complete_first_run_wizard');
    },

    async subscribeEvents(handlers: KotohaEventHandlers): Promise<() => void> {
      const { listen } = await import('@tauri-apps/api/event');
      const unlistenFns: Array<() => void> = [];

      if (handlers.onSubtitle) {
        const unlisten = await listen<SubtitleEntry>(TAURI_EVENTS.SUBTITLE, (event) => {
          handlers.onSubtitle?.(event.payload);
        });
        unlistenFns.push(unlisten);
      }

      if (handlers.onConnectionStatus) {
        const unlisten = await listen<ConnectionStatus>(
          TAURI_EVENTS.CONNECTION_STATUS,
          (event) => {
            handlers.onConnectionStatus?.(event.payload);
          },
        );
        unlistenFns.push(unlisten);
      }

      if (handlers.onDownloadProgress) {
        const unlisten = await listen<DownloadProgressEvent>(
          TAURI_EVENTS.DOWNLOAD_PROGRESS,
          (event) => {
            handlers.onDownloadProgress?.(event.payload);
          },
        );
        unlistenFns.push(unlisten);
      }

      if (handlers.onOverlayLockChanged) {
        const unlisten = await listen<boolean>(
          TAURI_EVENTS.OVERLAY_LOCK_CHANGED,
          (event) => {
            handlers.onOverlayLockChanged?.(event.payload);
          },
        );
        unlistenFns.push(unlisten);
      }

      if (handlers.onOverlayStyleChanged) {
        const unlisten = await listen<OverlayDisplayOptions>(
          TAURI_EVENTS.OVERLAY_STYLE_CHANGED,
          (event) => {
            handlers.onOverlayStyleChanged?.(event.payload);
          },
        );
        unlistenFns.push(unlisten);
      }

      return () => {
        for (const unlisten of unlistenFns) {
          unlisten();
        }
      };
    },
  };
}

let cachedDefaultBridge: KotohaBridge | null = null;

export function getKotohaBridge(): KotohaBridge {
  if (isTauriRuntime()) {
    return createTauriBridge();
  }
  if (!cachedDefaultBridge) {
    cachedDefaultBridge = createMockBridge();
  }
  return cachedDefaultBridge;
}

export function resetDefaultMockBridgeForTesting(): void {
  cachedDefaultBridge = null;
}
