import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'
import { ADMIN_STATE } from './fixtures'

const MOVIE = 'Gemeinsamfilm'

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
      const text = await response.text()
      return text ? JSON.parse(text) : null
    },
    { method, path, body },
  )
}

/** A 60 s VP9/Opus film that Chromium plays directly. */
async function film(page: Page): Promise<number> {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-syncplay-'))
  mkdirSync(join(root, `${MOVIE} (2024)`))
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc2=s=320x180:r=10:d=60',
    '-f', 'lavfi', '-i', 'sine=frequency=500:duration=60',
    '-c:v', 'libvpx-vp9', '-deadline', 'realtime', '-b:v', '150k', '-c:a', 'libopus',
    join(root, `${MOVIE} (2024)`, `${MOVIE}.2024.webm`),
  ])
  const library = (await api(page, 'POST', '/api/v1/libraries', { name: 'Gemeinsam', kind: 'movies', paths: [root] })) as { id: number }
  await expect
    .poll(
      async () => {
        const l = (await api(page, 'GET', `/api/v1/libraries/${library.id}`)) as { counts: { movies: number; unprobed: number } }
        return l.counts.movies === 1 && l.counts.unprobed === 0
      },
      { timeout: 60_000 },
    )
    .toBe(true)
  const page1 = (await api(page, 'GET', `/api/v1/items?library_id=${library.id}&kind=movie`)) as { items: { id: number }[] }
  return page1.items[0]!.id
}

const paused = (page: Page) => page.getByTestId('player-video').evaluate((v: HTMLVideoElement) => v.paused)
const time = (page: Page) => page.getByTestId('player-video').evaluate((v: HTMLVideoElement) => v.currentTime)

// Two browsers in one group: play and pause in one apply to the other.
test('pause in one browser pauses the other', async ({ browser }) => {
  const contexts = await Promise.all([1, 2].map(() => browser.newContext({ storageState: ADMIN_STATE })))
  // Muted, so the browser lets the group start playback without a click.
  for (const context of contexts) {
    await context.addInitScript(() => localStorage.setItem('flixnet.player.volume', JSON.stringify({ volume: 1, muted: true })))
  }
  const [a, b] = await Promise.all(contexts.map((c) => c.newPage())) as [Page, Page]
  await a.goto('/')
  const id = await film(a)

  await a.goto(`/play/${id}?start=0`)
  await expect(a.getByTestId('player-title')).toHaveText(MOVIE)
  await a.getByTestId('syncplay-button').click()
  await a.getByRole('button', { name: 'Gruppe starten' }).click()
  await expect(a).toHaveURL(/[?&]group=[0-9a-f]{16}/)
  const invite = (await a.getByTestId('syncplay-invite').textContent())!
  expect(invite).toMatch(/\/watch\/[0-9a-f]{10}$/)
  await a.keyboard.press('Escape')

  // B follows the invitation link into the same group.
  await b.goto(new URL(invite).pathname)
  await expect(b).toHaveURL(new RegExp(`/play/${id}\\?group=`))
  await expect(b.getByTestId('player-title')).toHaveText(MOVIE)
  await expect(a.getByTestId('syncplay-button')).toContainText('2', { timeout: 15_000 })
  await expect(b.getByTestId('syncplay-button')).toContainText('2')

  // Play in A → B plays.
  await a.getByTestId('player').focus()
  await a.keyboard.press('k')
  await expect.poll(() => paused(b), { timeout: 15_000 }).toBe(false)
  await expect.poll(() => time(b), { timeout: 15_000 }).toBeGreaterThan(1)
  expect(await paused(a)).toBe(false)

  // Pause in A → B pauses, at about the same position.
  await a.keyboard.press('k')
  await expect.poll(() => paused(b), { timeout: 10_000 }).toBe(true)
  await expect.poll(async () => Math.abs((await time(a)) - (await time(b))), { timeout: 10_000 }).toBeLessThan(0.75)

  // And B can drive the group too.
  await b.getByTestId('player').focus()
  await b.keyboard.press('k')
  await expect.poll(() => paused(a), { timeout: 15_000 }).toBe(false)
  await b.keyboard.press('k')
  await expect.poll(() => paused(a), { timeout: 10_000 }).toBe(true)

  // Leaving: the panel offers to start or join again.
  await b.getByTestId('syncplay-button').click()
  await b.getByRole('button', { name: 'Gruppe verlassen' }).click()
  await expect(b).not.toHaveURL(/group=/)
  await expect(a.getByTestId('syncplay-button')).not.toContainText('2', { timeout: 10_000 })
  await Promise.all(contexts.map((c) => c.close()))
})
