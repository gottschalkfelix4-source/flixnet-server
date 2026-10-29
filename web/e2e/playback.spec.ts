import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from '@playwright/test'

// Playwright's Chromium has no proprietary codecs, so the playable file is VP9/WebM;
// MPEG-4 Part 2 in Matroska stands in for "needs transcoding".
const PLAYABLE = 'Abspieltest'
const UNPLAYABLE = 'Formatfehler'

function encode(path: string, codec: string[]) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc=s=320x240:r=25:d=3',
    ...codec, path,
  ])
}

function libraryRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-play-'))
  mkdirSync(join(root, `${PLAYABLE} (2021)`))
  encode(join(root, `${PLAYABLE} (2021)`, `${PLAYABLE}.2021.webm`), ['-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-b:v', '200k'])
  mkdirSync(join(root, `${UNPLAYABLE} (2021)`))
  encode(join(root, `${UNPLAYABLE} (2021)`, `${UNPLAYABLE}.2021.mkv`), ['-c:v', 'mpeg4'])
  return root
}

test.describe.configure({ mode: 'serial' })

test('library with playable files gets scanned', async ({ page }) => {
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('Wiedergabe')
  await dialog.getByLabel('Ordnerpfad').fill(libraryRoot())
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: 'Wiedergabe' })
  await expect(entry.getByTestId('library-counts')).toContainText('2 Filme', { timeout: 20_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 20_000 })
})

test('direct play streams the original file with range requests', async ({ page }) => {
  await page.goto(`/search?q=${PLAYABLE}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click()

  const ranged = page.waitForResponse((r) => r.url().includes('/api/v1/stream/') && r.status() === 206)
  await page.getByRole('link', { name: 'Abspielen' }).click()
  await expect(page).toHaveURL(/\/play\/\d+$/)
  await expect(page.getByTestId('player-title')).toHaveText(PLAYABLE)
  expect((await ranged).headers()['content-range']).toMatch(/^bytes 0-\d+\/\d+$/)

  // Playback actually advances (muted, so autoplay policies don't interfere).
  const video = page.getByTestId('player-video')
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play()
  })
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 10_000 }).toBeGreaterThan(0.5)
  expect(await video.evaluate((v: HTMLVideoElement) => Math.round(v.duration))).toBe(3)

  // Seeking near the end works (another range request).
  await video.evaluate((v: HTMLVideoElement) => {
    v.currentTime = 2.5
  })
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime)).toBeGreaterThanOrEqual(2.5)

  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByRole('heading', { name: PLAYABLE })).toBeVisible()
})

test('formats the browser cannot play show a hint', async ({ page }) => {
  await page.goto(`/search?q=${UNPLAYABLE}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click()
  await page.getByRole('link', { name: 'Abspielen' }).click()
  await expect(page.getByRole('alert')).toContainText('nicht direkt abspielen', { timeout: 10_000 })
  // The decision engine explains why: MPEG-4 Part 2 and Matroska are not playable here.
  const reasons = page.getByTestId('playback-reasons')
  await expect(reasons).toContainText('Video-Codec wird nicht unterstützt')
  await expect(reasons).toContainText('Dateiformat (Container) wird nicht unterstützt')
  await expect(page.getByRole('alert')).toContainText('Umwandeln (Transcoding)')
  await expect(page.getByTestId('player-video')).toHaveCount(0)
})
