import {
  COMPUTE_DEVICES,
  MODEL_CATALOG,
  PROVIDER_METADATA,
  createDefaultProviderConfig,
  findModelProfile,
} from '../data/modelCatalog';
import type {
  ComputeDevice,
  DeepLPlan,
  GoogleTranslateApiVersion,
  OverlayDisplayOptions,
  TranslationProviderConfig,
  TranslationProviderType,
} from '../types/kotoha';

export interface TranslationSettingsProps {
  srcLang: string;
  tgtLang: string;
  provider: TranslationProviderConfig;
  overlayOptions: OverlayDisplayOptions;
  onChangeLanguages: (srcLang: string, tgtLang: string) => void;
  onChangeProvider: (config: TranslationProviderConfig) => void;
  onChangeOverlayOptions: (options: OverlayDisplayOptions) => void;
}

const LANGUAGE_OPTIONS = [
  { code: 'en', label: 'English (en)' },
  { code: 'ja', label: 'Japanese (ja)' },
  { code: 'zh', label: 'Chinese (zh)' },
  { code: 'ko', label: 'Korean (ko)' },
] as const;

export function TranslationSettings({
  srcLang,
  tgtLang,
  provider,
  overlayOptions,
  onChangeLanguages,
  onChangeProvider,
  onChangeOverlayOptions,
}: TranslationSettingsProps) {
  const handleProviderSelect = (nextType: TranslationProviderType) => {
    if (nextType !== provider.type) {
      onChangeProvider(createDefaultProviderConfig(nextType));
    }
  };

  const selectedModel =
    provider.type === 'local_model'
      ? findModelProfile(provider.model_id)
      : undefined;

  return (
    <section
      className="settings-panel"
      aria-label="Translation and overlay settings"
      data-testid="translation-settings"
    >
      <div className="settings-section">
        <h2 className="section-title">Language Pair</h2>
        <div className="field-grid two-col">
          <label className="field">
            <span className="field-label">Source Language (`src_lang`)</span>
            <select
              id="src-lang-select"
              value={srcLang}
              onChange={(e) => onChangeLanguages(e.target.value, tgtLang)}
            >
              {LANGUAGE_OPTIONS.map((lang) => (
                <option key={lang.code} value={lang.code}>
                  {lang.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field-label">Target Language (`tgt_lang`)</span>
            <select
              id="tgt-lang-select"
              value={tgtLang}
              onChange={(e) => onChangeLanguages(srcLang, e.target.value)}
            >
              {LANGUAGE_OPTIONS.map((lang) => (
                <option key={lang.code} value={lang.code}>
                  {lang.label}
                </option>
              ))}
            </select>
          </label>
        </div>
      </div>

      <div className="settings-section">
        <h2 className="section-title">Translation Provider</h2>
        <div className="provider-tabs" role="radiogroup" aria-label="Translation Provider">
          {PROVIDER_METADATA.map((item) => {
            const active = provider.type === item.type;
            return (
              <button
                key={item.type}
                type="button"
                id={`provider-tab-${item.type}`}
                data-testid={`provider-tab-${item.type}`}
                role="radio"
                aria-checked={active}
                className={`provider-tab ${active ? 'is-active' : ''}`}
                onClick={() => handleProviderSelect(item.type)}
              >
                <span className="provider-tab-title">{item.label}</span>
                <span className="provider-tab-badge">{item.badge}</span>
              </button>
            );
          })}
        </div>

        <p className="provider-description">
          {PROVIDER_METADATA.find((p) => p.type === provider.type)?.description}
        </p>

        {provider.type === 'local_model' && selectedModel && (
          <div className="provider-config" data-testid="config-local-model">
            <div className="field-grid two-col">
              <label className="field">
                <span className="field-label">Model Profile</span>
                <select
                  id="local-model-select"
                  data-testid="local-model-select"
                  value={provider.model_id}
                  onChange={(e) => {
                    const nextProfile = findModelProfile(e.target.value);
                    onChangeProvider({
                      ...provider,
                      model_id: nextProfile.model_id,
                      compute_type: nextProfile.compute_type,
                    });
                  }}
                >
                  {MODEL_CATALOG.map((profile) => (
                    <option key={profile.model_id} value={profile.model_id}>
                      {profile.display_name} ({profile.size_mb} MB · {profile.license})
                    </option>
                  ))}
                </select>
              </label>

              <label className="field">
                <span className="field-label">Compute Device</span>
                <select
                  id="compute-device-select"
                  data-testid="compute-device-select"
                  value={provider.device}
                  onChange={(e) =>
                    onChangeProvider({
                      ...provider,
                      device: e.target.value as ComputeDevice,
                    })
                  }
                >
                  {COMPUTE_DEVICES.map((dev) => (
                    <option key={dev.value} value={dev.value}>
                      {dev.label} — {dev.hint}
                    </option>
                  ))}
                </select>
              </label>
            </div>

            <div className="model-meta-card" data-testid="selected-model-meta">
              <div className="model-meta-header">
                <strong>{selectedModel.display_name}</strong>
                <span
                  className={`license-pill ${
                    selectedModel.license_caution === 'non_commercial'
                      ? 'is-caution'
                      : 'is-ok'
                  }`}
                  data-testid="model-license-badge"
                >
                  {selectedModel.license}
                </span>
              </div>
              <p className="model-meta-summary">{selectedModel.summary}</p>
              <dl className="model-meta-specs">
                <div>
                  <dt>Model ID</dt>
                  <dd>
                    <code>{selectedModel.model_id}</code>
                  </dd>
                </div>
                <div>
                  <dt>Size</dt>
                  <dd data-testid="model-size-value">
                    {selectedModel.size_mb >= 1000
                      ? `${(selectedModel.size_mb / 1000).toFixed(1)} GB`
                      : `${selectedModel.size_mb} MB`}
                  </dd>
                </div>
                <div>
                  <dt>Recommended</dt>
                  <dd>{selectedModel.recommended_device.toUpperCase()}</dd>
                </div>
                <div>
                  <dt>Precision</dt>
                  <dd>{selectedModel.compute_type}</dd>
                </div>
              </dl>
              {selectedModel.license_caution === 'non_commercial' && (
                <p className="caution-notice" data-testid="non-commercial-warning">
                  Non-Commercial Notice: {selectedModel.license} prohibits commercial use.
                  Kotoha downloads this model separately on demand.
                </p>
              )}
            </div>
          </div>
        )}

        {provider.type === 'openai_compatible' && (
          <div className="provider-config" data-testid="config-openai-compatible">
            <div className="field-grid two-col">
              <label className="field">
                <span className="field-label">Base URL</span>
                <input
                  id="openai-base-url"
                  type="url"
                  value={provider.base_url}
                  placeholder="https://api.openai.com/v1 or http://localhost:11434/v1"
                  onChange={(e) =>
                    onChangeProvider({ ...provider, base_url: e.target.value })
                  }
                />
              </label>
              <label className="field">
                <span className="field-label">Model Name</span>
                <input
                  id="openai-model-name"
                  type="text"
                  value={provider.model}
                  placeholder="gpt-4o-mini / qwen2.5:7b"
                  onChange={(e) =>
                    onChangeProvider({ ...provider, model: e.target.value })
                  }
                />
              </label>
            </div>
            <div className="field-grid two-col">
              <label className="field">
                <span className="field-label">API Key (Optional for Local Ollama)</span>
                <input
                  id="openai-api-key"
                  type="password"
                  value={provider.api_key}
                  placeholder="Stored in OS keychain at runtime"
                  onChange={(e) =>
                    onChangeProvider({ ...provider, api_key: e.target.value })
                  }
                />
              </label>
              <label className="field">
                <span className="field-label">Context Window (5–10 turns)</span>
                <input
                  id="openai-context-window"
                  type="number"
                  min={5}
                  max={10}
                  value={provider.context_window}
                  onChange={(e) =>
                    onChangeProvider({
                      ...provider,
                      context_window: Math.min(10, Math.max(5, Number(e.target.value) || 8)),
                    })
                  }
                />
              </label>
            </div>
          </div>
        )}

        {provider.type === 'deepl' && (
          <div className="provider-config" data-testid="config-deepl">
            <div className="field-grid two-col">
              <label className="field">
                <span className="field-label">DeepL Plan</span>
                <select
                  id="deepl-plan-select"
                  value={provider.plan}
                  onChange={(e) =>
                    onChangeProvider({
                      ...provider,
                      plan: e.target.value as DeepLPlan,
                    })
                  }
                >
                  <option value="free">Free (api-free.deepl.com)</option>
                  <option value="pro">Pro (api.deepl.com)</option>
                </select>
              </label>
              <label className="field">
                <span className="field-label">Formality</span>
                <select
                  id="deepl-formality-select"
                  value={provider.formality}
                  onChange={(e) =>
                    onChangeProvider({ ...provider, formality: e.target.value })
                  }
                >
                  <option value="default">Default</option>
                  <option value="more">More Formal</option>
                  <option value="less">Less Formal</option>
                </select>
              </label>
            </div>
            <label className="field">
              <span className="field-label">DeepL API Key</span>
              <input
                id="deepl-api-key"
                type="password"
                value={provider.api_key}
                placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx:fx"
                onChange={(e) =>
                  onChangeProvider({ ...provider, api_key: e.target.value })
                }
              />
            </label>
          </div>
        )}

        {provider.type === 'google_cloud' && (
          <div className="provider-config" data-testid="config-google-cloud">
            <div className="field-grid two-col">
              <label className="field">
                <span className="field-label">API Version</span>
                <select
                  id="google-api-version"
                  value={provider.api_version}
                  onChange={(e) =>
                    onChangeProvider({
                      ...provider,
                      api_version: e.target.value as GoogleTranslateApiVersion,
                    })
                  }
                >
                  <option value="v2">v2 Basic REST</option>
                  <option value="v3">v3 Advanced (Project ID)</option>
                </select>
              </label>
              <label className="field">
                <span className="field-label">Project ID (v3 only)</span>
                <input
                  id="google-project-id"
                  type="text"
                  value={provider.project_id}
                  disabled={provider.api_version !== 'v3'}
                  placeholder="my-gcp-project"
                  onChange={(e) =>
                    onChangeProvider({ ...provider, project_id: e.target.value })
                  }
                />
              </label>
            </div>
            <label className="field">
              <span className="field-label">Google Cloud API Key</span>
              <input
                id="google-api-key"
                type="password"
                value={provider.api_key}
                placeholder="AIza..."
                onChange={(e) =>
                  onChangeProvider({ ...provider, api_key: e.target.value })
                }
              />
            </label>
          </div>
        )}

        {provider.type === 'gas_webhook' && (
          <div className="provider-config" data-testid="config-gas-webhook">
            <label className="field">
              <span className="field-label">GAS Web App / HTTP Proxy URL</span>
              <input
                id="gas-endpoint-url"
                type="url"
                value={provider.endpoint_url}
                placeholder="https://script.google.com/macros/s/.../exec"
                onChange={(e) =>
                  onChangeProvider({ ...provider, endpoint_url: e.target.value })
                }
              />
            </label>
            <label className="field">
              <span className="field-label">Bearer Token (Optional)</span>
              <input
                id="gas-bearer-token"
                type="password"
                value={provider.bearer_token}
                placeholder="Optional shared secret"
                onChange={(e) =>
                  onChangeProvider({ ...provider, bearer_token: e.target.value })
                }
              />
            </label>
          </div>
        )}

        {provider.type === 'mock' && (
          <div className="provider-config" data-testid="config-mock">
            <label className="field">
              <span className="field-label">Simulated Latency (ms)</span>
              <input
                id="mock-latency-ms"
                type="number"
                min={0}
                max={2000}
                value={provider.latency_ms}
                onChange={(e) =>
                  onChangeProvider({
                    ...provider,
                    latency_ms: Math.max(0, Number(e.target.value) || 0),
                  })
                }
              />
            </label>
          </div>
        )}
      </div>

      <div className="settings-section">
        <h2 className="section-title">Overlay Appearance</h2>
        <div className="field-grid two-col">
          <label className="field">
            <span className="field-label">Max Visible Lines ({overlayOptions.max_lines})</span>
            <input
              id="overlay-max-lines"
              type="range"
              min={1}
              max={6}
              value={overlayOptions.max_lines}
              onChange={(e) =>
                onChangeOverlayOptions({
                  ...overlayOptions,
                  max_lines: Number(e.target.value),
                })
              }
            />
          </label>
          <label className="field">
            <span className="field-label">
              Font Size ({overlayOptions.font_size_px}px)
            </span>
            <input
              id="overlay-font-size"
              type="range"
              min={14}
              max={28}
              value={overlayOptions.font_size_px}
              onChange={(e) =>
                onChangeOverlayOptions({
                  ...overlayOptions,
                  font_size_px: Number(e.target.value),
                })
              }
            />
          </label>
        </div>
        <div className="toggle-row">
          <label className="checkbox-field">
            <input
              id="show-source-text-checkbox"
              type="checkbox"
              checked={overlayOptions.show_source_text}
              onChange={(e) =>
                onChangeOverlayOptions({
                  ...overlayOptions,
                  show_source_text: e.target.checked,
                })
              }
            />
            <span>Show original source text beneath translated subtitle</span>
          </label>
          <label className="checkbox-field">
            <input
              id="lock-overlay-checkbox"
              type="checkbox"
              checked={overlayOptions.locked}
              onChange={(e) =>
                onChangeOverlayOptions({
                  ...overlayOptions,
                  locked: e.target.checked,
                })
              }
            />
            <span>Lock overlay for mouse click-through</span>
          </label>
        </div>
      </div>
    </section>
  );
}
