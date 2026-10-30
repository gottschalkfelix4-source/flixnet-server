import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

// The E2E server runs with the stub embedder (FLIXNET_AI__EMBEDDER=stub): it links
// "Zeitreise"/"Zeitmaschine" and "Dinosaurier"/"T-Rex" without any model download.
// Unique titles so parallel specs sharing the server don't interfere.
const WATCHED = 'Urzeitreise Alpha'
const SIMILAR = 'Urzeitreise Beta'
const OTHER = 'Pariser Sommerliebe'
const QUERY = 'Film über Zeitreisen mit Dinosauriern'

const MOVIES = [
  { title: WATCHED, plot: 'Ein Forscher reist mit einer Zeitmaschine in die Kreidezeit und flieht vor einem T-Rex.', genre: 'Abenteuer' },
  { title: SIMILAR, plot: 'Kinder entdecken auf einer Insel lebende Dinosaurier und eine alte Zeitmaschine.', genre: 'Abenteuer' },
  { title: 'Urzeitreise Gamma', plot: 'Im Museum schlüpft ein Dinosaurier aus einem Ei aus der Kreidezeit.', genre: 'Abenteuer' },
  { title: OTHER, plot: 'Zwei Fremde verlieben sich an einem Sommerabend in Paris.', genre: 'Liebesfilm' },
]

function movieRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-foryou-'))
  for (const [n, movie] of MOVIES.entries()) {
    const folder = join(root, `${movie.title} (${2010 + n})`)
    mkdirSync(folder)
    execFileSync('ffmpeg', [
      '-hide_banner', '-loglevel', 'error', '-y',
      '-f', 'lavfi', '-i', 'testsrc=s=320x240:r=25:d=1',
      '-c:v', 'mpeg4', join(folder, `${movie.title}.${2010 + n}.mkv`),
    ])
    // Artwork too, so the home hero (newest titles with a backdrop) stays filled.
    for (const [file, size] of [['poster.jpg', '200x300'], ['fanart.jpg', '640x360']]) {
      execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', `color=c=0x14532d:s=${size}`, '-frames:v', '1', join(folder, file)])
    }
    writeFileSync(
      join(folder, 'movie.nfo'),
      `<movie><title>${movie.title}</title><year>${2010 + n}</year><plot>${movie.plot}</plot>` +
        `<genre>${movie.genre}</genre><mpaa>FSK 6</mpaa></movie>`,
    )
  }
  return root
}

/** API call with the stored session token. */
async function api(page: Page, method: string, path: string): Promise<unknown> {
  return page.evaluate(
    async ({ method, path }) => {
      const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
      const response = await fetch(path, { method, headers: { Authorization: `Bearer ${token}` } })
      return response.status === 204 ? null : response.json()
    },
    { method, path },
  )
}

type Page_ = { items: { id: number; title: string }[]; total: number }

test.describe.configure({ mode: 'serial' })

test('the library is scanned and embedded', async ({ page }) => {
  test.setTimeout(120_000)
  await page.goto('/settings?tab=libraries')
  await page.getByRole('button', { name: 'Bibliothek hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('KI Filme')
  await dialog.getByLabel('Inhalt').selectOption('movies')
  await dialog.getByLabel('Ordnerpfad').fill(movieRoot())
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()
  const entry = page.getByTestId('library-list').getByRole('listitem').filter({ hasText: 'KI Filme' })
  await expect(entry.getByTestId('library-counts')).toContainText('4 Filme', { timeout: 30_000 })

  // Embeddings follow the metadata refresh on their own (debounced job in the ai pool).
  await page.goto('/settings?tab=ai')
  await expect(page.getByTestId('ai-status')).toContainText('Test-Embedder aktiv')
  await expect
    .poll(
      async () => {
        const result = (await api(page, 'GET', `/api/v1/items?search=${encodeURIComponent(QUERY)}&kind=movie`)) as Page_
        return result.items.map((i) => i.title)
      },
      { timeout: 60_000, intervals: [1000] },
    )
    .toContain(WATCHED)
})

test('semantic search finds a film that plain full-text search misses', async ({ page }) => {
  // Full text alone (sorted by title, not relevance) knows no film with all these words.
  await page.goto('/')
  const plain = (await api(page, 'GET', `/api/v1/items?search=${encodeURIComponent(QUERY)}&sort=title`)) as Page_
  expect(plain.total).toBe(0)

  await page.goto(`/search?q=${encodeURIComponent(QUERY)}`)
  const movies = page.getByTestId('search-results').getByRole('region', { name: 'Filme' })
  await expect(movies.getByTestId('poster-card').first()).toContainText(/Urzeitreise (Alpha|Beta|Gamma)/)
  await expect(movies.getByTestId('poster-card').filter({ hasText: WATCHED })).toBeVisible()
  await expect(movies.getByTestId('poster-card').filter({ hasText: OTHER })).toHaveCount(0)

  // Looking up a title still shows just that title.
  await page.goto(`/search?q=${encodeURIComponent(OTHER)}`)
  await expect(page.getByTestId('search-results').getByRole('region', { name: 'Filme' }).getByTestId('poster-card')).toHaveCount(1)
})

test('"Für dich" recommends similar titles after watching, with a reason', async ({ page }) => {
  await page.goto('/')
  const found = (await api(page, 'GET', `/api/v1/items?search=${encodeURIComponent(WATCHED)}&kind=movie&sort=title`)) as Page_
  const watched = found.items.find((i) => i.title === WATCHED)
  expect(watched).toBeDefined()
  await api(page, 'PUT', `/api/v1/items/${watched!.id}/played`)

  await page.goto('/for-you')
  await expect(page.getByRole('heading', { level: 1, name: 'Für dich' })).toBeVisible()
  const card = page.getByTestId('recommendation').filter({ hasText: SIMILAR })
  await expect(card).toBeVisible({ timeout: 15_000 })
  await expect(card.getByTestId('recommendation-reason')).toHaveText(`Weil du „${WATCHED}“ gesehen hast`)
  // Watched titles are not recommended (the reasons do mention them).
  await expect(page.getByTestId('recommendation').getByTestId('poster-card').filter({ hasText: WATCHED })).toHaveCount(0)

  // The home page gets a "Weil du … gesehen hast" row.
  await page.goto('/')
  await expect(page.getByRole('region', { name: `Weil du „${WATCHED}“ gesehen hast` })).toBeVisible()
  // Undo, so other specs see an unchanged watch state.
  await api(page, 'DELETE', `/api/v1/items/${watched!.id}/played`)
})

async function watchedId(page: Page): Promise<number> {
  const found = (await api(page, 'GET', `/api/v1/items?search=${encodeURIComponent(WATCHED)}&kind=movie&sort=title`)) as Page_
  return found.items.find((i) => i.title === WATCHED)!.id
}

test('group mode suggests titles and lets everyone vote', async ({ page }) => {
  await page.goto('/')
  // The group needs some taste to work with.
  const id = await watchedId(page)
  await api(page, 'PUT', `/api/v1/items/${id}/played`)
  await page.goto('/for-you')
  await page.getByRole('link', { name: 'Was schauen wir heute?' }).click()
  await expect(page.getByRole('heading', { level: 1, name: 'Was schauen wir heute?' })).toBeVisible()
  await page.getByRole('button', { name: 'Vorschläge holen' }).click()
  const suggestions = page.getByTestId('group-suggestion')
  await expect(suggestions.first()).toBeVisible({ timeout: 15_000 })

  // Thumbs up for the last suggestion moves it to the top as the favourite.
  const last = suggestions.last()
  const title = (await last.getByTestId('poster-card').locator('p').first().textContent())!.trim()
  await last.getByRole('button', { name: `Daumen hoch für ${title}` }).click()
  await expect(suggestions.first()).toContainText(title)
  await expect(suggestions.first()).toContainText('Favorit')
  await page.getByRole('button', { name: 'Stimmen zurücksetzen' }).click()
  await expect(page.getByText('Favorit', { exact: true })).toHaveCount(0)
  await api(page, 'DELETE', `/api/v1/items/${id}/played`)
})
