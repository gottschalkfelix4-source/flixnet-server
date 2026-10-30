import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Locator, type Page, expect, test } from '@playwright/test'

// Unique titles so parallel specs sharing the server don't interfere.
const MOVIE = 'Fortsetzungsfilm'
const TRANSCODED = 'Umwandlungsfilm'
const SERIES = 'Folgenserie'

function encode(path: string, seconds: number, codec: string[], fps = 5) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', `testsrc2=s=320x180:r=${fps}:d=${seconds}`,
    '-f', 'lavfi', '-i', `sine=frequency=330:duration=${seconds}`,
    ...codec, path,
  ])
}

// H.264/AAC in MP4 plays directly in Chromium; MPEG-4 Part 2 needs a transcode (the
// AAC audio is copied: browsers currently reject our MP3-to-AAC transcode output).
const DIRECT = ['-c:v', 'libx264', '-preset', 'ultrafast', '-g', '10', '-c:a', 'aac', '-movflags', '+faststart']
const TRANSCODE = ['-c:v', 'mpeg4', '-g', '50', '-c:a', 'aac']

function movieRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-resume-movies-'))
  // Ten minutes, so 5:00 is past the "no resume point before 2 minutes / 5 %" rule.
  mkdirSync(join(root, `${MOVIE} (2023)`))
  encode(join(root, `${MOVIE} (2023)`, `${MOVIE}.2023.mp4`), 600, DIRECT)
  mkdirSync(join(root, `${TRANSCODED} (2023)`))
  encode(join(root, `${TRANSCODED} (2023)`, `${TRANSCODED}.2023.mkv`), 240, TRANSCODE, 25)
  return root
}

function seriesRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-resume-shows-'))
  const season = join(root, SERIES, 'Season 01')
  mkdirSync(season, { recursive: true })
  for (const n of [1, 2]) encode(join(season, `${SERIES}.S01E0${n}.mp4`), 30, DIRECT)
  return root
}

async function addLibrary(page: Page, name: string, kind: 'movies' | 'shows', root: string, counts: string) {
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(name)
  await dialog.getByLabel('Inhalt').selectOption(kind)
  await dialog.getByLabel('Ordnerpfad').fill(root)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: name })
  await expect(entry.getByTestId('library-counts')).toContainText(counts, { timeout: 60_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 60_000 })
}

/** API call with the stored session token. */
async function api(page: Page, method: string, path: string, body?: unknown): Promise<unknown> {
  return page.evaluate(
    async ({ method, path, body }) => {
      const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
      const response = await fetch(path, {
        method,
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      })
      return response.status === 204 ? null : response.json()
    },
    { method, path, body },
  )
}

async function openItem(page: Page, title: string, region: 'Filme' | 'Serien') {
  await page.goto(`/search?q=${title}`)
  await page.getByRole('region', { name: region }).getByTestId('poster-card').click()
  await expect(page.getByRole('heading', { name: title })).toBeVisible()
}

async function play(video: Locator) {
  // hls.js loads lazily; wait until the element has media attached.
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.readyState), { timeout: 20_000 }).toBeGreaterThan(0)
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play()
  })
}

const currentTime = (video: Locator) => video.evaluate((v: HTMLVideoElement) => v.currentTime)

async function seek(video: Locator, seconds: number) {
  await video.evaluate(async (v: HTMLVideoElement, to: number) => {
    v.currentTime = to
    await new Promise<void>((resolve) => v.addEventListener('seeked', () => resolve(), { once: true }))
  }, seconds)
}

test.describe.configure({ mode: 'serial' })

test('libraries for resuming get scanned', async ({ page }) => {
  test.setTimeout(180_000)
  await addLibrary(page, 'Fortsetzen Filme', 'movies', movieRoot(), '2 Filme')
  await addLibrary(page, 'Fortsetzen Serien', 'shows', seriesRoot(), '2 Folgen')
})

test('progress is saved, shows in "Weiterschauen" and playback resumes there', async ({ page }) => {
  await openItem(page, MOVIE, 'Filme')
  await page.getByRole('link', { name: 'Abspielen' }).click()
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  const video = page.getByTestId('player-video')
  await play(video)
  await expect.poll(() => currentTime(video), { timeout: 10_000 }).toBeGreaterThan(0.3)

  // Watching at 5:00 of 10:00; the seek is reported right away.
  const reported = page.waitForRequest((r) => r.url().includes('/progress') && r.method() === 'POST')
  await seek(video, 300)
  await reported

  // The activity bubble in the top bar counts the stream and lists it.
  const dashboard = await page.context().newPage()
  await dashboard.goto('/')
  const bubble = dashboard.getByTestId('activity-button')
  await expect(bubble.getByTestId('activity-streams')).toHaveText(/^[1-9]\d*$/)
  await bubble.click()
  const panel = dashboard.getByTestId('activity-panel')
  await expect(panel.getByTestId('activity-stream').filter({ hasText: MOVIE })).toContainText('Felix')
  await panel.getByRole('link', { name: 'Dashboard öffnen' }).click()
  await expect(dashboard).toHaveURL(/tab=dashboard/)

  // The sessions page shows it live.
  await dashboard.goto('/settings?tab=sessions')
  const session = dashboard.getByTestId('active-session').filter({ hasText: MOVIE })
  await expect(session).toBeVisible()
  await expect(session.getByTestId('session-method')).toHaveText('Direct Play')
  await expect(session.getByTestId('session-user')).toContainText('Felix')
  await expect(session.getByTestId('session-position')).toContainText('/ 10:00')

  // Leaving the player stops the session and keeps the resume point.
  const stopped = page.waitForResponse((r) => r.url().includes('/stop') && r.status() === 200)
  await page.getByRole('button', { name: 'Zurück' }).click()
  expect(await (await stopped).json()).toMatchObject({ played: false })
  await expect(session).toHaveCount(0, { timeout: 10_000 })
  await dashboard.close()

  await page.goto('/')
  const row = page.getByRole('region', { name: 'Weiterschauen' })
  const card = row.getByTestId('poster-card').filter({ hasText: MOVIE })
  await expect(card).toBeVisible()
  await expect(card.locator('.bg-accent')).toBeVisible()

  // The detail page offers to continue; the player starts at the saved position.
  await card.click()
  await expect(page.getByRole('link', { name: 'Fortsetzen' })).toBeVisible()
  await expect(page.getByTestId('remaining-time')).toContainText('Noch 5 Min.')
  await page.getByRole('link', { name: 'Fortsetzen' }).click()
  await expect(page.getByTestId('resume-toast')).toContainText('Fortgesetzt bei 5:0')
  await play(video)
  await expect.poll(() => currentTime(video), { timeout: 10_000 }).toBeGreaterThan(299)

  // "Von vorne" in the toast starts over.
  await page.getByTestId('resume-toast').getByRole('button', { name: 'Von vorne' }).click()
  await expect.poll(() => currentTime(video)).toBeLessThan(5)
})

test('a transcode resumes without encoding from the start first', async ({ page }) => {
  await openItem(page, TRANSCODED, 'Filme')
  const itemId = Number(page.url().match(/\/item\/(\d+)/)?.[1])
  // A resume point at 2:30 of 4:00, reported like a player would.
  const report = { item_id: itemId, position_ms: 0, duration_ms: 240_000, play_method: 'transcode' }
  const { id } = (await api(page, 'POST', '/api/v1/playback/sessions', report)) as { id: string }
  await api(page, 'POST', `/api/v1/playback/sessions/${id}/stop`, { ...report, position_ms: 150_000 })
  await page.reload()

  const segments: string[] = []
  page.on('request', (r) => {
    const m = r.url().match(/\/transcode\/[0-9a-f]{32}\/(\d+)\.m4s/)
    if (m) segments.push(m[1]!)
  })
  await page.getByRole('link', { name: 'Fortsetzen' }).click()
  await expect(page.getByTestId('play-method')).toHaveText('Transcode')
  const video = page.getByTestId('player-video')
  await play(video)
  await expect.poll(() => currentTime(video), { timeout: 20_000 }).toBeGreaterThan(150.5)
  expect(segments.length).toBeGreaterThan(0)
  expect(segments).not.toContain('0')
  expect(Number(segments[0])).toBeGreaterThanOrEqual(24)

  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByRole('heading', { name: TRANSCODED })).toBeVisible()
})

test('after episode 1, "Nächste Folge" offers episode 2', async ({ page }) => {
  await openItem(page, SERIES, 'Serien')
  await expect(page.getByTestId('play-button')).toContainText('S1 · F1')
  await page.getByTestId('play-button').click()
  const video = page.getByTestId('player-video')
  await play(video)
  await expect.poll(() => currentTime(video), { timeout: 10_000 }).toBeGreaterThan(0.3)
  // Watch the end: past 90 % counts as watched. The player offers to go on with
  // episode 2 (autoplay); cancelled here, so this player sees the end.
  await seek(video, 25)
  await page.getByTestId('up-next').getByRole('button', { name: 'Abbrechen' }).click()
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.ended), { timeout: 10_000 }).toBe(true)
  const stopped = page.waitForResponse((r) => r.url().includes('/stop') && r.status() === 200)
  await page.getByRole('button', { name: 'Zurück' }).click()
  expect(await (await stopped).json()).toMatchObject({ played: true, resume_position_ms: 0 })

  await page.goto('/')
  const next = page.getByRole('region', { name: 'Nächste Folge' }).getByTestId('poster-card').filter({ hasText: SERIES })
  await expect(next).toContainText('S1 · F2')

  // The series' play button continues with episode 2 as well.
  await openItem(page, SERIES, 'Serien')
  await expect(page.getByTestId('play-button')).toContainText('Abspielen')
  await expect(page.getByTestId('play-button')).toContainText('S1 · F2')
})
