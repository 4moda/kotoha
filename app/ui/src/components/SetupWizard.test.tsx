import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { SetupWizard } from './SetupWizard';
import type {
  ConnectionStatus,
  DownloadProgressEvent,
  PluginInstallResult,
} from '../types/kotoha';

const waitingConnection: ConnectionStatus = {
  state: 'disconnected',
  source: 'mumble',
  plugin_version: null,
  endpoint: '$XDG_RUNTIME_DIR/kotoha_v1.sock',
  active_speakers: 0,
  dropped_frames: 0,
  last_heartbeat_ms: null,
};

const connectedStatus: ConnectionStatus = {
  state: 'connected',
  source: 'mumble',
  plugin_version: '0.1.0',
  endpoint: '$XDG_RUNTIME_DIR/kotoha_v1.sock',
  active_speakers: 2,
  dropped_frames: 0,
  last_heartbeat_ms: 95,
};

describe('SetupWizard', () => {
  it('handles Step 1 plugin installation states (installed, already_up_to_date, locked_by_running_app) and optional custom directory', async () => {
    const onInstallPlugin = vi
      .fn<(customDir?: string) => Promise<PluginInstallResult>>()
      .mockResolvedValueOnce({
        status: 'installed',
        platform: 'linux',
        target_path: '~/.local/share/Mumble/Mumble/Plugins',
        detected_paths: ['~/.local/share/Mumble/Mumble/Plugins'],
        is_flatpak: false,
        message: 'Installed successfully',
      })
      .mockResolvedValueOnce({
        status: 'already_up_to_date',
        platform: 'linux',
        target_path: '~/.local/share/Mumble/Mumble/Plugins',
        detected_paths: ['~/.local/share/Mumble/Mumble/Plugins'],
        is_flatpak: false,
        message: 'Already up to date',
      })
      .mockResolvedValueOnce({
        status: 'locked_by_running_app',
        platform: 'windows',
        target_path: 'C:\\Mumble\\Plugins',
        detected_paths: ['C:\\Mumble\\Plugins'],
        is_flatpak: false,
        message: 'DLL locked by running Mumble',
      });

    render(
      <SetupWizard
        platform="windows"
        detectedMumblePaths={['C:\\Users\\user\\AppData\\Roaming\\Mumble\\Mumble\\Plugins']}
        isFlatpakMumble={false}
        connection={waitingConnection}
        selectedModelId="fugumt-en-ja-int8"
        downloadProgress={null}
        wizardCompleted={false}
        onInstallPlugin={onInstallPlugin}
        onDownloadModel={vi.fn()}
        onSkipToApiTranslation={vi.fn()}
        onCompleteWizard={vi.fn()}
      />,
    );

    expect(screen.getByTestId('wizard-step-1')).toBeInTheDocument();
    expect(screen.getByTestId('windows-fullscreen-note')).toHaveTextContent(
      'ゲーム側はボーダーレスウィンドウ表示を推奨',
    );

    fireEvent.click(screen.getByTestId('install-plugin-btn'));
    await waitFor(() => {
      expect(screen.getByTestId('plugin-install-status-code')).toHaveTextContent('installed');
    });
    expect(onInstallPlugin).toHaveBeenNthCalledWith(1, undefined);

    fireEvent.click(screen.getByTestId('install-plugin-btn'));
    await waitFor(() => {
      expect(screen.getByTestId('plugin-install-status-code')).toHaveTextContent(
        'already_up_to_date',
      );
    });

    fireEvent.change(screen.getByTestId('custom-plugin-dir-input'), {
      target: { value: 'C:\\Mumble\\Plugins' },
    });
    fireEvent.click(screen.getByTestId('install-plugin-btn'));
    await waitFor(() => {
      expect(screen.getByTestId('plugin-install-status-code')).toHaveTextContent(
        'locked_by_running_app',
      );
    });
    expect(onInstallPlugin).toHaveBeenNthCalledWith(3, 'C:\\Mumble\\Plugins');
    expect(screen.getByTestId('locked-plugin-guidance')).toHaveTextContent('Mumble');
  });

  it('handles Step 2 model selection, download progress + SHA-256 verification, and skip-to-API flow', async () => {
    const onDownloadModel = vi
      .fn<(modelId: string) => Promise<DownloadProgressEvent>>()
      .mockResolvedValue({
        model_id: 'nllb-200-distilled-600m-int8',
        bytes_downloaded: 681574400,
        total_bytes: 681574400,
        status: 'verified',
        sha256_verified: true,
      });
    const onSkipToApiTranslation = vi.fn();

    const { rerender } = render(
      <SetupWizard
        platform="linux"
        detectedMumblePaths={['~/.local/share/Mumble/Mumble/Plugins']}
        isFlatpakMumble={true}
        connection={waitingConnection}
        selectedModelId="fugumt-en-ja-int8"
        downloadProgress={{
          model_id: 'nllb-200-distilled-600m-int8',
          bytes_downloaded: 340787200,
          total_bytes: 681574400,
          status: 'downloading',
          sha256_verified: false,
        }}
        wizardCompleted={false}
        onInstallPlugin={vi.fn()}
        onDownloadModel={onDownloadModel}
        onSkipToApiTranslation={onSkipToApiTranslation}
        onCompleteWizard={vi.fn()}
      />,
    );

    expect(screen.getByTestId('wizard-platform-chip')).toHaveTextContent('Flatpak 検出');

    fireEvent.click(screen.getByTestId('wizard-next-step-1'));
    expect(screen.getByTestId('wizard-step-2')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('wizard-model-card-nllb-200-distilled-600m-int8'));
    expect(screen.getByTestId('wizard-non-commercial-note')).toHaveTextContent('非商用');
    expect(screen.getByTestId('download-status-label')).toHaveTextContent('downloading');
    expect(screen.getByTestId('download-bytes-label')).toHaveTextContent('50%');

    fireEvent.click(screen.getByTestId('download-model-btn'));
    await waitFor(() => {
      expect(onDownloadModel).toHaveBeenCalledWith('nllb-200-distilled-600m-int8');
    });

    rerender(
      <SetupWizard
        platform="linux"
        detectedMumblePaths={['~/.local/share/Mumble/Mumble/Plugins']}
        isFlatpakMumble={true}
        connection={waitingConnection}
        selectedModelId="nllb-200-distilled-600m-int8"
        downloadProgress={{
          model_id: 'nllb-200-distilled-600m-int8',
          bytes_downloaded: 681574400,
          total_bytes: 681574400,
          status: 'verified',
          sha256_verified: true,
        }}
        wizardCompleted={false}
        onInstallPlugin={vi.fn()}
        onDownloadModel={onDownloadModel}
        onSkipToApiTranslation={onSkipToApiTranslation}
        onCompleteWizard={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId('wizard-step-tab-2'));
    expect(screen.getByTestId('sha256-verification-status')).toHaveTextContent('Verified OK');

    fireEvent.click(screen.getByTestId('skip-to-api-btn'));
    expect(onSkipToApiTranslation).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('wizard-step-3')).toBeInTheDocument();
  });

  it('transitions Step 3 Heartbeat indicator from waiting spinner to connected badge and completes setup', async () => {
    const onCompleteWizard = vi.fn().mockResolvedValue(undefined);
    const { rerender } = render(
      <SetupWizard
        platform="linux"
        detectedMumblePaths={[]}
        isFlatpakMumble={false}
        connection={waitingConnection}
        selectedModelId="fugumt-en-ja-int8"
        downloadProgress={null}
        wizardCompleted={false}
        onInstallPlugin={vi.fn()}
        onDownloadModel={vi.fn()}
        onSkipToApiTranslation={vi.fn()}
        onCompleteWizard={onCompleteWizard}
      />,
    );

    fireEvent.click(screen.getByTestId('wizard-step-tab-3'));
    expect(screen.getByTestId('mumble-enable-guide')).toHaveTextContent('Kotoha Voice Translator');
    expect(screen.getByTestId('heartbeat-waiting-spinner')).toBeInTheDocument();

    rerender(
      <SetupWizard
        platform="linux"
        detectedMumblePaths={[]}
        isFlatpakMumble={false}
        connection={connectedStatus}
        selectedModelId="fugumt-en-ja-int8"
        downloadProgress={null}
        wizardCompleted={false}
        onInstallPlugin={vi.fn()}
        onDownloadModel={vi.fn()}
        onSkipToApiTranslation={vi.fn()}
        onCompleteWizard={onCompleteWizard}
      />,
    );

    expect(screen.getByTestId('heartbeat-connected-badge')).toBeInTheDocument();
    expect(screen.getByTestId('heartbeat-connected-badge')).toHaveTextContent('mumble');
    expect(screen.getByTestId('heartbeat-connected-badge')).toHaveTextContent('0.1.0');

    fireEvent.click(screen.getByTestId('complete-wizard-btn'));
    await waitFor(() => {
      expect(onCompleteWizard).toHaveBeenCalledTimes(1);
    });
  });

  it('surfaces plugin installation, model download, failed status error_message, and completion errors cleanly', async () => {
    const onInstallPlugin = vi
      .fn<(customDir?: string) => Promise<PluginInstallResult>>()
      .mockRejectedValueOnce(new Error('Permission denied writing Mumble plugin directory'))
      .mockResolvedValueOnce({
        status: 'installed',
        platform: 'linux',
        target_path: '~/.local/share/Mumble/Mumble/Plugins',
        detected_paths: ['~/.local/share/Mumble/Mumble/Plugins'],
        is_flatpak: false,
        message: 'Installed on retry',
      });

    const onDownloadModel = vi
      .fn<(modelId: string) => Promise<DownloadProgressEvent>>()
      .mockRejectedValueOnce(new Error('SHA-256 mismatch from mirror'))
      .mockResolvedValueOnce({
        model_id: 'fugumt-en-ja-int8',
        bytes_downloaded: 209715200,
        total_bytes: 209715200,
        status: 'verified',
        sha256_verified: true,
      });

    const onCompleteWizard = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error('Failed to persist wizard completion'))
      .mockResolvedValueOnce(undefined);

    const { rerender } = render(
      <SetupWizard
        platform="linux"
        detectedMumblePaths={['~/.local/share/Mumble/Mumble/Plugins']}
        isFlatpakMumble={false}
        connection={connectedStatus}
        selectedModelId="fugumt-en-ja-int8"
        downloadProgress={null}
        wizardCompleted={false}
        onInstallPlugin={onInstallPlugin}
        onDownloadModel={onDownloadModel}
        onSkipToApiTranslation={vi.fn()}
        onCompleteWizard={onCompleteWizard}
      />,
    );

    // Step 1: rejected plugin install renders error banner and recovers on retry
    fireEvent.click(screen.getByTestId('install-plugin-btn'));
    await waitFor(() => {
      expect(screen.getByTestId('plugin-install-error')).toHaveTextContent(
        'Permission denied writing Mumble plugin directory',
      );
    });

    fireEvent.click(screen.getByTestId('install-plugin-btn'));
    await waitFor(() => {
      expect(screen.queryByTestId('plugin-install-error')).not.toBeInTheDocument();
      expect(screen.getByTestId('plugin-install-status-code')).toHaveTextContent('installed');
    });

    // Step 2: rejected model download renders error banner + activeProgress status='failed' renders error_message
    fireEvent.click(screen.getByTestId('wizard-next-step-1'));
    fireEvent.click(screen.getByTestId('download-model-btn'));
    await waitFor(() => {
      expect(screen.getByTestId('model-download-error')).toHaveTextContent(
        'SHA-256 mismatch from mirror',
      );
    });

    rerender(
      <SetupWizard
        platform="linux"
        detectedMumblePaths={['~/.local/share/Mumble/Mumble/Plugins']}
        isFlatpakMumble={false}
        connection={connectedStatus}
        selectedModelId="fugumt-en-ja-int8"
        downloadProgress={{
          model_id: 'fugumt-en-ja-int8',
          bytes_downloaded: 104857600,
          total_bytes: 209715200,
          status: 'failed',
          sha256_verified: false,
          error_message: 'Checksum verification failed: expected deadbeef',
        }}
        wizardCompleted={false}
        onInstallPlugin={onInstallPlugin}
        onDownloadModel={onDownloadModel}
        onSkipToApiTranslation={vi.fn()}
        onCompleteWizard={onCompleteWizard}
      />,
    );

    expect(screen.getByTestId('download-status-label')).toHaveTextContent('failed');
    expect(screen.getByTestId('sha256-verification-status')).toHaveTextContent('Failed');
    expect(screen.getByTestId('model-download-failed-callout')).toBeInTheDocument();
    expect(screen.getByTestId('model-download-error-message')).toHaveTextContent(
      'Checksum verification failed: expected deadbeef',
    );

    // Retry clears error and succeeds
    fireEvent.click(screen.getByTestId('download-model-btn'));
    await waitFor(() => {
      expect(onDownloadModel).toHaveBeenCalledTimes(2);
    });

    // Step 3: rejected completion renders error banner and recovers on retry
    fireEvent.click(screen.getByTestId('wizard-next-step-2'));
    fireEvent.click(screen.getByTestId('complete-wizard-btn'));
    await waitFor(() => {
      expect(screen.getByTestId('wizard-complete-error')).toHaveTextContent(
        'Failed to persist wizard completion',
      );
    });

    fireEvent.click(screen.getByTestId('complete-wizard-btn'));
    await waitFor(() => {
      expect(screen.queryByTestId('wizard-complete-error')).not.toBeInTheDocument();
      expect(onCompleteWizard).toHaveBeenCalledTimes(2);
    });
  });
});
