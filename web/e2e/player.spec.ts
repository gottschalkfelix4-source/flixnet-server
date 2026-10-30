import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Locator, type Page, expect, test } from '@playwright/test'

// Milestone 4.8: own controls, trickplay previews, chapters, autoplay of the next
// episode, and the MP3-in-HLS regression. Unique titles: specs share one server.
const SERIES = 'Steuerserie'
const MP3 = 'Mpdreifilm'

const CHAPTERS =
  ';FFMETADATA1\n' +
  '[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=15000\ntitle=Anfang\n' +
  '[CHAPTER]\nTIMEBASE=1/1000\nSTART=15000\nEND=30000\ntitle=Mitte\n' +
  '[CHAPTER]\nTIMEBASE=1/1000\nSTART=30000\nEND=50000\ntitle=Ende\n'

function encode(path: string, seconds: number, codec: string[], extraInput: string[] = []) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', `testsrc2=s=320x180:r=10:d=${seconds}`,
    '-f', 'lavfi', '-i', `sine=frequency=330:duration=${seconds}`,
    ...extraInput,
    ...codec, path,
  ])
}

// H.264/AAC in MP4 (Direct Play), a keyframe every 2 s; episode 1 has chapters.
const DIRECT = ['-c:v', 'libx264', '-preset', 'ultrafast', '-g', '20', '-c:a', 'aac', '-movflags', '+faststart']

function seriesRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-player-shows-'))
  const season = join(root, SERIES, 'Season 01')
  mkdirSync(season, { recursive: true })
  const metadata = join(root, 'chapters.txt')
  writeFileSync(metadata, CHAPTERS)
  encode(join(season, `${SERIES}.S01E01.mp4`), 90, ['-map', '0', '-map', '1', '-map_chapters', '2', ...DIRECT], ['-i', metadata])
  encode(join(season, `${SERIES}.S01E02.mp4`), 90, DIRECT)
  return root
}

// MPEG-4 Part 2 with MP3 in Matroska: a transcode whose audio used to break Chromium.
function mp3Root(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-player-movies-'))
  mkdirSync(join(root, `${MP3} (2022)`))
  encode(join(root, `${MP3} (2022)`, `${MP3}.2022.mkv`), 20, ['-c:v', 'mpeg4', '-g', '50', '-c:a', 'libmp3lame'])
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

/** API call with the stored session token: status and JSON body. */
async function api(page: Page, path: string): Promise<{ status: number; body: unknown }> {
  return page.evaluate(async (url) => {
    const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
    const response = await fetch(url, { headers: { Authorization: `Bearer ${token}` } })
    return { status: response.status, body: response.status === 200 ? await response.json() : null }
  }, path)
}

const currentTime = (video: Locator) => video.evaluate((v: HTMLVideoElement) => v.currentTime)
const paused = (video: Locator) => video.evaluate((v: HTMLVideoElement) => v.paused)

/** Moves the mouse over the player, which shows the auto-hiding controls. */
async function wake(page: Page) {
  await page.mouse.move(200, 200)
  await page.mouse.move(210, 205)
  await expect(page.getByTestId('player')).toHaveAttribute('data-controls', 'visible')
}

/** Item id of episode 1, once opened. */
let episodeOne = 0

/** Opens episode 1 from the series page; returns its item id. */
async function openEpisodeOne(page: Page): Promise<number> {
  await page.goto(`/search?q=${SERIES}`)
  await page.getByRole('region', { name: 'Serien' }).getByTestId('poster-card').click({ position: { x: 8, y: 8 } })
  await expect(page.getByRole('heading', { name: SERIES })).toBeVisible()
  await expect(page.getByTestId('play-button')).toContainText('S1 · F1')
  await page.getByTestId('play-button').click()
  await expect(page).toHaveURL(/\/play\/\d+/)
  await expect(page.getByTestId('player-title')).toContainText('S1 · F1')
  episodeOne = Number(/\/play\/(\d+)/.exec(page.url())?.[1])
  return episodeOne
}

/** Muted (no autoplay restrictions), then playing. */
async function startMuted(page: Page, video: Locator) {
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.readyState), { timeout: 20_000 }).toBeGreaterThan(0)
  if (!(await video.evaluate((v: HTMLVideoElement) => v.muted))) await page.keyboard.press('m')
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.muted)).toBe(true)
  // Autoplay may still be starting; press play only while it is really paused.
  await expect(async () => {
    if (await paused(video)) {
      await wake(page)
      await page.getByRole('button', { name: 'Abspielen', exact: true }).click({ timeout: 1_000 })
    }
    expect(await paused(video)).toBe(false)
  }).toPass({ timeout: 15_000 })
  const from = await currentTime(video)
  await expect.poll(() => currentTime(video), { timeout: 10_000 }).toBeGreaterThan(from + 0.3)
}

/** The media file of an item (for its trickplay and chapter endpoints). */
async function fileOf(page: Page, itemId: number): Promise<number> {
  const item = (await api(page, `/api/v1/items/${itemId}`)).body as { files: { id: number; role: string }[] }
  return item.files.find((f) => f.role === 'main')!.id
}

test.describe.configure({ mode: 'serial' })

test('libraries for the player get scanned', async ({ page }) => {
  test.setTimeout(180_000)
  await addLibrary(page, 'Spieler Serien', 'shows', seriesRoot(), '2 Folgen')
  await addLibrary(page, 'Spieler Filme', 'movies', mp3Root(), '1 Film')
})

test('playback starts by itself; the logo shows until the picture moves', async ({ page }) => {
  await openEpisodeOne(page)
  const video = page.getByTestId('player-video')
  // No click on play: the player starts it (muted only if the browser insists).
  await expect.poll(() => paused(video), { timeout: 20_000 }).toBe(false)
  const from = await currentTime(video)
  await expect.poll(() => currentTime(video), { timeout: 10_000 }).toBeGreaterThan(from + 0.3)
  await expect(page.getByTestId('player-loading')).toBeHidden()
})

test('own controls: play/pause, seek bar and keyboard shortcuts', async ({ page }) => {
  await openEpisodeOne(page)
  await expect(page.getByTestId('play-method')).toHaveText('Direct Play')
  const video = page.getByTestId('player-video')
  expect(await video.getAttribute('controls')).toBeNull()
  await startMuted(page, video)

  // Pause and play with the buttons.
  await wake(page)
  await page.getByRole('button', { name: 'Pause', exact: true }).click()
  await expect.poll(() => paused(video)).toBe(true)
  await expect(page.getByTestId('player')).toHaveAttribute('data-controls', 'visible')

  // A click on the seek bar jumps there (paused, so the position stays put).
  const bar = page.getByRole('slider', { name: 'Wiedergabeposition' })
  const box = (await bar.boundingBox())!
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2)
  await expect.poll(() => currentTime(video)).toBeGreaterThan(43.5)
  expect(await currentTime(video)).toBeLessThan(46.5)
  await expect(bar).toHaveAttribute('aria-valuenow', /^4[4-6]$/)
  // Chapter starts are marked (the one at 0:00 is the start of the bar).
  await expect(page.getByTestId('chapter-marker')).toHaveCount(2)

  // Keyboard: L / J ±10 s, arrows ±5 s, K play/pause, M mute.
  const before = await currentTime(video)
  await page.keyboard.press('l')
  await expect.poll(() => currentTime(video)).toBeGreaterThan(before + 9)
  await page.keyboard.press('j')
  await expect.poll(() => currentTime(video)).toBeLessThan(before + 1)
  await page.keyboard.press('ArrowLeft')
  await expect.poll(() => currentTime(video)).toBeLessThan(before - 4)
  await page.keyboard.press('ArrowRight')
  await expect.poll(() => currentTime(video)).toBeGreaterThan(before - 1)
  await page.keyboard.press('k')
  await expect.poll(() => paused(video)).toBe(false)
  await page.keyboard.press('k')
  await expect.poll(() => paused(video)).toBe(true)
  await page.keyboard.press('m')
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.muted)).toBe(false)
  await page.keyboard.press('m')

  // While playing, the controls hide after a few seconds without input.
  await page.keyboard.press('k')
  await expect(page.getByTestId('player')).toHaveAttribute('data-controls', 'hidden', { timeout: 8_000 })
  await wake(page)

  // Speed from the settings sheet.
  await page.getByRole('button', { name: 'Wiedergabe-Einstellungen' }).click()
  const menu = page.getByRole('menu', { name: 'Wiedergabe-Einstellungen' })
  await menu.getByRole('group', { name: 'Geschwindigkeit' }).getByRole('menuitemradio', { name: '1,5×' }).click()
  await expect(menu).toBeHidden()
  expect(await video.evaluate((v: HTMLVideoElement) => v.playbackRate)).toBe(1.5)

  // A quality limit asks the server for a new plan (this small file stays Direct Play).
  const limited = page.waitForRequest((r) => r.url().includes('/playback-info') && /"max_bitrate":2000000/.test(r.postData() ?? ''))
  await wake(page)
  await page.getByRole('button', { name: 'Wiedergabe-Einstellungen' }).click()
  await menu.getByRole('group', { name: 'Qualität' }).getByRole('menuitemradio', { name: /2 Mbit\/s/ }).click()
  await limited
  await page.getByRole('button', { name: 'Wiedergabe-Einstellungen' }).click()
  await expect(menu.getByRole('group', { name: 'Qualität' }).getByRole('menuitemradio', { name: /2 Mbit\/s/ })).toHaveAttribute(
    'aria-checked',
    'true',
  )
  await page.keyboard.press('Escape')
  await expect(menu).toBeHidden()
  expect(await video.evaluate((v: HTMLVideoElement) => v.playbackRate)).toBe(1.5)

  await page.getByRole('button', { name: 'Zurück', exact: true }).click()
  await expect(page.getByRole('heading', { name: SERIES })).toBeVisible()
})

test('trickplay preview on hover and chapter jump from the settings sheet', async ({ page }) => {
  const itemId = await openEpisodeOne(page)
  const file = await fileOf(page, itemId)
  // Thumbnails are made in the background after the scan.
  await expect
    .poll(async () => (await api(page, `/api/v1/files/${file}/trickplay`)).status, { timeout: 90_000 })
    .toBe(200)
  await expect
    .poll(
      async () => {
        const chapters = (await api(page, `/api/v1/files/${file}/chapters`)).body as { image_url: string | null }[]
        return chapters.filter((c) => c.image_url).length
      },
      { timeout: 60_000 },
    )
    .toBe(3)
  await page.reload()
  const video = page.getByTestId('player-video')
  await startMuted(page, video)
  await page.keyboard.press('k')
  await expect.poll(() => paused(video)).toBe(true)

  // Hovering at 20 % (0:18) shows that thumbnail, the time and the chapter.
  const bar = page.getByRole('slider', { name: 'Wiedergabeposition' })
  const box = (await bar.boundingBox())!
  await page.mouse.move(box.x + box.width * 0.2, box.y + box.height / 2)
  const preview = page.getByTestId('trickplay-preview')
  await expect(preview).toBeVisible()
  expect(await preview.evaluate((el) => getComputedStyle(el).backgroundImage)).toContain(`/files/${file}/trickplay/0.jpg`)
  // Tile 1 of row 0 (0:18 / 10 s), scaled to 240 px wide: 240 px to the left.
  expect(await preview.evaluate((el) => getComputedStyle(el).backgroundPosition)).toBe('-240px 0px')
  await expect(page.getByTestId('preview-time')).toHaveText('0:18')
  await expect(page.getByTestId('preview-chapter')).toHaveText('Mitte')
  // The sheet loads (signed URL, no token needed).
  const sheetUrl = (await preview.evaluate((el) => getComputedStyle(el).backgroundImage)).replace(/^url\("(.*)"\)$/, '$1')
  const sheet = await page.request.get(sheetUrl)
  expect(sheet.status()).toBe(200)
  expect(sheet.headers()['content-type']).toBe('image/jpeg')
  await page.mouse.move(box.x + box.width * 0.2, box.y - 200)
  await expect(preview).toBeHidden()

  // Chapters with images in the settings sheet; picking one jumps there.
  await page.getByRole('button', { name: 'Wiedergabe-Einstellungen' }).click()
  const chapters = page.getByRole('menu', { name: 'Wiedergabe-Einstellungen' }).getByRole('group', { name: 'Kapitel' })
  await expect(chapters.getByTestId('chapter-item')).toHaveCount(3)
  await expect(chapters.getByTestId('chapter-item').nth(2).locator('img')).toHaveAttribute('src', /\/chapters\/2\.jpg\?/)
  await chapters.getByRole('menuitem', { name: /Ende/ }).click()
  await expect.poll(() => currentTime(video)).toBeGreaterThan(29.5)
  expect(await currentTime(video)).toBeLessThan(31)
})

test('autoplay: the next episode follows after the countdown', async ({ page }) => {
  const first = await openEpisodeOne(page)
  const video = page.getByTestId('player-video')
  await startMuted(page, video)

  // 12 s before the end: the card counts down; "Abbrechen" keeps watching.
  await video.evaluate((v: HTMLVideoElement) => {
    v.currentTime = 78
  })
  const card = page.getByTestId('up-next')
  await expect(card).toBeVisible()
  await expect(card).toContainText('S1 · F2')
  await expect(page.getByTestId('up-next-countdown')).toContainText(/Nächste Folge in (10|9|8) s/)
  await card.getByRole('button', { name: 'Abbrechen' }).click()
  await expect(card).toBeHidden()
  await expect(page).toHaveURL(new RegExp(`/play/${first}$`))

  // Again from 1:18 (a fresh player): this time the countdown runs out.
  await page.goto(`/play/${first}?start=78000`)
  await expect(page.getByTestId('player-title')).toContainText('S1 · F1')
  await startMuted(page, video)
  await expect(card).toBeVisible()
  await expect(page.getByTestId('player-title')).toContainText('S1 · F2', { timeout: 20_000 })
  await expect(page).not.toHaveURL(new RegExp(`/play/${first}$`))
  // The volume setting came along (muted), so the next episode starts by itself.
  await expect.poll(() => currentTime(video), { timeout: 15_000 }).toBeGreaterThan(0.3)
  expect(await video.evaluate((v: HTMLVideoElement) => v.muted)).toBe(true)
  // Episode 2 is the last one: no card, no "next" button.
  await expect(page.getByRole('button', { name: /^Nächste Folge/ })).toHaveCount(0)

  // Episode 1 counts as watched now.
  const watched = (await api(page, `/api/v1/items/${first}`)).body as { played: boolean }
  expect(watched.played).toBe(true)
})

test('"N" jumps to the next episode right away', async ({ page }) => {
  // Episode 1 is watched by now, so the series would offer episode 2: open it directly.
  await page.goto(`/play/${episodeOne}?start=0`)
  await expect(page.getByTestId('player-title')).toContainText('S1 · F1')
  const video = page.getByTestId('player-video')
  await startMuted(page, video)
  await wake(page)
  await expect(page.getByRole('button', { name: /^Nächste Folge: S1 · F2/ })).toBeVisible()
  await page.keyboard.press('n')
  await expect(page.getByTestId('player-title')).toContainText('S1 · F2')
})

test('MP3 audio is converted to AAC for HLS and plays in Chromium', async ({ page }) => {
  await page.goto(`/search?q=${MP3}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click({ position: { x: 8, y: 8 } })
  const info = page.waitForResponse((r) => r.url().includes('/playback-info') && r.status() === 200)
  await page.getByRole('link', { name: 'Abspielen' }).click()
  const plan = ((await (await info).json()) as { sources: { plan: { audio: { action: string; codec: string } } }[] })
    .sources[0]!.plan
  expect(plan.audio).toMatchObject({ action: 'transcode', codec: 'aac' })
  await expect(page.getByTestId('play-method')).toHaveText('Transcode')

  const video = page.getByTestId('player-video')
  await startMuted(page, video)
  await expect.poll(() => currentTime(video), { timeout: 20_000 }).toBeGreaterThan(2)
  await expect(page.getByRole('alert')).toHaveCount(0)
  expect(await video.evaluate((v: HTMLVideoElement) => v.error)).toBeNull()
})
