import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

// Milestone 5.3: playlists, smart playlists and manual collections through the UI.
// Unique titles so parallel specs sharing the server don't interfere.
const PREFIX = 'Kollektora'
const ONE = `${PREFIX} Eins`
const TWO = `${PREFIX} Zwei`
const THREE = `${PREFIX} Drei`
const SERIES = `${PREFIX} Serie`
const GENRE = 'Kollektorgenre'
const PLAYLIST = `${PREFIX} Abend`
const SMART = `${PREFIX} Smart`
const COLLECTION = `${PREFIX} Sammlung`

function video(path: string) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc=s=320x240:r=25:d=1',
    '-c:v', 'mpeg4', path,
  ])
}

function movieRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-collections-movies-'))
  for (const [title, year, genre, rating] of [
    [ONE, 2001, GENRE, '8.0'],
    [TWO, 2002, GENRE, '6.0'],
    [THREE, 2003, 'Kollektorsonst', '9.0'],
  ] as const) {
    const folder = join(root, `${title} (${year})`)
    mkdirSync(folder)
    video(join(folder, `${title.replace(' ', '.')}.${year}.mkv`))
    writeFileSync(
      join(folder, 'movie.nfo'),
      `<movie><title>${title}</title><year>${year}</year><genre>${genre}</genre><rating>${rating}</rating><mpaa>FSK 0</mpaa></movie>`,
    )
  }
  return root
}

function seriesRoot(): string {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-collections-shows-'))
  const show = join(root, SERIES)
  const season = join(show, 'Season 01')
  mkdirSync(season, { recursive: true })
  writeFileSync(join(show, 'tvshow.nfo'), `<tvshow><title>${SERIES}</title><mpaa>FSK 0</mpaa></tvshow>`)
  for (const [n, title] of [[1, 'Erste Folge'], [2, 'Zweite Folge']] as const) {
    const stem = `Kollektora.Serie.S01E0${n}`
    video(join(season, `${stem}.mkv`))
    writeFileSync(
      join(season, `${stem}.nfo`),
      `<episodedetails><title>${title}</title><season>1</season><episode>${n}</episode></episodedetails>`,
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

async function openItem(page: Page, title: string, region: 'Filme' | 'Serien' = 'Filme') {
  await page.goto(`/search?q=${encodeURIComponent(title)}`)
  await page.getByRole('region', { name: region }).getByTestId('poster-card').filter({ hasText: title }).click()
  await expect(page.getByRole('heading', { name: title })).toBeVisible()
}

async function addToPlaylist(page: Page, title: string, region: 'Filme' | 'Serien' = 'Filme') {
  await openItem(page, title, region)
  await page.getByTestId('item-header').getByRole('button', { name: 'Zur Playlist hinzufügen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: new RegExp(PLAYLIST) }).click()
  await expect(dialog.getByRole('status')).toHaveText(`Zu „${PLAYLIST}“ hinzugefügt.`)
  await dialog.getByRole('button', { name: 'Fertig' }).click()
  await expect(dialog).toBeHidden()
}

const entryTitles = (page: Page) => page.getByTestId('playlist-entries').getByTestId('playlist-entry-title')

test.describe.configure({ mode: 'serial' })

test('libraries for collections get scanned', async ({ page }) => {
  await addLibrary(page, 'Kollektora Filme', 'movies', movieRoot(), '3 Filme')
  await addLibrary(page, 'Kollektora Serien', 'shows', seriesRoot(), '2 Folgen')
})

test('a playlist is created, filled and reordered', async ({ page }) => {
  await page.goto('/playlists')
  await page.getByRole('button', { name: 'Neue Playlist', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(PLAYLIST)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(page.getByRole('heading', { name: PLAYLIST })).toBeVisible()
  await expect(page.getByText('Noch leer.')).toBeVisible()

  await addToPlaylist(page, ONE)
  await addToPlaylist(page, TWO)
  // A whole series adds all its episodes.
  await addToPlaylist(page, SERIES, 'Serien')

  await page.goto('/playlists')
  await page.getByTestId('playlist-card').filter({ hasText: PLAYLIST }).click()
  await expect(page.getByTestId('playlist-meta')).toContainText('4 Titel')
  await expect(entryTitles(page)).toHaveText([ONE, TWO, 'Erste Folge', 'Zweite Folge'])

  await page.getByRole('button', { name: `${TWO} nach oben` }).click()
  await expect(entryTitles(page)).toHaveText([TWO, ONE, 'Erste Folge', 'Zweite Folge'])
  await page.getByRole('button', { name: 'Zweite Folge nach oben' }).click()
  await expect(entryTitles(page)).toHaveText([TWO, ONE, 'Zweite Folge', 'Erste Folge'])

  // The order is stored on the server.
  await page.reload()
  await expect(entryTitles(page)).toHaveText([TWO, ONE, 'Zweite Folge', 'Erste Folge'])

  await page.getByRole('button', { name: `${ONE} entfernen` }).click()
  await expect(entryTitles(page)).toHaveText([TWO, 'Zweite Folge', 'Erste Folge'])
})

test('a smart playlist shows what matches its filter', async ({ page }) => {
  await page.goto('/playlists')
  await page.getByRole('button', { name: 'Neue Smart-Playlist' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(SMART)
  await dialog.getByRole('group', { name: 'Genres' }).getByLabel(GENRE).check()
  await dialog.getByLabel('Mindestbewertung (0–10)').fill('7')
  await dialog.getByRole('button', { name: 'Speichern' }).click()

  await expect(page.getByRole('heading', { name: SMART })).toBeVisible()
  await expect(page.getByText('Smart-Playlist', { exact: true })).toBeVisible()
  await expect(entryTitles(page)).toHaveText([ONE])
  // Smart playlists cannot be reordered by hand.
  await expect(page.getByRole('button', { name: `${ONE} nach oben` })).toHaveCount(0)

  // Editing the filter changes the result right away.
  await page.getByRole('button', { name: 'Bearbeiten' }).click()
  await page.getByRole('dialog').getByLabel('Mindestbewertung (0–10)').fill('')
  await page.getByRole('dialog').getByRole('button', { name: 'Speichern' }).click()
  await expect(entryTitles(page)).toHaveText([ONE, TWO])
})

test('an administrator curates a manual collection', async ({ page }) => {
  await page.goto('/collections')
  await page.getByRole('button', { name: 'Sammlung anlegen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(COLLECTION)
  await dialog.getByRole('button', { name: 'Anlegen' }).click()
  await expect(page.getByRole('heading', { name: COLLECTION })).toBeVisible()
  await expect(page.getByText('Noch leer.')).toBeVisible()

  await openItem(page, THREE)
  await page.getByTestId('item-header').getByRole('button', { name: 'Zur Sammlung hinzufügen' }).click()
  await page.getByRole('dialog').getByRole('button', { name: new RegExp(COLLECTION) }).click()
  await expect(page.getByRole('dialog').getByRole('status')).toHaveText(`Zu „${COLLECTION}“ hinzugefügt.`)
  await page.getByRole('dialog').getByRole('button', { name: 'Fertig' }).click()

  // "Teil der Sammlung …" on the movie's page.
  const row = page.getByRole('region', { name: `Teil der Sammlung „${COLLECTION}“` })
  await expect(row.getByTestId('poster-card')).toHaveCount(1)
  await row.getByRole('link', { name: 'Zur Sammlung' }).click()
  await expect(page.getByRole('heading', { name: COLLECTION })).toBeVisible()
  await expect(page.getByTestId('collection-counts')).toHaveText('1 Titel')
  await expect(page.getByTestId('collection-items')).toContainText(THREE)

  await page.goto('/collections')
  await expect(page.getByTestId('collection-card').filter({ hasText: COLLECTION })).toBeVisible()
  await page.getByTestId('collection-card').filter({ hasText: COLLECTION }).click()

  await page.getByRole('button', { name: `${THREE} aus der Sammlung entfernen` }).click()
  await expect(page.getByText('Noch leer.')).toBeVisible()
  await page.getByRole('button', { name: 'Sammlung löschen' }).click()
  await page.getByRole('button', { name: 'Wirklich löschen?' }).click()
  await expect(page).toHaveURL(/\/collections$/)
  await expect(page.getByTestId('collection-card').filter({ hasText: COLLECTION })).toHaveCount(0)
})
