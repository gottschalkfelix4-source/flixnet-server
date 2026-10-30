import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

// Scene search (6.5) and the LLM features (6.4, 6.7). The LLM is e2e/llm-stub.mjs,
// started by playwright.config.ts on the E2E port + 1001.
const llmStub = `http://127.0.0.1:${Number(process.env.FLIXNET_E2E_PORT ?? 18097) + 1001}/v1`

test.describe.configure({ mode: 'serial' })

async function api(page: Page, path: string, method = 'GET', body?: unknown): Promise<unknown> {
  return page.evaluate(
    async ([url, verb, payload]) => {
      const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
      const response = await fetch(url as string, {
        method: verb as string,
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: payload ? JSON.stringify(payload) : undefined,
      })
      return response.json()
    },
    [path, method, body] as const,
  )
}

async function addLibrary(page: Page, name: string, root: string, count: string) {
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(name)
  await dialog.getByLabel('Ordnerpfad').fill(root)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: name })
  await expect(entry.getByTestId('library-counts')).toContainText(count, { timeout: 30_000 })
}

test('scene search finds a line from the subtitles and opens the player there', async ({ page }) => {
  test.setTimeout(120_000)
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-scenes-'))
  const folder = join(root, 'Szenentest (2023)')
  mkdirSync(folder)
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'color=c=0x1e3a8a:s=320x180:r=25:d=90',
    '-f', 'lavfi', '-i', 'sine=frequency=330:duration=90',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-g', '50', '-pix_fmt', 'yuv420p',
    '-c:a', 'aac', join(folder, 'Szenentest.2023.mp4'),
  ])
  writeFileSync(
    join(folder, 'Szenentest.2023.de.srt'),
    '1\n00:00:02,000 --> 00:00:05,000\nWillkommen an Bord.\n\n' +
      '2\n00:01:10,000 --> 00:01:14,000\nDer Leuchtturm steht am Ende der Welt.\n',
  )
  await addLibrary(page, 'Szenen', root, '1 Film')

  // Indexing runs in the background after the probe.
  await expect
    .poll(async () => ((await api(page, '/api/v1/scenes/search?q=Leuchtturm')) as { hits: unknown[] }).hits.length, {
      timeout: 60_000,
    })
    .toBe(1)

  await page.goto('/search?mode=scenes')
  await expect(page.getByRole('tab', { name: 'Szenen' })).toHaveAttribute('aria-selected', 'true')
  await page.getByRole('searchbox', { name: 'Suche' }).fill('die Szene mit dem Leuchtturm')
  const hit = page.getByTestId('scene-hit')
  await expect(hit).toHaveCount(1)
  await expect(hit).toContainText('Szenentest')
  await expect(hit).toContainText('1:10')
  await expect(hit.locator('mark')).toHaveText('Leuchtturm')
  await hit.getByRole('link', { name: 'An dieser Stelle abspielen' }).click()
  await expect(page).toHaveURL(/\/play\/\d+\?start=70000$/)
  const video = page.locator('video')
  await expect
    .poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 30_000 })
    .toBeGreaterThan(69)
})

test('natural-language search and chat with a (stub) LLM', async ({ page }) => {
  test.setTimeout(120_000)
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-llm-'))
  for (const [title, year, runtime] of [
    ['Neunziger Lacher', 1994, 95],
    ['Langer Lacher', 1997, 150],
    ['Moderner Lacher', 2015, 90],
  ] as const) {
    const dir = join(root, `${title} (${year})`)
    mkdirSync(dir)
    writeFileSync(join(dir, `${title}.${year}.mkv`), Buffer.alloc(1000))
    writeFileSync(
      join(dir, 'movie.nfo'),
      `<movie><title>${title}</title><year>${year}</year><genre>Komödie</genre><runtime>${runtime}</runtime></movie>`,
    )
  }
  await addLibrary(page, 'KI-Test', root, '3 Filme')

  // Without an LLM the free-text search is a keyword search.
  await page.goto('/search?mode=natural&q=Neunziger')
  await expect(page.getByTestId('natural-filter')).toContainText('Stichwortsuche')
  await expect(page.getByTestId('natural-results').getByTestId('poster-card')).toHaveCount(1)

  await page.goto('/settings?tab=ai')
  await expect(page.getByTestId('llm-status')).toContainText('Kein Sprachmodell')
  await page.getByLabel('Adresse').fill(llmStub)
  await page.getByRole('textbox', { name: 'Modell' }).fill('stub-model')
  await page.getByLabel('Sprachmodell verwenden').check()
  await page.getByRole('button', { name: 'Speichern' }).first().click()
  await expect(page.getByTestId('llm-status')).toContainText('Aktiv: stub-model')
  try {
    await page.getByRole('button', { name: 'Verbindung testen' }).click()
    await expect(page.getByTestId('llm-test')).toContainText('„OK“')

    await page.goto('/search?mode=natural')
    await page
      .getByRole('searchbox', { name: 'Suche' })
      .fill('lustige Filme aus den 90ern unter 2 Stunden die ich noch nicht gesehen habe')
    const filter = page.getByTestId('natural-filter')
    await expect(filter).toContainText('Verstanden als:', { timeout: 15_000 })
    for (const chip of ['Filme', 'Komödie', '1990–1999', 'bis 120 Min.', 'noch nicht gesehen']) {
      await expect(filter).toContainText(chip)
    }
    const cards = page.getByTestId('natural-results').getByTestId('poster-card')
    await expect(cards).toHaveCount(1)
    await expect(cards).toContainText('Neunziger Lacher')

    // "Frag Flixnet" (sidebar): the stub calls search_catalog, then answers.
    await page.getByRole('button', { name: 'Frag Flixnet' }).click()
    const panel = page.getByRole('dialog', { name: 'Frag Flixnet' })
    await panel.getByLabel('Frage an Flixnet').fill('Welche Filme habe ich?')
    await panel.getByRole('button', { name: 'Senden' }).click()
    await expect(panel.getByTestId('chat-assistant')).toContainText('Ich habe gefunden:', { timeout: 15_000 })
    await expect(panel.getByTestId('chat-assistant')).toContainText('Neunziger Lacher')
  } finally {
    // Other specs run in parallel: switch the LLM off again.
    await api(page, '/api/v1/settings/llm', 'PUT', { enabled: false })
  }
})
