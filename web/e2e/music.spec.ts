import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { expect, test } from '@playwright/test'

// Music: a generated library (two FLAC tracks and a WMA that Chromium cannot play),
// the album page, gapless playback into the next track, synced lyrics, the AAC
// conversion and the queue surviving a reload.
const ALBUM = 'Erstes Album'
const ARTIST = 'E2E Band'

function encode(path: string, frequency: number, tags: [string, string][], codec: string[]) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', `sine=frequency=${frequency}:duration=3`,
    ...tags.flatMap(([k, v]) => ['-metadata', `${k}=${v}`]),
    ...codec, path,
  ])
}

function musicLibrary(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-music-'))
  const album = join(root, ARTIST, ALBUM)
  mkdirSync(album, { recursive: true })
  const tags = (title: string, track: number): [string, string][] => [
    ['title', title],
    ['artist', ARTIST],
    ['album_artist', ARTIST],
    ['album', ALBUM],
    ['track', String(track)],
    ['date', '2024'],
    ['genre', 'Testmusik'],
  ]
  encode(join(album, '01 - Anfang.flac'), 440, tags('Anfang', 1), ['-c:a', 'flac'])
  encode(join(album, '02 - Weiter.flac'), 550, tags('Weiter', 2), ['-c:a', 'flac'])
  // Untagged WMA: title and number from the file name, played as AAC conversion.
  encode(join(album, '03 - Umgewandelt.wma'), 660, [], ['-c:a', 'wmav2', '-b:a', '64k'])
  writeFileSync(
    join(album, '02 - Weiter.lrc'),
    '[ar:E2E Band]\n[00:00.10]Erste Zeile im Takt\n[00:01.20]Zweite Zeile im Takt\n[00:02.30]Dritte Zeile\n',
  )
  return root
}

test.describe.configure({ mode: 'serial' })

test('a music library gets scanned from tags', async ({ page }) => {
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('E2E Musik')
  await dialog.getByLabel('Inhalt').selectOption('music')
  await dialog.getByLabel('Ordnerpfad').fill(musicLibrary())
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: 'E2E Musik' })
  await expect(entry.getByTestId('library-counts')).toContainText('1 Album · 3 Titel', { timeout: 30_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 30_000 })
})

test('album plays gaplessly into the next track, with synced lyrics and conversion', async ({ page }) => {
  await page.goto('/music')
  await page.getByTestId('album-card').filter({ hasText: ALBUM }).click()
  await expect(page.getByRole('heading', { name: ALBUM })).toBeVisible()
  await expect(page.getByRole('link', { name: ARTIST })).toBeVisible()
  const rows = page.getByTestId('track-row')
  await expect(rows).toHaveCount(3)
  await expect(rows.nth(2)).toContainText('Umgewandelt')

  await page.getByRole('button', { name: 'Abspielen', exact: true }).click()
  const player = page.getByTestId('mini-player')
  await expect(player.getByTestId('player-track-title')).toHaveText('Anfang')

  // Record when each audio element ends and starts playing.
  await page.evaluate(() => {
    const log: { event: string; track: string | undefined; at: number }[] = []
    ;(window as unknown as { musicLog: typeof log }).musicLog = log
    for (const audio of Array.from(document.querySelectorAll<HTMLAudioElement>('audio[data-flixnet-audio]'))) {
      for (const event of ['ended', 'playing']) {
        audio.addEventListener(event, () => log.push({ event, track: audio.dataset.trackId, at: performance.now() }))
      }
    }
  })
  await expect
    .poll(() => page.evaluate(() => Array.from(document.querySelectorAll<HTMLAudioElement>('audio')).some((a) => !a.paused && a.currentTime > 0.3)))
    .toBe(true)

  // The second track starts by itself once the first one ends — without a gap.
  await expect(player.getByTestId('player-track-title')).toHaveText('Weiter', { timeout: 15_000 })
  const gap = await page.evaluate(async () => {
    const log = (window as unknown as { musicLog: { event: string; track: string | undefined; at: number }[] }).musicLog
    for (let i = 0; i < 40 && !log.some((e) => e.event === 'playing'); i++) await new Promise((r) => setTimeout(r, 50))
    const ended = log.find((e) => e.event === 'ended')
    const started = log.find((e) => e.event === 'playing' && e.track !== ended?.track)
    return ended && started ? started.at - ended.at : null
  })
  expect(gap).not.toBeNull()
  expect(gap!).toBeLessThan(400)

  // Synced lyrics from the .lrc follow the playback position.
  await player.getByTestId('player-lyrics').click()
  const lyrics = page.getByTestId('lyrics-panel')
  await expect(lyrics).toContainText('Zweite Zeile im Takt')
  await expect(lyrics.locator('[data-active]')).toHaveText(/Zeile/, { timeout: 5_000 })
  await expect(lyrics.locator('[data-active]')).toHaveText('Dritte Zeile', { timeout: 5_000 })

  // The WMA plays as AAC conversion.
  await expect(player.getByTestId('player-track-title')).toHaveText('Umgewandelt', { timeout: 15_000 })
  await expect
    .poll(
      () =>
        page.evaluate(() =>
          Array.from(document.querySelectorAll<HTMLAudioElement>('audio')).some(
            (a) => a.src.includes('/api/v1/music/stream/') && !a.paused && a.currentTime > 0.5,
          ),
        ),
      { timeout: 15_000 },
    )
    .toBe(true)
  await player.getByTestId('player-play').click()
  await expect(player.getByRole('button', { name: 'Abspielen' })).toBeVisible()

  // The queue belongs to this device and survives a reload.
  await page.reload()
  await expect(page.getByTestId('mini-player').getByTestId('player-track-title')).toHaveText('Umgewandelt')
  await page.getByTestId('player-queue').click()
  await expect(page.getByTestId('queue-entry')).toHaveCount(3)
})

test('search finds artists, albums and tracks', async ({ page }) => {
  await page.goto('/search?q=Weiter')
  await expect(page.getByRole('region', { name: 'Titel' }).getByTestId('track-row')).toContainText('Weiter')
  await page.goto(`/search?q=${encodeURIComponent(ARTIST)}`)
  await page.getByRole('region', { name: 'Künstler' }).getByTestId('artist-card').click()
  await expect(page.getByRole('heading', { name: ARTIST })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Alben' }).getByTestId('album-card')).toContainText(ALBUM)
})
