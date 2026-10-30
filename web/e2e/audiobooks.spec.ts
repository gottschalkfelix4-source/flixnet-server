import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

const BOOK = 'Das Testhörbuch'

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

const audioTime = (page: Page) => page.getByTestId('audiobook-audio').evaluate((a: HTMLAudioElement) => a.currentTime)

test('audiobook: chapters across parts and resume per book', async ({ page }) => {
  // Two 40 s parts with book tags.
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-books-'))
  const folder = join(root, 'Erika Erzählerin', BOOK)
  mkdirSync(folder, { recursive: true })
  for (const [n, freq] of [[1, 300], [2, 600]] as const) {
    execFileSync('ffmpeg', [
      '-hide_banner', '-loglevel', 'error', '-y',
      '-f', 'lavfi', '-i', `sine=frequency=${freq}:duration=40`,
      '-c:a', 'libmp3lame', '-b:a', '32k',
      '-metadata', `album=${BOOK}`, '-metadata', 'artist=Erika Erzählerin', '-metadata', 'composer=Sven Sprecher',
      join(folder, `Kapitel ${n}.mp3`),
    ])
  }
  await page.goto('/')
  const library = (await api(page, 'POST', '/api/v1/libraries', { name: 'Hörbücher', kind: 'audiobooks', paths: [root] })) as { id: number }
  await expect
    .poll(
      async () => {
        const l = (await api(page, 'GET', `/api/v1/libraries/${library.id}`)) as { counts: { files: number; unprobed: number } }
        return l.counts.files === 2 && l.counts.unprobed === 0
      },
      { timeout: 60_000 },
    )
    .toBe(true)

  await page.goto('/audiobooks')
  const card = page.getByTestId('audiobook-card').filter({ hasText: BOOK })
  await expect(card).toContainText('Erika Erzählerin')
  await card.click()
  await expect(page.getByTestId('audiobook-title')).toHaveText(BOOK)
  await expect(page.getByText('gelesen von Sven Sprecher')).toBeVisible()

  // Jump to the second part's chapter, listen a little, pause: the position is saved.
  await page.getByTestId('audiobook-audio').evaluate((a: HTMLAudioElement) => {
    a.muted = true
  })
  await page.getByRole('button', { name: /Kapitel 2/ }).click()
  await expect(page.getByTestId('audiobook-position')).toHaveText('0:40')
  await page.getByTestId('audiobook-play').click()
  await expect.poll(() => audioTime(page), { timeout: 10_000 }).toBeGreaterThan(1.5)
  await page.getByTestId('audiobook-play').click()
  const id = Number(new URL(page.url()).pathname.split('/').pop())
  await expect
    .poll(async () => ((await api(page, 'GET', `/api/v1/audiobooks/${id}`)) as { position_ms: number }).position_ms, {
      timeout: 10_000,
    })
    .toBeGreaterThan(41_000)

  // Speed and sleep timer.
  await page.getByLabel('Wiedergabetempo').selectOption('1.5')
  expect(await page.getByTestId('audiobook-audio').evaluate((a: HTMLAudioElement) => a.playbackRate)).toBe(1.5)
  await page.getByLabel('Schlummerfunktion').selectOption('15')
  await expect(page.getByTestId('sleep-remaining')).toHaveText(/^1[45]:\d\d$/)

  // Back later: the book resumes in part 2 where it was left.
  await page.reload()
  await expect(page.getByTestId('audiobook-position')).toHaveText(/^0:4[1-9]$/)
  await expect(page.getByRole('button', { name: 'Fortsetzen' })).toBeVisible()
  await expect(page.getByTestId('audiobook-audio')).toHaveAttribute('src', /\/api\/v1\/stream\/\d+\?/)
  await expect.poll(() => audioTime(page), { timeout: 10_000 }).toBeGreaterThan(1)
  expect(await page.getByRole('button', { name: /Kapitel 2/ }).getAttribute('aria-current')).toBe('true')
  expect(await page.getByTestId('audiobook-audio').evaluate((a: HTMLAudioElement) => a.playbackRate)).toBe(1.5)
})
