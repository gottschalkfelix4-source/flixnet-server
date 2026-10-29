import { expect, test } from '@playwright/test'

test('admin changes metadata language and region', async ({ page }) => {
  await page.goto('/settings?tab=metadata')
  await expect(page.getByTestId('tmdb-status')).toContainText('Kein Schlüssel')

  await page.getByLabel('Sprache der Metadaten').selectOption('en-US')
  await page.getByLabel('Altersfreigaben aus').selectOption('AT')
  await page.reload()
  await expect(page.getByLabel('Sprache der Metadaten')).toHaveValue('en-US')
  await expect(page.getByLabel('Altersfreigaben aus')).toHaveValue('AT')

  // Restore defaults for other tests sharing the server.
  await page.getByLabel('Sprache der Metadaten').selectOption('de-DE')
  await page.getByLabel('Altersfreigaben aus').selectOption('DE')
})
