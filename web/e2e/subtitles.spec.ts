import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

// OpenSubtitles is replaced by e2e/opensubtitles-stub.mjs (see playwright.config.ts),
// which accepts the API key "e2e-key".
const TITLE = 'Untertiteltest'
const FILE = `${TITLE}.2021.German.1080p.WEB.x264-E2E.mkv`

test.describe.configure({ mode: 'serial' })

async function api(page: Page, path: string, method = 'GET'): Promise<unknown> {
  return page.evaluate(
    async ([url, verb]) => {
      const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
      const response = await fetch(url, {
        method: verb,
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: verb === 'POST' ? '{}' : undefined,
      })
      return response.json()
    },
    [path, method],
  )
}

test('OpenSubtitles key is checked and stored without being shown', async ({ page }) => {
  await page.goto('/settings?tab=subtitles')
  await expect(page.getByTestId('opensubtitles-status')).toContainText('Kein Schlüssel')
  await page.getByRole('textbox', { name: 'API-Schlüssel' }).fill('falsch')
  await page.getByRole('button', { name: 'Prüfen & speichern' }).click()
  await expect(page.getByRole('alert')).toContainText('ungültig')

  await page.getByRole('textbox', { name: 'API-Schlüssel' }).fill('e2e-key')
  await page.getByRole('button', { name: 'Prüfen & speichern' }).click()
  await expect(page.getByTestId('opensubtitles-status')).toContainText('Schlüssel hinterlegt')
  const settings = await api(page, '/api/v1/settings/subtitles')
  expect(JSON.stringify(settings)).not.toContain('e2e-key')

  // Automatic downloads: a language first, then the switch.
  await page.getByRole('button', { name: 'Englisch' }).click()
  await expect(page.getByRole('button', { name: 'Englisch' })).toHaveAttribute('aria-pressed', 'true')
  await page.getByLabel('Untertitel für neue Inhalte automatisch laden').click()
  await expect(page.getByLabel('Untertitel für neue Inhalte automatisch laden')).toBeChecked()
  await page.getByLabel('Untertitel für neue Inhalte automatisch laden').click()
  await expect(page.getByLabel('Untertitel für neue Inhalte automatisch laden')).not.toBeChecked()
})

test('search and download a subtitle from the movie page', async ({ page }) => {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-subs-'))
  mkdirSync(join(root, `${TITLE} (2021)`))
  writeFileSync(join(root, `${TITLE} (2021)`, FILE), Buffer.alloc(200_000))
  // A backdrop, so the home hero (the newest items with one) never ends up empty
  // because of this movie (browse.spec.ts runs in parallel).
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=c=0x14532d:s=640x360',
    '-frames:v', '1', join(root, `${TITLE} (2021)`, 'fanart.jpg'),
  ])

  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('Untertitel')
  await dialog.getByLabel('Ordnerpfad').fill(root)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: 'Untertitel' })
  await expect(entry.getByTestId('library-counts')).toContainText('1 Film', { timeout: 30_000 })

  await page.goto(`/search?q=${TITLE}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click()
  await page.getByRole('button', { name: 'Untertitel suchen' }).click()
  const search = page.getByRole('dialog', { name: 'Untertitel suchen' })
  const results = search.getByTestId('subtitle-result')
  await expect(results).toHaveCount(2)
  // The release that fits the file name comes first despite fewer downloads.
  await expect(results.first()).toContainText('Untertiteltest.2021.German.1080p.WEB.x264-E2E')
  await expect(results.first()).toContainText('Passung 95 %')

  await results.first().getByRole('button', { name: 'Herunterladen' }).click()
  await expect(search.getByRole('status')).toContainText('Untertitel gespeichert')
  await expect(results.first().getByRole('button', { name: 'Gespeichert' })).toBeDisabled()

  // The track is part of the file's streams at once (playback info reads the same
  // streams; it is not called here because it would open a transcode session that
  // playback.spec.ts does not expect).
  const itemId = page.url().split('/').pop()
  const item = (await api(page, `/api/v1/items/${itemId}`)) as {
    files: { subtitles: { index: number; language: string; is_external: boolean }[] }[]
  }
  const track = item.files[0].subtitles.find((t) => t.is_external)
  expect(track?.language).toBe('deu')
  expect(track?.index).toBe(1000)
})
