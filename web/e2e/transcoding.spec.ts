import { expect, test } from '@playwright/test'

test('admin sees the hardware transcoding diagnostics', async ({ page }) => {
  await page.goto('/settings?tab=transcoding')
  await expect(page.getByRole('heading', { name: 'Hardware-Transcoding' })).toBeVisible()
  const info = page.getByTestId('transcoding-info')
  await expect(info.getByText('Automatisch (beste verfügbare)')).toBeVisible()
  // Detection runs in the background after startup; the page polls until it is done.
  await expect(page.getByRole('heading', { name: 'Getestete Verfahren' })).toBeVisible({ timeout: 60_000 })
  await expect(info.getByText('Hardware wird getestet …')).toBeHidden()
  await expect(info.getByText(/^ffmpeg version/)).toBeVisible()
})
