import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Locator, type Page, expect, test } from '@playwright/test'

// Playwright's Chromium plays VP9 and H.264/AAC but neither HEVC nor AC-3: the VP9
// file is direct-played, H.264 with AC-3 in Matroska is remuxed (video copied, audio
// converted) and HEVC 10-bit is transcoded.
const DIRECT = 'Abspieltest'
const HEVC = 'Zehnbitfilm'
const REMUX = 'Umpacktest'

function encode(path: string, seconds: number, codec: string[]) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', `testsrc2=s=640x360:r=25:d=${seconds}`,
    '-f', 'lavfi', '-i', `sine=frequency=440:duration=${seconds}`,
    ...codec, path,
  ])
}

function libraryRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-play-'))
  mkdirSync(join(root, `${DIRECT} (2021)`))
  encode(join(root, `${DIRECT} (2021)`, `${DIRECT}.2021.webm`), 3, ['-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-b:v', '200k', '-c:a', 'libopus'])
  mkdirSync(join(root, `${HEVC} (2021)`))
  encode(join(root, `${HEVC} (2021)`, `${HEVC}.2021.German.2160p.HDR.x265-FLX.mkv`), 60, [
    '-c:v', 'libx265', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p10le', '-x265-params', 'log-level=error',
    '-c:a', 'aac',
  ])
  mkdirSync(join(root, `${REMUX} (2021)`))
  encode(join(root, `${REMUX} (2021)`, `${REMUX}.2021.German.AC3.1080p.x264-FLX.mkv`), 30, [
    '-c:v', 'libx264', '-preset', 'ultrafast', '-g', '50', '-sc_threshold', '0', '-c:a', 'ac3',
  ])
  return root
}

/** Admin API call with the stored session token. */
async function api(page: Page, path: string): Promise<unknown> {
  return page.evaluate(async (url) => {
    const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
    const response = await fetch(url, { headers: { Authorization: `Bearer ${token}` } })
    return response.json()
  }, path)
}

/** Opens the player for a movie and returns its item id. */
async function openPlayer(page: Page, title: string): Promise<number> {
  await page.goto(`/search?q=${title}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click({ position: { x: 8, y: 8 } })
  await page.getByRole('link', { name: 'Abspielen' }).click()
  await expect(page).toHaveURL(/\/play\/\d+$/)
  await expect(page.getByTestId('player-title')).toHaveText(title)
  return Number(new URL(page.url()).pathname.split('/').pop())
}

/** Transcode sessions of one item (other specs may play other items meanwhile). */
async function sessionsOf(page: Page, itemId: number) {
  return ((await api(page, '/api/v1/transcode/sessions')) as { item_id: number }[]).filter((s) => s.item_id === itemId)
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
  await expect(entry.getByTestId('library-counts')).toContainText('3 Filme', { timeout: 30_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 30_000 })
})

test('direct play streams the original file with range requests', async ({ page }) => {
  const ranged = page.waitForResponse((r) => r.url().includes('/api/v1/stream/') && r.status() === 206)
  await openPlayer(page, DIRECT)
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  expect((await ranged).headers()['content-range']).toMatch(/^bytes 0-\d+\/\d+$/)

  // Playback advances (muted, so autoplay policies don't interfere).
  const video = page.getByTestId('player-video')
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play()
  })
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 10_000 }).toBeGreaterThan(0.5)
  expect(await video.evaluate((v: HTMLVideoElement) => Math.round(v.duration))).toBe(3)

  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByRole('heading', { name: DIRECT })).toBeVisible()
})

// Milestone 4.3 gate: HEVC 10-bit plays in a browser without HEVC, a seek to 80 %
// plays within 3 s, and closing the player leaves no ffmpeg running.
test('HEVC 10-bit is transcoded to HLS, seeks fast and stops on close', async ({ page }) => {
  const itemId = await openPlayer(page, HEVC)
  await expect(page.getByTestId('play-method')).toHaveText('Transcode')

  const video = page.getByTestId('player-video')
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play()
  })
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 15_000 }).toBeGreaterThan(1)
  expect(await video.evaluate((v: HTMLVideoElement) => Math.round(v.duration))).toBe(60)
  await expect.poll(async () => (await sessionsOf(page, itemId)).length).toBe(1)

  // Seek to 80 %: playing again at the new position within 3 seconds.
  const seekMs = await video.evaluate(async (v: HTMLVideoElement) => {
    const started = performance.now()
    v.currentTime = 48
    await new Promise<void>((resolve) => {
      const check = () => {
        if (!v.seeking && v.readyState >= 3 && v.currentTime >= 48) resolve()
        else setTimeout(check, 50)
      }
      check()
    })
    return performance.now() - started
  })
  expect(seekMs).toBeLessThan(3000)
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime)).toBeGreaterThan(48.5)

  // Closing the player ends the session and its ffmpeg.
  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByRole('heading', { name: HEVC })).toBeVisible()
  await expect.poll(() => sessionsOf(page, itemId), { timeout: 5_000 }).toEqual([])
})

test('H.264 with AC-3 is remuxed: video copied on keyframes, audio converted', async ({ page }) => {
  const itemId = await openPlayer(page, REMUX)
  await expect(page.getByTestId('play-method')).toHaveText('Remux')

  const video = page.getByTestId('player-video')
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play()
  })
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 15_000 }).toBeGreaterThan(1)
  expect(await video.evaluate((v: HTMLVideoElement) => Math.round(v.duration))).toBe(30)

  // Seek into an unencoded part: ffmpeg restarts at that GOP.
  const seekMs = await video.evaluate(async (v: HTMLVideoElement) => {
    const started = performance.now()
    v.currentTime = 24
    await new Promise<void>((resolve) => {
      const check = () => {
        if (!v.seeking && v.readyState >= 3 && v.currentTime >= 24) resolve()
        else setTimeout(check, 50)
      }
      check()
    })
    return performance.now() - started
  })
  expect(seekMs).toBeLessThan(3000)
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime)).toBeGreaterThan(24.5)

  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByRole('heading', { name: REMUX })).toBeVisible()
  await expect.poll(() => sessionsOf(page, itemId), { timeout: 5_000 }).toEqual([])
})

// Milestone 4.6: an MP4 (H.264/AAC, so Direct Play) with German and English audio and
// two sidecar subtitles, a forced German and an English one.
const TRACKS = 'Spurwechsel'

function tracksRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-tracks-'))
  const folder = join(root, `${TRACKS} (2021)`)
  mkdirSync(folder)
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc2=s=640x360:r=25:d=20',
    '-f', 'lavfi', '-i', 'sine=frequency=440:duration=20',
    '-f', 'lavfi', '-i', 'sine=frequency=880:duration=20',
    '-map', '0', '-map', '1', '-map', '2',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-g', '50', '-sc_threshold', '0', '-c:a', 'aac',
    '-metadata:s:a:0', 'language=ger', '-metadata:s:a:1', 'language=eng',
    '-disposition:a:0', 'default', '-disposition:a:1', '0',
    join(folder, `${TRACKS}.2021.mp4`),
  ])
  writeFileSync(join(folder, `${TRACKS}.2021.de.forced.srt`), '1\n00:00:00,000 --> 00:00:20,000\nErzwungener Text\n')
  writeFileSync(join(folder, `${TRACKS}.2021.en.srt`), '1\n00:00:00,000 --> 00:00:20,000\nEnglish subtitle line\n')
  return root
}

test('subtitles and audio tracks can be switched in the player', async ({ page }) => {
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(TRACKS)
  await dialog.getByLabel('Ordnerpfad').fill(tracksRoot())
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: TRACKS })
  await expect(entry.getByTestId('library-counts')).toContainText('1 Film', { timeout: 30_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 30_000 })

  await openPlayer(page, TRACKS)
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  const itemId = Number(/\/play\/(\d+)/.exec(page.url())?.[1])
  const video = page.getByTestId('player-video')
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play()
  })

  // The subtitle track the player shows, with the cues active right now.
  const showing = () =>
    video.evaluate((v: HTMLVideoElement) => {
      const track = Array.from(v.textTracks).find((t) => t.mode === 'showing')
      const cues = Array.from(track?.activeCues ?? []).map((c) => (c as VTTCue).text)
      return { label: track?.label ?? null, cues }
    })
  const menu = async () => {
    await page.getByRole('button', { name: 'Wiedergabe-Einstellungen' }).click()
    return page.getByRole('menu', { name: 'Wiedergabe-Einstellungen' })
  }
  const audioItem = (m: Locator, name: RegExp) => m.getByRole('group', { name: 'Tonspur' }).getByRole('menuitemradio', { name })
  const subtitleItem = (m: Locator, name: RegExp) =>
    m.getByRole('group', { name: 'Untertitel' }).getByRole('menuitemradio', { name })

  // Forced German subtitles are on without being asked.
  await expect.poll(showing, { timeout: 10_000 }).toEqual({ label: 'Deutsch (erzwungen)', cues: ['Erzwungener Text'] })
  let m = await menu()
  await expect(audioItem(m, /Deutsch/)).toHaveAttribute('aria-checked', 'true')
  await expect(subtitleItem(m, /Deutsch/)).toHaveAttribute('aria-checked', 'true')

  // Other text subtitles switch in the player, without a new source.
  await subtitleItem(m, /Englisch/).click()
  await expect(m).toBeHidden()
  await expect.poll(showing).toEqual({ label: 'Englisch', cues: ['English subtitle line'] })
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  m = await menu()
  await subtitleItem(m, /^Aus$/).click()
  await expect.poll(showing).toEqual({ label: null, cues: [] })

  // Browsers cannot switch audio tracks of a file: the English track comes as a remux
  // and playback continues where it was.
  await video.evaluate((v: HTMLVideoElement) => {
    v.currentTime = 8
  })
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime)).toBeGreaterThan(8)
  m = await menu()
  await audioItem(m, /Englisch/).click()
  await expect(page.getByTestId('play-method')).toHaveText('Remux')
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 15_000 }).toBeGreaterThan(8.5)
  const sessions = async () =>
    ((await api(page, '/api/v1/transcode/sessions')) as { item_id: number }[]).filter((s) => s.item_id === itemId)
  expect(await sessions()).toHaveLength(1)
  m = await menu()
  await expect(audioItem(m, /Englisch/)).toHaveAttribute('aria-checked', 'true')

  // Text subtitles work over HLS as well.
  await subtitleItem(m, /Englisch/).click()
  await expect.poll(showing).toEqual({ label: 'Englisch', cues: ['English subtitle line'] })

  // Back to the German track: Direct Play again, the remux session is stopped and the
  // subtitles stay on.
  m = await menu()
  await audioItem(m, /Deutsch/).click()
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  await expect.poll(sessions, { timeout: 5_000 }).toEqual([])
  await expect.poll(showing, { timeout: 10_000 }).toEqual({ label: 'Englisch', cues: ['English subtitle line'] })
  expect(await video.evaluate((v: HTMLVideoElement) => v.currentTime)).toBeGreaterThan(8.5)

  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByRole('heading', { name: TRACKS })).toBeVisible()
})
