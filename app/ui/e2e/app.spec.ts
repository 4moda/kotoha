import { expect, test } from '@playwright/test';

test.describe('Kotoha UI E2E', () => {
  test('renders 3-stage subtitle overlay, switches providers/models, with zero console errors', async ({
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

    // Verify 3-stage overlay rendering
    await expect(page.getByTestId('subtitle-overlay')).toBeVisible();
    await expect(page.getByTestId('subtitle-entry-101')).toHaveAttribute(
      'data-stage',
      'translated',
    );
    await expect(page.getByTestId('subtitle-entry-102')).toHaveAttribute(
      'data-stage',
      'final_pending',
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

    // Switch to Settings & test model profile + compute device + provider switching
    await page.getByTestId('mode-settings-btn').click();
    await expect(page.getByTestId('translation-settings')).toBeVisible();

    await page
      .getByTestId('local-model-select')
      .selectOption('nllb-200-distilled-600m-int8');
    await page.getByTestId('compute-device-select').selectOption('metal');
    await expect(page.getByTestId('non-commercial-warning')).toBeVisible();
    await expect(page.getByTestId('active-engine-summary')).toContainText(
      'NLLB-200 Distilled 600M (int8) · METAL',
    );

    await page.getByTestId('provider-tab-openai_compatible').click();
    await expect(page.getByTestId('config-openai-compatible')).toBeVisible();

    await page.getByTestId('provider-tab-deepl').click();
    await expect(page.getByTestId('config-deepl')).toBeVisible();

    await page.getByTestId('provider-tab-google_cloud').click();
    await expect(page.getByTestId('config-google-cloud')).toBeVisible();

    await page.getByTestId('provider-tab-gas_webhook').click();
    await expect(page.getByTestId('config-gas-webhook')).toBeVisible();

    // Switch back to Overlay Preview
    await page.getByTestId('mode-overlay-btn').click();
    await expect(page.getByTestId('subtitle-overlay')).toBeVisible();

    expect(consoleErrors).toEqual([]);
    expect(pageErrors).toEqual([]);
  });
});
