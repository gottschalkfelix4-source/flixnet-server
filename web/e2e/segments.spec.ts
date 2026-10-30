import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Locator, type Page, expect, test } from '@playwright/test'

// Unique title so parallel specs sharing the server don't interfere.
const SERIES = 'Vorspannserie'
const INTRO_S = 30
const TOTAL_S = 100
const COLD_OPENS = [4.3, 12.6, 9.1]

const chromaprint = (() => {
  try {
    return execFileSync('ffmpeg', ['-hide_banner', '-muxers'], { encoding: 'utf8' }).includes(' chromaprint ')
  } catch {
    return false
  }
})()

/** Random tones (three per second), different per seed. */
function ownAudio(seed: number, seconds: number) {
  const hash = `sin(floor(t*3)*12.9898+${seed}*78.233)*43758.5453`
  return `aevalsrc=0.25*sin(2*PI*t*(150+700*(${hash}-floor(${hash})))):s=48000:d=${seconds}`
}

/** An episode: own cold open, the season's intro tune, own rest. H.264/AAC plays directly. */
function encode(path: string, seed: number, coldOpen: number) {
  const intro = `aevalsrc=0.3*sin(2*PI*t*220*pow(2\\,mod(floor(t/0.75)*7\\,12)/12)):s=48000:d=${INTRO_S}`
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', `testsrc2=s=320x180:r=10:d=${TOTAL_S}`,
    '-f', 'lavfi', '-i', ownAudio(seed, coldOpen),
    '-f', 'lavfi', '-i', intro,
    '-f', 'lavfi', '-i', ownAudio(seed + 500, TOTAL_S - coldOpen - INTRO_S),
    '-filter_complex', '[1:a][2:a][3:a]concat=n=3:v=0:a=1[a]',
    '-map', '0:v', '-map', '[a]',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-g', '10', '-c:a', 'aac', '-movflags', '+faststart',
    path,
  ])
}

function seriesRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-segments-'))
  const season = join(root, SERIES, 'Season 01')
  mkdirSync(season, { recursive: true })
  COLD_OPENS.forEach((coldOpen, i) => encode(join(season, `${SERIES}.S01E0${i + 1}.mp4`), i * 7 + 1, coldOpen))
  return root
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

type Segment = { kind: string; start_ms: number; end_ms: number }
type Items = { items: { id: number; title: string }[] }

async function firstEpisode(page: Page): Promise<number> {
  const series = (await api(page, 'GET', `/api/v1/items?kind=series&search=${SERIES}`)) as Items
  const episodes = (await api(page, 'GET', `/api/v1/items?series_id=${series.items[0].id}&kind=episode`)) as Items
  return episodes.items[0].id
}

async function play(video: Locator) {
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
test.skip(!chromaprint, 'ffmpeg was built without Chromaprint')

let episode = 0
let intro: Segment

test('the intro of a scanned season is detected', async ({ page }) => {
  test.setTimeout(240_000)
  const root = seriesRoot()
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('Vorspann')
  await dialog.getByLabel('Inhalt').selectOption('shows')
  await expect(dialog.getByLabel('Intro-Erkennung')).toBeChecked()
  await dialog.getByLabel('Ordnerpfad').fill(root)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: 'Vorspann' })
  await expect(entry.getByTestId('library-counts')).toContainText('3 Folgen', { timeout: 60_000 })

  // Probing queues the season's analysis (a background job after a short delay).
  episode = await firstEpisode(page)
  await expect
    .poll(async () => ((await api(page, 'GET', `/api/v1/items/${episode}/segments`)) as Segment[]).length, {
      timeout: 150_000,
      intervals: [2_000],
    })
    .toBeGreaterThan(0)
  const segments = (await api(page, 'GET', `/api/v1/items/${episode}/segments`)) as Segment[]
  intro = segments.find((s) => s.kind === 'intro')!
  expect(Math.abs(intro.start_ms / 1000 - COLD_OPENS[0])).toBeLessThanOrEqual(1)
  expect(Math.abs(intro.end_ms / 1000 - COLD_OPENS[0] - INTRO_S)).toBeLessThanOrEqual(1)

  // Administrators see it on the episode page.
  await page.goto(`/item/${episode}`)
  await expect(page.getByTestId('segment-intro')).toContainText('erkannt')
})

test('"Intro überspringen" jumps past the intro', async ({ page }) => {
  await page.goto(`/play/${episode}?start=0`)
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  const video = page.getByTestId('player-video')
  await play(video)
  const skip = page.getByTestId('skip-button')
  await expect(skip).toBeHidden()

  await seek(video, intro.start_ms / 1000 + 2)
  await expect(skip).toHaveText('Intro überspringen')
  await skip.click()
  await expect.poll(() => currentTime(video)).toBeGreaterThanOrEqual(intro.end_ms / 1000 - 0.1)
  await expect(skip).toBeHidden()

  // Back inside the intro, the button returns.
  await seek(video, intro.start_ms / 1000 + 5)
  await expect(skip).toBeVisible()
})

test('with "Intro automatisch überspringen" the player skips by itself', async ({ page }) => {
  await page.goto('/settings?tab=account')
  const preference = page.getByLabel('Intro automatisch überspringen')
  await expect(preference).not.toBeChecked()
  await preference.check()
  await expect.poll(async () => ((await api(page, 'GET', '/api/v1/users/me/preferences')) as { skip_intro_automatically: boolean }).skip_intro_automatically).toBe(true)

  try {
    await page.goto(`/play/${episode}?start=0`)
    const video = page.getByTestId('player-video')
    await play(video)
    await seek(video, intro.start_ms / 1000 + 1)
    await expect.poll(() => currentTime(video)).toBeGreaterThanOrEqual(intro.end_ms / 1000 - 0.1)
    await expect(page.getByTestId('skip-button')).toBeHidden()
  } finally {
    await api(page, 'PATCH', '/api/v1/users/me/preferences', { skip_intro_automatically: false })
  }
})
