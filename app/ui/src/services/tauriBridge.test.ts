import { afterEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import { createDefaultProviderConfig } from '../data/modelCatalog';
import {
  SECRET_STATUS_FIELD,
  TAURI_EVENTS,
  createMockBridge,
  createTauriBridge,
  getKotohaBridge,
  isTauriRuntime,
  resetDefaultMockBridgeForTesting,
  urlOrigin,
} from './tauriBridge';
import type {
  ConnectionStatus,
  DownloadProgressEvent,
  SubtitleEntry,
} from '../types/kotoha';

describe('tauriBridge', () => {
  afterEach(() => {
    clearMocks();
    resetDefaultMockBridgeForTesting();
  });

  it('detects non-Tauri browser environment and returns interactive mock bridge', async () => {
    expect(isTauriRuntime()).toBe(false);

    const bridge = getKotohaBridge();
    expect(bridge.isTauri).toBe(false);

    const initialState = await bridge.getAppState();
    expect(initialState.src_lang).toBe('en');
    expect(initialState.tgt_lang).toBe('ja');
    expect(initialState.glossary.hotwords_score).toBe(1.5);
    expect(initialState.glossary.profiles.length).toBeGreaterThanOrEqual(2);

    const lockedState = await bridge.setOverlayLocked(false);
    expect(lockedState.locked).toBe(false);

    const visibleAfterToggle = await bridge.toggleOverlayVisible(false);
    expect(visibleAfterToggle).toBe(false);

    const updatedGlossary = await bridge.updateGlossarySettings({
      enabled_profiles: ['My Profile', 'FPS Callouts'],
      hotwords_score: 2.3,
    });
    expect(updatedGlossary.hotwords_score).toBe(2.3);
    expect(updatedGlossary.enabled_profiles).toEqual(['My Profile', 'FPS Callouts']);

    const install1 = await bridge.installMumblePlugin();
    expect(install1.status).toBe('installed');

    const install2 = await bridge.installMumblePlugin();
    expect(install2.status).toBe('already_up_to_date');

    const lockedInstall = await bridge.installMumblePlugin('C:\\locked\\Mumble\\Plugins');
    expect(lockedInstall.status).toBe('locked_by_running_app');
  });

  it('emits download progress events and verifies SHA-256 in mock bridge', async () => {
    const mockBridge = createMockBridge();
    const progressEvents: DownloadProgressEvent[] = [];
    const subtitleEvents: SubtitleEntry[] = [];
    const connectionEvents: ConnectionStatus[] = [];
    const lockEvents: boolean[] = [];

    const unlisten = await mockBridge.subscribeEvents({
      onDownloadProgress: (event) => progressEvents.push(event),
      onSubtitle: (entry) => subtitleEvents.push(entry),
      onConnectionStatus: (status) => connectionEvents.push(status),
      onOverlayLockChanged: (locked) => lockEvents.push(locked),
    });

    const finalProgress = await mockBridge.downloadModelProfile('fugumt-en-ja-int8');
    expect(finalProgress.status).toBe('verified');
    expect(finalProgress.sha256_verified).toBe(true);
    expect(progressEvents.map((e) => e.status)).toEqual([
      'downloading',
      'verifying_sha256',
      'verified',
    ]);

    mockBridge.emitSubtitle({
      utterance_id: 999,
      speaker_key: { source: 'mumble', user_id: 7 },
      speaker_name: 'Kira',
      speaker_color: '#38BDF8',
      src_text: 'Test subtitle',
      translated_text: 'テスト字幕',
      stage: 'translated',
      timestamp_ms: 1700000010000,
    });
    expect(subtitleEvents).toHaveLength(1);
    expect(subtitleEvents[0]?.utterance_id).toBe(999);

    await mockBridge.setOverlayLocked(false);
    expect(lockEvents).toEqual([false]);

    await mockBridge.completeFirstRunWizard();
    expect(connectionEvents.length).toBeGreaterThanOrEqual(1);

    unlisten();
  });

  it('invokes all real Tauri v2 commands and subscribes to kotoha://* events when __TAURI_INTERNALS__ exists', async () => {
    const invokedCommands: Array<{ cmd: string; payload: unknown }> = [];

    mockIPC(
      (cmd, payload) => {
        invokedCommands.push({ cmd, payload });
        switch (cmd) {
          case 'get_app_state':
            return {
              src_lang: 'en',
              tgt_lang: 'ja',
              provider: { type: 'mock', prefix: '[ja] ' },
              overlay_options: {
                max_lines: 4,
                show_source_text: true,
                locked: true,
                font_size_px: 18,
                background_opacity: 0.78,
              },
              overlay_visible: true,
              glossary: {
                profiles: [],
                enabled_profiles: [],
                hotwords_score: 1.5,
                glossary_dir: null,
              },
              connection: {
                state: 'connected',
                source: 'mumble',
                plugin_version: '0.1.0',
                endpoint: '/tmp/kotoha.sock',
                active_speakers: 1,
                dropped_frames: 0,
                last_heartbeat_ms: 50,
              },
              subtitles: [],
              wizard_completed: true,
              platform: 'linux',
              detected_mumble_paths: ['/tmp/mumble/Plugins'],
              is_flatpak_mumble: false,
            };
          case 'set_overlay_locked':
            return {
              max_lines: 4,
              show_source_text: true,
              locked: Boolean((payload as { locked?: boolean })?.locked),
              font_size_px: 18,
              background_opacity: 0.78,
            };
          case 'toggle_overlay_visible':
            return Boolean((payload as { visible?: boolean })?.visible ?? true);
          case 'install_mumble_plugin':
            return {
              status: 'installed',
              platform: 'linux',
              target_path: '/tmp/mumble/Plugins',
              detected_paths: ['/tmp/mumble/Plugins'],
              is_flatpak: false,
              message: 'ok',
            };
          case 'download_model_profile':
            return {
              model_id: 'fugumt-en-ja-int8',
              bytes_downloaded: 100,
              total_bytes: 100,
              status: 'verified',
              sha256_verified: true,
            };
          case 'set_provider_secret':
          case 'clear_provider_secret':
            return {
              openai_api_key: false,
              deepl_api_key: true,
              google_cloud_api_key: false,
              gas_endpoint_url: false,
              gas_bearer_token: false,
            };
          case 'switch_translation_provider':
          case 'complete_first_run_wizard':
            return { wizard_completed: true };
          case 'update_glossary_settings':
            return {
              profiles: [],
              enabled_profiles: ['My Profile'],
              hotwords_score: 2.0,
              glossary_dir: null,
            };
          case 'update_overlay_style':
            return {
              max_lines: 3,
              show_source_text: false,
              locked: true,
              font_size_px: 20,
              background_opacity: 0.8,
            };
          default:
            return null;
        }
      },
      { shouldMockEvents: true },
    );

    expect(isTauriRuntime()).toBe(true);
    const bridge = getKotohaBridge();
    expect(bridge.isTauri).toBe(true);

    const state = await bridge.getAppState();
    expect(state.wizard_completed).toBe(true);

    await bridge.setOverlayLocked(false);
    await bridge.toggleOverlayVisible(false);
    await bridge.installMumblePlugin('/custom/path');
    await bridge.downloadModelProfile('fugumt-en-ja-int8');
    await bridge.switchTranslationProvider({ type: 'mock', prefix: '[ja] ' }, 'en', 'ja');
    const setStatus = await bridge.setProviderSecret('deep_l_api_key', 'abc');
    expect(setStatus.deepl_api_key).toBe(true);
    await bridge.clearProviderSecret('deep_l_api_key');
    await bridge.updateGlossarySettings({
      enabled_profiles: ['My Profile'],
      hotwords_score: 2.0,
    });
    await bridge.updateOverlayStyle({ max_lines: 3, font_size_px: 20 });
    await bridge.completeFirstRunWizard();

    expect(invokedCommands.map((entry) => entry.cmd)).toEqual([
      'get_app_state',
      'set_overlay_locked',
      'toggle_overlay_visible',
      'install_mumble_plugin',
      'download_model_profile',
      'switch_translation_provider',
      'set_provider_secret',
      'clear_provider_secret',
      'update_glossary_settings',
      'update_overlay_style',
      'complete_first_run_wizard',
    ]);

    expect(
      invokedCommands.find((entry) => entry.cmd === 'set_provider_secret')?.payload,
    ).toEqual({ kind: 'deep_l_api_key', value: 'abc' });
    expect(
      invokedCommands.find((entry) => entry.cmd === 'clear_provider_secret')?.payload,
    ).toEqual({ kind: 'deep_l_api_key' });

    const onSubtitle = vi.fn();
    const onConnectionStatus = vi.fn();
    const onDownloadProgress = vi.fn();
    const onOverlayLockChanged = vi.fn();
    const onOverlayStyleChanged = vi.fn();

    await bridge.toggleOverlayVisible();
    const toggleVisibleNoArgCall = invokedCommands
      .filter((entry) => entry.cmd === 'toggle_overlay_visible')
      .at(-1);
    expect(toggleVisibleNoArgCall?.payload).toEqual({ visible: null });

    const tauriBridge = createTauriBridge();
    const unlisten = await tauriBridge.subscribeEvents({
      onSubtitle,
      onConnectionStatus,
      onDownloadProgress,
      onOverlayLockChanged,
      onOverlayStyleChanged,
    });

    await emit(TAURI_EVENTS.SUBTITLE, {
      utterance_id: 55,
      speaker_key: { source: 'mumble', user_id: 9 },
      speaker_name: 'Nova',
      speaker_color: '#34D399',
      src_text: 'Ready',
      translated_text: '準備完了',
      stage: 'translated',
      timestamp_ms: 1700000012000,
    });
    await emit(TAURI_EVENTS.CONNECTION_STATUS, {
      state: 'connected',
      source: 'mumble',
      plugin_version: '0.1.0',
      endpoint: '/tmp/kotoha.sock',
      active_speakers: 2,
      dropped_frames: 0,
      last_heartbeat_ms: 100,
    });
    await emit(TAURI_EVENTS.DOWNLOAD_PROGRESS, {
      model_id: 'fugumt-en-ja-int8',
      bytes_downloaded: 50,
      total_bytes: 100,
      status: 'downloading',
      sha256_verified: false,
    });
    await emit(TAURI_EVENTS.OVERLAY_LOCK_CHANGED, false);
    await emit(TAURI_EVENTS.OVERLAY_STYLE_CHANGED, {
      max_lines: 6,
      show_source_text: false,
      locked: false,
      font_size_px: 24,
      background_opacity: 0.65,
    });

    expect(onSubtitle).toHaveBeenCalledTimes(1);
    expect(onConnectionStatus).toHaveBeenCalledTimes(1);
    expect(onDownloadProgress).toHaveBeenCalledTimes(1);
    expect(onOverlayLockChanged).toHaveBeenCalledWith(false);
    expect(onOverlayStyleChanged).toHaveBeenCalledWith({
      max_lines: 6,
      show_source_text: false,
      locked: false,
      font_size_px: 24,
      background_opacity: 0.65,
    });

    unlisten();
  });

  it('emits onOverlayStyleChanged when updateOverlayStyle is called on mock bridge', async () => {
    const mockBridge = createMockBridge();
    const styleEvents: Array<{
      max_lines: number;
      show_source_text: boolean;
      locked: boolean;
      font_size_px: number;
      background_opacity: number;
    }> = [];

    const unlisten = await mockBridge.subscribeEvents({
      onOverlayStyleChanged: (style) => styleEvents.push(style),
    });

    const updated = await mockBridge.updateOverlayStyle({
      max_lines: 2,
      show_source_text: false,
      font_size_px: 22,
    });

    expect(styleEvents).toEqual([updated]);
    expect(styleEvents[0]?.font_size_px).toBe(22);
    expect(styleEvents[0]?.show_source_text).toBe(false);

    unlisten();
  });
  describe('mock bridge secret handling', () => {
    const openAi = (base_url: string) => ({
      type: 'openai_compatible' as const,
      base_url,
      model: 'm',
      timeout_ms: 15000,
    });

    it('starts with no secrets and rejects kinds of other providers', async () => {
      const bridge = createMockBridge();
      const state = await bridge.getAppState();
      expect(Object.values(state.secrets)).toEqual([false, false, false, false, false]);
      await expect(bridge.setProviderSecret('deep_l_api_key', 'k')).rejects.toThrow(
        /active translation provider/,
      );
      await expect(bridge.setProviderSecret('open_ai_api_key', '  ')).rejects.toThrow(/empty/);
    });

    it('stores booleans only and never returns the value', async () => {
      const bridge = createMockBridge();
      await bridge.switchTranslationProvider(openAi('http://localhost:11434/v1'));
      const status = await bridge.setProviderSecret('open_ai_api_key', 'sk-very-secret');
      expect(status.openai_api_key).toBe(true);
      expect(JSON.stringify(await bridge.getAppState())).not.toContain('sk-very-secret');
      expect((await bridge.clearProviderSecret('open_ai_api_key')).openai_api_key).toBe(false);
    });

    it('clears the OpenAI key on origin change or provider switch, not on path change', async () => {
      const bridge = createMockBridge();
      await bridge.switchTranslationProvider(openAi('http://localhost:11434/v1'));
      await bridge.setProviderSecret('open_ai_api_key', 'k');

      let state = await bridge.switchTranslationProvider(openAi('http://LOCALHOST:11434/other'));
      expect(state.secrets.openai_api_key).toBe(true);

      state = await bridge.switchTranslationProvider(openAi('https://api.example.com/v1'));
      expect(state.secrets.openai_api_key).toBe(false);

      await bridge.setProviderSecret('open_ai_api_key', 'k');
      await bridge.switchTranslationProvider({ type: 'mock', prefix: '' });
      state = await bridge.switchTranslationProvider(openAi('https://api.example.com/v1'));
      expect(state.secrets.openai_api_key).toBe(false);
    });

    it('fails closed on ambiguous base URLs (backslash, userinfo, escapes)', async () => {
      expect(urlOrigin('HTTPS://API.OpenAI.com:8443/v1?x#y')).toBe('https://api.openai.com:8443');
      for (const bad of [
        'https://evil.example\\@api.openai.com/v1',
        'https://api.openai.com@evil.example/v1',
        'https://a%40b/',
        'https://a b/',
        'localhost:11434/v1',
      ]) {
        expect(urlOrigin(bad)).toBeNull();
      }

      const bridge = createMockBridge();
      await bridge.switchTranslationProvider(openAi('https://api.openai.com/v1'));
      await bridge.setProviderSecret('open_ai_api_key', 'k');
      const state = await bridge.switchTranslationProvider(
        openAi('https://evil.example\\@api.openai.com/v1'),
      );
      expect(state.secrets.openai_api_key).toBe(false);
    });

    it('keeps the key when nothing changed, even for an unparseable URL', async () => {
      const bridge = createMockBridge();
      await bridge.switchTranslationProvider(openAi('http://localhost:11434\\v1'));
      await bridge.setProviderSecret('open_ai_api_key', 'k');
      const state = await bridge.switchTranslationProvider(openAi('http://localhost:11434\\v1'));
      expect(state.secrets.openai_api_key).toBe(true);
    });

    it('clears the GAS bearer token when the endpoint is replaced', async () => {
      const bridge = createMockBridge();
      await bridge.switchTranslationProvider({
        type: 'gas_webhook',
        timeout_ms: 10000,
      });
      await bridge.setProviderSecret('gas_endpoint_url', 'https://script.example/exec');
      await bridge.setProviderSecret('gas_bearer_token', 't');
      expect((await bridge.getAppState()).secrets.gas_bearer_token).toBe(true);
      const status = await bridge.setProviderSecret('gas_endpoint_url', 'https://other/exec');
      expect(status.gas_endpoint_url).toBe(true);
      expect(status.gas_bearer_token).toBe(false);
    });
  });

  it('uses the same provider and secret-kind wire tags as the Rust serde format', async () => {
    // Keep in sync with provider_and_secret_kind_wire_tags_match_the_ui (domain_secret_test.rs).
    const providerTags = [
      'local_model',
      'openai_compatible',
      'deepl',
      'google_cloud',
      'gas_webhook',
      'mock',
    ] as const;
    for (const tag of providerTags) {
      expect(createDefaultProviderConfig(tag).type).toBe(tag);
    }
    const kindTags = [
      'open_ai_api_key',
      'deep_l_api_key',
      'google_cloud_api_key',
      'gas_endpoint_url',
      'gas_bearer_token',
    ] as const;
    expect(Object.keys(SECRET_STATUS_FIELD)).toEqual([...kindTags]);
  });
});
