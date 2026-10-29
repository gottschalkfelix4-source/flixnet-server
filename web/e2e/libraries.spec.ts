import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from '@playwright/test'

/** A tiny real movie file (needs ffmpeg on PATH, like the server's ffprobe). */
function movieLibrary(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-movies-'))
  const folder = join(root, 'Mein Testfilm (2020)')
  mkdirSync(folder, { recursive: true })
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc=s=320x240:r=25:d=1',
    '-c:v', 'mpeg4',
    join(folder, 'Mein.Testfilm.2020.German.1080p.WEB.x264-FLX.mkv'),
  ])
  return root
}

test('admin adds a movie library and it gets scanned and analysed', async ({ page }) => {
  const root = movieLibrary()
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()

  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('E2E Filme')
  await dialog.getByLabel('Ordnerpfad').fill(root)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()

  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: 'E2E Filme' })
  // Scan (creates the movie) and probe (analysis) both run in the background.
  await expect(entry.getByTestId('library-counts')).toContainText('1 Film · 1 Dateien', { timeout: 15_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 15_000 })
  await expect(entry.getByTestId('library-counts')).toContainText('gescannt')
})

test('folder picker browses server folders', async ({ page }) => {
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Durchsuchen' }).click()
  const folders = page.getByRole('list', { name: 'Ordner' })
  await expect(folders.getByRole('button').first()).toBeVisible()
  await folders.getByRole('button').first().click()
  await expect(page.getByTestId('picker-path')).not.toHaveText('Laufwerke')
})
