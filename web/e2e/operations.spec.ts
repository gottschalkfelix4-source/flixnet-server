import { type Page, expect, test } from '@playwright/test'

// Webhooks go to e2e/webhook-stub.mjs (see playwright.config.ts).
const webhookPort = Number(process.env.FLIXNET_E2E_PORT ?? 18097) + 1002
const stub = `http://127.0.0.1:${webhookPort}`

async function received(page: Page): Promise<{ path: string; event: string; signature: string | null; body: { title: string } }[]> {
  const response = await page.request.get(`${stub}/received`)
  return response.json()
}

test('dashboard shows load, storage and jobs', async ({ page }) => {
  await page.goto('/settings?tab=dashboard')
  const dashboard = page.getByTestId('dashboard')
  await expect(dashboard.getByText('Arbeitsspeicher')).toBeVisible()
  await expect(dashboard.getByText('Konfiguration', { exact: true })).toBeVisible()
  await expect(dashboard.getByText('Wiedergaben & Bandbreite')).toBeVisible()
  await dashboard.getByRole('link', { name: 'Protokoll ansehen' }).click()
  await expect(page).toHaveURL(/tab=logs/)
  await expect(page.getByTestId('log-lines')).toContainText('Flixnet is listening')
  await page.getByRole('combobox', { name: 'Mindest-Level' }).selectOption('error')
  await expect(page.getByTestId('log-lines')).not.toContainText('Flixnet is listening')
})

test('a webhook target receives a signed test message', async ({ page }) => {
  await page.goto('/settings?tab=notifications')
  await page.getByRole('button', { name: 'Ziel hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('textbox', { name: 'Name' }).fill('E2E-Hook')
  await dialog.getByRole('textbox', { name: 'Adresse' }).fill(`${stub}/hook`)
  await dialog.getByLabel('Signatur-Geheimnis (optional)').fill('e2e-geheim')
  await dialog.getByRole('checkbox', { name: 'Wiedergabe gestartet' }).check()
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()

  const row = page.getByTestId('notification-target').filter({ hasText: 'E2E-Hook' })
  await expect(row).toContainText('5 Ereignisse')
  await row.getByRole('button', { name: 'Test senden' }).click()
  await expect(row.getByTestId('test-result')).toHaveText('Testnachricht wurde zugestellt.')
  const hooks = (await received(page)).filter((r) => r.path === '/hook')
  expect(hooks.at(-1)?.event).toBe('test')
  expect(hooks.at(-1)?.body.title).toBe('Testnachricht von Flixnet')
  expect(hooks.at(-1)?.signature).toMatch(/^t=\d+,v1=[0-9a-f]{64}$/)

  // A failing endpoint shows the reason.
  await page.getByRole('button', { name: 'Ziel hinzufügen' }).click()
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Kaputt')
  await dialog.getByRole('textbox', { name: 'Adresse' }).fill(`${stub}/broken`)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  const broken = page.getByTestId('notification-target').filter({ hasText: 'Kaputt' })
  await broken.getByRole('button', { name: 'Test senden' }).click()
  await expect(broken.getByTestId('test-result')).toContainText('HTTP 500')
})

test('backups can be made on demand', async ({ page }) => {
  await page.goto('/settings?tab=backup')
  await expect(page.getByText('Noch keine Sicherung vorhanden.')).toBeVisible()
  await page.getByRole('button', { name: 'Jetzt sichern' }).click()
  await expect(page.getByRole('status')).toContainText('Sicherung flixnet-backup-')
  await expect(page.getByTestId('backup-list').getByRole('listitem')).toHaveCount(1)
  await expect(page.getByTestId('restore-command')).toContainText('flixnet-server restore')
})

test('Trakt needs the app registered first', async ({ page }) => {
  await page.goto('/settings?tab=trakt')
  await expect(page.getByText('Trakt ist auf diesem Server noch nicht eingerichtet')).toBeVisible()
  await page.getByRole('textbox', { name: 'Client ID' }).fill('e2e-client')
  await page.getByLabel('Client Secret').fill('e2e-secret')
  await page.getByRole('button', { name: 'Speichern' }).click()
  await expect(page.getByRole('button', { name: 'Mit Trakt verbinden' })).toBeVisible()
  await expect(page.getByLabel('Neues Client Secret (gespeichert)')).toBeVisible()
})

test('single sign-on settings and import preview validate input', async ({ page }) => {
  await page.goto('/settings?tab=sso')
  await expect(page.getByText('/login/oidc')).toBeVisible()
  await page.getByRole('checkbox', { name: 'Anmeldung über OpenID Connect anbieten' }).check()
  await page.getByRole('button', { name: 'Speichern' }).click()
  await expect(page.getByRole('alert')).toContainText('Issuer-Adresse und Client-ID')

  await page.goto('/settings?tab=import')
  await page.getByRole('textbox', { name: 'Server-Adresse' }).fill(`${stub}/plex`)
  await page.getByLabel('Plex-Token').fill('kein-plex')
  await page.getByRole('button', { name: 'Vorschau' }).click()
  // The stub answers 204 without JSON: reported as an invalid answer, nothing written.
  await expect(page.getByRole('alert')).toContainText('Plex')
})
