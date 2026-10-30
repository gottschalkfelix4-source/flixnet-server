import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'

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

function picture(path: string, color: string) {
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', `color=c=${color}:s=320x240`, '-frames:v', '1', path,
  ])
}

test('photo library: timeline, albums and the lightbox', async ({ page }) => {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-photos-'))
  mkdirSync(join(root, 'Sommerurlaub'))
  picture(join(root, 'Sommerurlaub', 'Strand.jpg'), 'blue')
  picture(join(root, 'Sommerurlaub', 'Düne.jpg'), 'yellow')
  picture(join(root, 'Sommerurlaub', 'Meer.png'), 'navy')
  picture(join(root, 'Balkon.jpg'), 'green')

  await page.goto('/')
  const library = (await api(page, 'POST', '/api/v1/libraries', { name: 'Fotoalbum', kind: 'photos', paths: [root] })) as { id: number }
  await expect
    .poll(async () => ((await api(page, 'GET', `/api/v1/photos?library_id=${library.id}`)) as { total: number }).total, {
      timeout: 30_000,
    })
    .toBe(4)

  await page.goto('/photos')
  const thumbs = page.getByTestId('photo-thumb')
  await expect(thumbs).toHaveCount(4)
  await expect(page.getByTestId('photo-month').first()).toContainText(/\d{4}/)

  // Lightbox: arrow keys step through, Space starts the slideshow, Escape closes.
  await thumbs.first().click()
  const lightbox = page.getByTestId('lightbox')
  await expect(lightbox).toBeVisible()
  await expect(page.getByTestId('lightbox-position')).toHaveText('1 / 4')
  const first = await page.getByTestId('lightbox-title').textContent()
  await expect(page.getByTestId('lightbox-image')).toHaveJSProperty('complete', true)
  expect(await page.getByTestId('lightbox-image').evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(320)
  await page.keyboard.press('ArrowRight')
  await expect(page.getByTestId('lightbox-position')).toHaveText('2 / 4')
  await expect(page.getByTestId('lightbox-title')).not.toHaveText(first!)
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await expect(page.getByTestId('lightbox-position')).toHaveText('4 / 4')
  await page.keyboard.press('i')
  await expect(page.getByTestId('lightbox-details')).toContainText('320 × 240 Pixel')

  await page.keyboard.press(' ')
  await expect(page.getByRole('button', { name: 'Diashow anhalten' })).toBeVisible()
  await expect(page.getByTestId('lightbox-position')).toHaveText('1 / 4', { timeout: 8_000 })
  await page.keyboard.press(' ')
  await expect(page.getByRole('button', { name: 'Diashow starten' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(lightbox).toBeHidden()

  // Albums are the folders.
  await page.getByRole('tab', { name: 'Alben' }).click()
  const album = page.getByTestId('photo-albums').getByRole('link').filter({ hasText: 'Sommerurlaub' })
  await expect(album).toContainText('3 Fotos')
  await album.click()
  await expect(page).toHaveURL(/album=\d+/)
  await expect(page.getByRole('heading', { name: 'Sommerurlaub' })).toBeVisible()
  await expect(page.getByTestId('photo-thumb')).toHaveCount(3)
  await page.getByRole('button', { name: 'Zurück' }).click()
  await expect(page.getByTestId('photo-albums')).toBeVisible()
  await page.getByRole('tab', { name: 'Zeitleiste' }).click()
  await expect(page.getByTestId('photo-thumb')).toHaveCount(4)
})
