import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

// Unique titles so parallel specs sharing the server don't interfere.
const MOVIE = 'Zephyrkino'
const SERIES = 'Quasarwelle'
const GENRE = 'Zephyrgenre'

function video(path: string) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc=s=320x240:r=25:d=1',
    '-c:v', 'mpeg4', path,
  ])
}

/** Solid-colour artwork so the hero (which needs a backdrop) has something to show. */
function image(path: string, size: string) {
  execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', `color=c=0x1e3a8a:s=${size}`, '-frames:v', '1', path])
}

function movieRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-browse-movies-'))
  const folder = join(root, `${MOVIE} (2019)`)
  mkdirSync(folder)
  video(join(folder, `${MOVIE}.2019.German.1080p.WEB.x264-FLX.mkv`))
  image(join(folder, 'poster.jpg'), '200x300')
  image(join(folder, 'fanart.jpg'), '640x360')
  writeFileSync(
    join(folder, 'movie.nfo'),
    `<movie><title>${MOVIE}</title><year>2019</year><plot>Ein Kino im Wind erzählt von Pixeln.</plot>` +
      `<genre>${GENRE}</genre><mpaa>FSK 12</mpaa><rating>7.4</rating>` +
      '<director>Regina Rahmen</director>' +
      '<actor><name>Bruno Bildpunkt</name><role>Filmvorführer</role><order>0</order></actor></movie>',
  )
  return root
}

function seriesRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-browse-shows-'))
  const show = join(root, SERIES)
  const season = join(show, 'Season 01')
  mkdirSync(season, { recursive: true })
  writeFileSync(
    join(show, 'tvshow.nfo'),
    `<tvshow><title>${SERIES}</title><year>2022</year><plot>Wellen aus dem All.</plot><genre>${GENRE}</genre></tvshow>`,
  )
  for (const [n, title] of [[1, 'Das Signal'], [2, 'Die Antwort']] as const) {
    const stem = `${SERIES}.S01E0${n}.German.1080p.WEB.x264-FLX`
    video(join(season, `${stem}.mkv`))
    writeFileSync(
      join(season, `${stem}.nfo`),
      `<episodedetails><title>${title}</title><season>1</season><episode>${n}</episode>` +
        `<plot>Folge ${n} der Testserie.</plot></episodedetails>`,
    )
  }
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
  await expect(entry.getByTestId('library-counts')).toContainText(counts, { timeout: 20_000 })
  await expect(entry.getByTestId('library-counts')).not.toContainText('analysiert', { timeout: 20_000 })
}

test.describe.configure({ mode: 'serial' })

test('libraries for browsing get scanned', async ({ page }) => {
  await addLibrary(page, 'Browse Filme', 'movies', movieRoot(), '1 Film')
  await addLibrary(page, 'Browse Serien', 'shows', seriesRoot(), '2 Folgen')
})

test('search finds the movie and opens its detail page', async ({ page }) => {
  await page.goto('/search')
  await page.getByLabel('Suche').fill('zephyr')
  await expect(page).toHaveURL(/q=zephyr/)
  const results = page.getByTestId('search-results')
  await expect(results.getByRole('region', { name: 'Filme' }).getByTestId('poster-card')).toHaveCount(1)
  await expect(results.getByRole('region', { name: 'Serien' })).toHaveCount(0)

  await results.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click()
  await expect(page.getByRole('heading', { name: MOVIE })).toBeVisible()
  await expect(page.getByTestId('item-overview')).toHaveText('Ein Kino im Wind erzählt von Pixeln.')
  await expect(page.getByTestId('item-header')).toContainText('FSK 12')
  await expect(page.getByTestId('cast-row')).toContainText('Bruno Bildpunkt')
  await expect(page.getByTestId('cast-row')).toContainText('Filmvorführer')
})

test('watched and favorite toggles persist', async ({ page }) => {
  await page.goto(`/search?q=${MOVIE}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click()
  await page.getByRole('button', { name: 'Als gesehen markieren' }).click()
  await expect(page.getByRole('button', { name: 'Als ungesehen markieren' })).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('button', { name: 'Zu Favoriten' }).click()
  await expect(page.getByRole('button', { name: 'Aus Favoriten entfernen' })).toBeVisible()

  await page.reload()
  await expect(page.getByRole('button', { name: 'Als ungesehen markieren' })).toBeVisible()
  await page.getByRole('button', { name: 'Als ungesehen markieren' }).click()
  await expect(page.getByRole('button', { name: 'Als gesehen markieren' })).toHaveAttribute('aria-pressed', 'false')
})

test('person page lists their work', async ({ page }) => {
  await page.goto(`/search?q=${MOVIE}`)
  await page.getByRole('region', { name: 'Filme' }).getByTestId('poster-card').click()
  await page.getByTestId('cast-row').getByRole('link', { name: /Bruno Bildpunkt/ }).click()
  await expect(page.getByRole('heading', { name: 'Bruno Bildpunkt' })).toBeVisible()
  await expect(page.getByTestId('person-credits')).toContainText('Filmvorführer')
  await expect(page.getByTestId('person-credits').getByTestId('poster-card')).toHaveCount(1)
})

test('series page shows seasons and episodes', async ({ page }) => {
  await page.goto(`/search?q=${SERIES}`)
  await page.getByRole('region', { name: 'Serien' }).getByTestId('poster-card').click()
  await expect(page.getByRole('heading', { name: SERIES })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'Staffel 1' })).toHaveAttribute('aria-selected', 'true')
  const episodes = page.getByTestId('episode-list').getByRole('link')
  await expect(episodes).toHaveCount(2)
  await expect(episodes.first()).toContainText('Das Signal')
  await expect(episodes.first()).toContainText('Folge 1 der Testserie.')

  await episodes.nth(1).click()
  await expect(page.getByRole('heading', { name: 'Die Antwort' })).toBeVisible()
  await page.getByRole('link', { name: SERIES }).click()
  await expect(page.getByTestId('episode-list')).toBeVisible()
})

test('movie grid filters by genre', async ({ page }) => {
  await page.goto('/movies')
  await expect(page.getByTestId('virtual-grid')).toBeVisible()
  await page.getByLabel('Genre').selectOption(GENRE)
  await expect(page).toHaveURL(new RegExp(`genre=${GENRE}`))
  await expect(page.getByTestId('browse-total')).toHaveText('1 Film')
  await expect(page.getByTestId('virtual-grid').getByTestId('poster-card')).toHaveCount(1)
  await expect(page.getByTestId('virtual-grid')).toContainText(MOVIE)
})

test('home shows the hero and rows', async ({ page }) => {
  await page.goto('/')
  // Many specs share the server; the home page can take a moment under load.
  await expect(page.getByTestId('hero')).toBeVisible({ timeout: 15_000 })
  await expect(page.getByRole('region', { name: 'Neu: Filme' })).toContainText(MOVIE)
})
