import { expect, test } from '@playwright/test';

test.describe('Kotoha UI E2E', () => {
  test('renders 3-stage subtitle overlay, switches providers/models/glossary, and walks through 3-step Setup Wizard with zero console errors', async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];

    page.on('console', (msg) => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
      }
    });
    page.on('pageerror', (err) => {
      pageErrors.push(err.message);
    });

    await page.goto('/');

    // Fresh state has wizard_completed=false, so App opens on Setup Wizard automatically
    await expect(page.getByTestId('setup-wizard')).toBeVisible();
    await expect(page.getByTestId('wizard-step-1')).toBeVisible();
    await expect(page.getByTestId('windows-fullscreen-note')).toContainText(
      'ボーダーレスウィンドウ表示を推奨',
    );

    // Walkthrough 3-step Setup Wizard
    await page.getByTestId('install-plugin-btn').click();
    await expect(page.getByTestId('plugin-install-status-code')).toHaveText('installed');

    await page.getByTestId('wizard-next-step-1').click();
    await expect(page.getByTestId('wizard-step-2')).toBeVisible();
    await page.getByTestId('download-model-btn').click();
    await expect(page.getByTestId('sha256-verification-status')).toContainText('Verified OK', {
      timeout: 5000,
    });

    await page.getByTestId('wizard-next-step-2').click();
    await expect(page.getByTestId('wizard-step-3')).toBeVisible();
    await expect(page.getByTestId('heartbeat-connected-badge')).toBeVisible();
    await page.getByTestId('complete-wizard-btn').click();

    // Completing wizard transitions to Overlay Preview and renders 3-stage subtitles
    await expect(page.getByTestId('overlay-preview-stage')).toBeVisible();
    await expect(page.getByTestId('subtitle-overlay')).toBeVisible();
    await expect(page.getByTestId('subtitle-entry-101')).toHaveAttribute(
      'data-stage',
      'translated',
    );
    await expect(page.getByTestId('subtitle-entry-102')).toHaveAttribute(
      'data-stage',
      'final_pending_translation',
    );
    await expect(page.getByTestId('subtitle-entry-103')).toHaveAttribute(
      'data-stage',
      'interim',
    );

    // Run live 3-stage utterance simulation
    await page.getByTestId('simulate-utterance-btn').click();
    await expect(page.getByTestId('subtitle-entry-104')).toHaveAttribute(
      'data-stage',
      'interim',
    );
    await expect(page.getByTestId('subtitle-entry-104')).toHaveAttribute(
      'data-stage',
      'translated',
      { timeout: 5000 },
    );

    // Switch to Settings & test model profile + compute device + provider switching + glossary
    await page.getByTestId('mode-settings-btn').click();
    await expect(page.getByTestId('translation-settings')).toBeVisible();

    await expect(page.getByTestId('glossary-hotwords-section')).toBeVisible();
    await page.getByTestId('glossary-toggle-fps-callouts').click();
    await expect(page.getByTestId('glossary-toggle-fps-callouts')).toBeChecked();
    await page.getByTestId('hotwords-score-slider').fill('2.3');
    await expect(page.getByTestId('hotwords-score-label')).toContainText('2.3');

    await page
      .getByTestId('local-model-select')
      .selectOption('nllb-200-distilled-600m-int8');
    await page.getByTestId('compute-device-select').selectOption('metal');
    await expect(page.getByTestId('non-commercial-warning')).toBeVisible();
    await expect(page.getByTestId('active-engine-summary')).toContainText(
      'NLLB-200 Distilled 600M (Multilingual INT8, Non-Commercial) · METAL',
    );

    await page.getByTestId('provider-tab-openai_compatible').click();
    await expect(page.getByTestId('config-openai-compatible')).toBeVisible();

    await page.getByTestId('provider-tab-deepl').click();
    await expect(page.getByTestId('config-deepl')).toBeVisible();

    // Secrets are write-only: the badge reflects status and the value is never rendered back.
    const secretValue = 'e2e-secret-deepl-key';
    await expect(page.getByTestId('secret-badge-deep_l_api_key')).toHaveText('未設定');
    await page.locator('#deepl-api-key').fill(secretValue);
    await page.getByTestId('secret-set-deep_l_api_key').click();
    await expect(page.getByTestId('secret-badge-deep_l_api_key')).toHaveText('設定済み');
    await expect(page.locator('#deepl-api-key')).toHaveValue('');
    expect(await page.content()).not.toContain(secretValue);
    await page.getByTestId('secret-clear-deep_l_api_key').click();
    await expect(page.getByTestId('secret-badge-deep_l_api_key')).toHaveText('未設定');

    await page.getByTestId('provider-tab-google_cloud').click();
    await expect(page.getByTestId('config-google-cloud')).toBeVisible();

    await page.getByTestId('provider-tab-gas_webhook').click();
    await expect(page.getByTestId('config-gas-webhook')).toBeVisible();

    await page.getByTestId('provider-tab-mock').click();
    await expect(page.getByTestId('config-mock')).toBeVisible();

    // Switch back to Overlay Preview
    await page.getByTestId('mode-overlay-btn').click();
    await expect(page.getByTestId('subtitle-overlay')).toBeVisible();

    expect(consoleErrors).toEqual([]);
    expect(pageErrors).toEqual([]);
  });

  test('renders pure transparent ?window=overlay mode without header/chrome and zero console errors', async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];

    page.on('console', (msg) => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
      }
    });
    page.on('pageerror', (err) => {
      pageErrors.push(err.message);
    });

    await page.goto('/?window=overlay');

    await expect(page.getByTestId('pure-overlay-window')).toBeVisible();
    await expect(page.getByTestId('subtitle-overlay')).toBeVisible();
    await expect(page.getByTestId('subtitle-overlay')).toHaveClass(/is-frameless/);
    await expect(page.getByTestId('mode-switcher')).toHaveCount(0);
    await expect(page.getByTestId('mode-settings-btn')).toHaveCount(0);
    await expect(page.getByTestId('overlay-drag-bar')).toHaveCount(0);

    // Real ?window=overlay initializes with empty subtitles (no dummy demo subtitles)
    await expect(page.getByTestId('overlay-empty-state')).toBeVisible();
    await expect(page.getByTestId('subtitle-entry-101')).toHaveCount(0);

    const bodyBg = await page.evaluate(() => {
      return window.getComputedStyle(document.body).backgroundColor;
    });
    expect(bodyBg).toBe('rgba(0, 0, 0, 0)');

    expect(consoleErrors).toEqual([]);
    expect(pageErrors).toEqual([]);
  });
});
