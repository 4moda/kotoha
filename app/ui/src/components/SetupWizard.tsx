import { useState } from 'react';
import { MODEL_CATALOG, findModelProfile } from '../data/modelCatalog';
import type {
  ConnectionStatus,
  DownloadProgressEvent,
  HostOsPlatform,
  PluginInstallResult,
  TranslationProviderConfig,
} from '../types/kotoha';

export interface SetupWizardProps {
  platform: HostOsPlatform;
  detectedMumblePaths: readonly string[];
  isFlatpakMumble: boolean;
  connection: ConnectionStatus;
  selectedModelId: string;
  downloadProgress: DownloadProgressEvent | null;
  wizardCompleted: boolean;
  onInstallPlugin: (customDir?: string) => Promise<PluginInstallResult>;
  onDownloadModel: (modelId: string) => Promise<DownloadProgressEvent>;
  onSkipToApiTranslation: (config?: TranslationProviderConfig) => void;
  onCompleteWizard: () => Promise<void> | void;
}

type WizardStep = 1 | 2 | 3;

function formatBytesMb(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function SetupWizard({
  platform,
  detectedMumblePaths,
  isFlatpakMumble,
  connection,
  selectedModelId,
  downloadProgress,
  wizardCompleted,
  onInstallPlugin,
  onDownloadModel,
  onSkipToApiTranslation,
  onCompleteWizard,
}: SetupWizardProps) {
  const [step, setStep] = useState<WizardStep>(1);
  const [customPluginDir, setCustomPluginDir] = useState('');
  const [installingPlugin, setInstallingPlugin] = useState(false);
  const [installResult, setInstallResult] = useState<PluginInstallResult | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);
  const [chosenModelId, setChosenModelId] = useState(selectedModelId);
  const [downloadingModel, setDownloadingModel] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [skippedToApi, setSkippedToApi] = useState(false);
  const [completing, setCompleting] = useState(false);
  const [completeError, setCompleteError] = useState<string | null>(null);

  const selectedModel = findModelProfile(chosenModelId);
  const activeProgress =
    downloadProgress && downloadProgress.model_id === chosenModelId ? downloadProgress : null;
  const progressPercent =
    activeProgress && activeProgress.total_bytes > 0
      ? Math.min(
          100,
          Math.round((activeProgress.bytes_downloaded / activeProgress.total_bytes) * 100),
        )
      : 0;

  const isHeartbeatDetected =
    connection.state === 'connected' && connection.last_heartbeat_ms !== null;

  const handleInstallPlugin = async () => {
    setInstallingPlugin(true);
    setInstallError(null);
    try {
      const result = await onInstallPlugin(customPluginDir.trim() || undefined);
      setInstallResult(result);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setInstallError(
        message || 'プラグインの配置に失敗しました。権限やパスを確認して再試行してください。',
      );
    } finally {
      setInstallingPlugin(false);
    }
  };

  const handleDownloadModel = async () => {
    setSkippedToApi(false);
    setDownloadingModel(true);
    setDownloadError(null);
    try {
      const result = await onDownloadModel(chosenModelId);
      if (result.status === 'failed') {
        setDownloadError(
          result.error_message || 'モデルのダウンロードまたは検証に失敗しました。再試行してください。',
        );
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setDownloadError(
        message || 'モデルのダウンロードまたは検証に失敗しました。再試行してください。',
      );
    } finally {
      setDownloadingModel(false);
    }
  };

  const handleSkipApi = () => {
    setSkippedToApi(true);
    setDownloadError(null);
    onSkipToApiTranslation();
    setStep(3);
  };

  const handleComplete = async () => {
    setCompleting(true);
    setCompleteError(null);
    try {
      await onCompleteWizard();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setCompleteError(
        message || 'セットアップ完了処理に失敗しました。もう一度お試しください。',
      );
    } finally {
      setCompleting(false);
    }
  };

  return (
    <section
      className="wizard-panel"
      aria-label="Kotoha First-Run Setup Wizard"
      data-testid="setup-wizard"
    >
      <header className="wizard-header">
        <div>
          <h2 className="wizard-title">初回セットアップウィザード (Zero-Config Setup)</h2>
          <p className="wizard-subtitle">
            3 ステップで Mumble プラグイン配置、ローカル翻訳モデル導入、Heartbeat 接続確認を完了します。
          </p>
        </div>
        {wizardCompleted && (
          <span className="wizard-completed-pill" data-testid="wizard-completed-badge">
            セットアップ完了済み
          </span>
        )}
      </header>

      <ol className="wizard-stepper" aria-label="Setup Steps">
        {[
          { num: 1 as const, label: 'Step 1: Mumble プラグイン自動配置' },
          { num: 2 as const, label: 'Step 2: 翻訳モデル・エンジン選択' },
          { num: 3 as const, label: 'Step 3: Mumble 有効化 & Heartbeat 検知' },
        ].map((item) => (
          <li key={item.num}>
            <button
              type="button"
              data-testid={`wizard-step-tab-${item.num}`}
              className={`wizard-step-pill ${step === item.num ? 'is-active' : ''} ${
                step > item.num ? 'is-done' : ''
              }`}
              onClick={() => setStep(item.num)}
            >
              {item.label}
            </button>
          </li>
        ))}
      </ol>

      {step === 1 && (
        <div className="wizard-card" data-testid="wizard-step-1">
          <div className="wizard-card-header">
            <h3>Step 1 — Mumble プラグイン自動配置</h3>
            <span className="platform-chip" data-testid="wizard-platform-chip">
              OS: {platform.toUpperCase()}
              {isFlatpakMumble ? ' (Flatpak 検出)' : ''}
            </span>
          </div>

          <div className="detected-paths-box">
            <span className="field-label">検出されたプラグイン配置先パス</span>
            {detectedMumblePaths.length > 0 ? (
              <ul className="detected-path-list" data-testid="detected-mumble-paths">
                {detectedMumblePaths.map((p) => (
                  <li key={p}>
                    <code>{p}</code>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="wizard-muted">標準の Mumble Plugins ディレクトリが自動検出されませんでした。</p>
            )}
          </div>

          <label className="field">
            <span className="field-label">任意の配置先フォルダ（ポータブル版・手動指定用）</span>
            <input
              id="custom-plugin-dir-input"
              data-testid="custom-plugin-dir-input"
              type="text"
              value={customPluginDir}
              placeholder="空欄の場合は自動検出パスを使用 (例: C:\Mumble\Plugins)"
              onChange={(e) => setCustomPluginDir(e.target.value)}
            />
          </label>

          <div className="wizard-actions-row">
            <button
              type="button"
              id="install-plugin-btn"
              data-testid="install-plugin-btn"
              className="btn btn-primary"
              onClick={handleInstallPlugin}
              disabled={installingPlugin}
            >
              {installingPlugin ? 'プラグイン配置中…' : 'プラグインを自動配置'}
            </button>
            <button
              type="button"
              data-testid="wizard-next-step-1"
              className="btn btn-subtle"
              onClick={() => setStep(2)}
            >
              次へ: モデル選択 →
            </button>
          </div>

          {installError && (
            <div
              className="wizard-status-banner status-error"
              data-testid="plugin-install-error"
              role="alert"
            >
              <div className="wizard-status-head">
                <strong>プラグイン配置エラー</strong>
              </div>
              <p>{installError}</p>
              <p className="wizard-guidance">
                設定パスやアクセス権限を確認したうえで「プラグインを自動配置」をもう一度お試しください。
              </p>
            </div>
          )}

          {installResult && (
            <div
              className={`wizard-status-banner status-${installResult.status}`}
              data-testid="plugin-install-status"
              role="status"
            >
              <div className="wizard-status-head">
                <strong data-testid="plugin-install-status-code">{installResult.status}</strong>
                <code>{installResult.target_path}</code>
              </div>
              <p>{installResult.message}</p>
              {installResult.status === 'locked_by_running_app' && (
                <p className="wizard-guidance" data-testid="locked-plugin-guidance">
                  Windows で Mumble が起動中の場合、DLL がロックされて置換できません。Mumble
                  を一度終了してから再実行してください。
                </p>
              )}
              {installResult.is_flatpak && installResult.flatpak_hint && (
                <p className="wizard-guidance" data-testid="flatpak-guidance">
                  {installResult.flatpak_hint}
                </p>
              )}
            </div>
          )}

          <p className="wizard-note" data-testid="windows-fullscreen-note">
            Windows 注意事項: ゲームの排他的フルスクリーン (Exclusive Fullscreen) では透過オーバーレイが背面に隠れるため、ゲーム側はボーダーレスウィンドウ表示を推奨します。
          </p>
        </div>
      )}

      {step === 2 && (
        <div className="wizard-card" data-testid="wizard-step-2">
          <div className="wizard-card-header">
            <h3>Step 2 — 翻訳モデル・エンジン選択</h3>
            <span className="platform-chip">Local NMT (SHA-256 検証付き) / Cloud API</span>
          </div>

          <div className="model-profile-grid" role="radiogroup" aria-label="Local Model Profiles">
            {MODEL_CATALOG.map((profile) => {
              const isSelected = chosenModelId === profile.model_id;
              return (
                <button
                  key={profile.model_id}
                  type="button"
                  role="radio"
                  aria-checked={isSelected}
                  data-testid={`wizard-model-card-${profile.model_id}`}
                  className={`wizard-model-card ${isSelected ? 'is-selected' : ''}`}
                  onClick={() => {
                    setChosenModelId(profile.model_id);
                    setDownloadError(null);
                  }}
                >
                  <div className="wizard-model-head">
                    <strong>{profile.display_name}</strong>
                    <span
                      className={`license-pill ${
                        profile.license_caution === 'non_commercial' ? 'is-caution' : 'is-ok'
                      }`}
                    >
                      {profile.license}
                    </span>
                  </div>
                  <p className="wizard-model-meta">
                    Device: {profile.recommended_device.toUpperCase()} · {profile.size_mb} MB ·{' '}
                    {profile.supported_langs.join(', ')}
                  </p>
                  <p className="wizard-model-summary">{profile.summary}</p>
                </button>
              );
            })}
          </div>

          {selectedModel.license_caution === 'non_commercial' && (
            <p className="caution-notice" data-testid="wizard-non-commercial-note">
              非商用ライセンス注意: {selectedModel.display_name} ({selectedModel.license})
              は非商用用途のみ利用可能です。
            </p>
          )}

          {activeProgress && (
            <div className="download-progress-box" data-testid="model-download-progress">
              <div className="download-progress-meta">
                <span>
                  {activeProgress.model_id} —{' '}
                  <strong data-testid="download-status-label">{activeProgress.status}</strong>
                </span>
                <span data-testid="download-bytes-label">
                  {formatBytesMb(activeProgress.bytes_downloaded)} /{' '}
                  {formatBytesMb(activeProgress.total_bytes)} ({progressPercent}%)
                </span>
              </div>
              <div
                className="progress-track"
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={progressPercent}
              >
                <div className="progress-fill" style={{ width: `${progressPercent}%` }} />
              </div>
              <p className="sha256-status" data-testid="sha256-verification-status">
                SHA-256 検証:{' '}
                {activeProgress.sha256_verified
                  ? '検証済み (Verified OK)'
                  : activeProgress.status === 'verifying_sha256'
                    ? 'チェックサム照合中…'
                    : activeProgress.status === 'failed'
                      ? '失敗 (Failed)'
                      : 'ダウンロード完了後に検証'}
              </p>
              {activeProgress.status === 'failed' && (
                <div
                  className="wizard-status-banner status-failed"
                  data-testid="model-download-failed-callout"
                  role="alert"
                >
                  <div className="wizard-status-head">
                    <strong>ダウンロード / 検証エラー</strong>
                  </div>
                  <p data-testid="model-download-error-message">
                    {activeProgress.error_message ||
                      downloadError ||
                      'モデルのダウンロードまたは SHA-256 検証に失敗しました。'}
                  </p>
                  <p className="wizard-guidance">
                    ネットワーク接続や空き容量を確認してから、再度「ダウンロード &amp; 検証」を実行してください。
                  </p>
                </div>
              )}
            </div>
          )}

          {downloadError && activeProgress?.status !== 'failed' && (
            <div
              className="wizard-status-banner status-failed"
              data-testid="model-download-error"
              role="alert"
            >
              <div className="wizard-status-head">
                <strong>ダウンロードエラー</strong>
              </div>
              <p>{downloadError}</p>
              <p className="wizard-guidance">
                ネットワーク状態をご確認のうえ、再度ダウンロードをお試しください。
              </p>
            </div>
          )}

          {skippedToApi && (
            <p className="wizard-guidance" data-testid="skipped-to-api-notice">
              外部 API / ローカル LLM 翻訳モードを選択しました。セットアップ完了後に設定タブからエンドポイントと API キーを入力できます。
            </p>
          )}

          <div className="wizard-actions-row">
            <button
              type="button"
              data-testid="wizard-back-step-2"
              className="btn btn-subtle"
              onClick={() => setStep(1)}
            >
              ← 戻る
            </button>
            <button
              type="button"
              id="download-model-btn"
              data-testid="download-model-btn"
              className="btn btn-primary"
              onClick={handleDownloadModel}
              disabled={downloadingModel}
            >
              {downloadingModel
                ? 'モデル導入中…'
                : `${selectedModel.model_id} をダウンロード & 検証`}
            </button>
            <button
              type="button"
              id="skip-to-api-btn"
              data-testid="skip-to-api-btn"
              className="btn btn-subtle"
              onClick={handleSkipApi}
            >
              API 翻訳を使う (スキップ)
            </button>
            <button
              type="button"
              data-testid="wizard-next-step-2"
              className="btn btn-subtle"
              onClick={() => setStep(3)}
            >
              次へ: 接続確認 →
            </button>
          </div>
        </div>
      )}

      {step === 3 && (
        <div className="wizard-card" data-testid="wizard-step-3">
          <div className="wizard-card-header">
            <h3>Step 3 — Mumble 側有効化 &amp; Heartbeat 自動検知</h3>
            <span className="platform-chip">IPC v1 Keepalive Monitor</span>
          </div>

          <ol className="wizard-guide-list" data-testid="mumble-enable-guide">
            <li>1. Mumble を起動（または再起動）し、「設定 (Configure) → プラグイン (Plugins)」を開きます。</li>
            <li>2. プラグイン一覧から「Kotoha Voice Translator」にチェックを入れて有効化します。</li>
            <li>3. サーバーへ接続すると、下の Heartbeat インジケーターが自動的に緑色の接続状態へ切り替わります。</li>
          </ol>

          <div
            className={`heartbeat-panel ${isHeartbeatDetected ? 'is-connected' : 'is-waiting'}`}
            data-testid="heartbeat-status-indicator"
          >
            {isHeartbeatDetected ? (
              <div className="heartbeat-connected" data-testid="heartbeat-connected-badge">
                <span className="heartbeat-dot-ok" aria-hidden="true" />
                <div>
                  <strong>Heartbeat 受信中 (接続確認 OK)</strong>
                  <p className="heartbeat-meta">
                    source: <code>{connection.source}</code> · plugin_version:{' '}
                    <code>{connection.plugin_version ?? 'unknown'}</code> · last_heartbeat:{' '}
                    {connection.last_heartbeat_ms} ms
                  </p>
                </div>
              </div>
            ) : (
              <div className="heartbeat-waiting" data-testid="heartbeat-waiting-spinner">
                <span className="spinner-ring" aria-hidden="true" />
                <div>
                  <strong>Heartbeat 待機中… ({connection.state})</strong>
                  <p className="heartbeat-meta">
                    Mumble でプラグインを有効化すると 1 秒周期の Heartbeat を自動検知します。
                  </p>
                </div>
              </div>
            )}
          </div>

          <p className="wizard-note">
            ヒント: ゲーム画面上で字幕を重ねる際は、ゲーム側はボーダーレスウィンドウ表示を推奨します。
          </p>

          {completeError && (
            <div
              className="wizard-status-banner status-error"
              data-testid="wizard-complete-error"
              role="alert"
            >
              <div className="wizard-status-head">
                <strong>セットアップ完了エラー</strong>
              </div>
              <p>{completeError}</p>
              <p className="wizard-guidance">
                接続状態をご確認のうえ、再度「セットアップを完了」をお試しください。
              </p>
            </div>
          )}

          <div className="wizard-actions-row">
            <button
              type="button"
              data-testid="wizard-back-step-3"
              className="btn btn-subtle"
              onClick={() => setStep(2)}
            >
              ← 戻る
            </button>
            <button
              type="button"
              id="complete-wizard-btn"
              data-testid="complete-wizard-btn"
              className="btn btn-primary"
              onClick={handleComplete}
              disabled={completing}
            >
              {completing ? '完了処理中…' : 'セットアップを完了'}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
