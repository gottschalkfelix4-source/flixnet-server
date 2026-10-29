import { expect, test } from '@playwright/test'

test('admin runs a scheduled task and sees it finish', async ({ page }) => {
  await page.goto('/settings?tab=jobs')
  await expect(page.getByRole('heading', { name: 'Geplante Aufgaben' })).toBeVisible()
  await page.getByRole('button', { name: 'Jetzt ausführen' }).first().click()

  const list = page.getByTestId('job-list')
  await expect(list.getByText('Aufgabenverlauf aufräumen').first()).toBeVisible()
  // Real workers run in the E2E server, so the job completes.
  await expect(list.getByText('Erledigt').first()).toBeVisible({ timeout: 10_000 })
})
