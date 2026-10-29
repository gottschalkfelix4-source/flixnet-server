import { expect, test } from '@playwright/test'

test('home page renders the shell and talks to the server', async ({ page }) => {
  await page.goto('/')

  await expect(page).toHaveTitle('Flixnet')
  const nav = page.getByRole('navigation', { name: 'Hauptnavigation' })
  for (const label of ['Startseite', 'Filme', 'Serien', 'Musik', 'Live TV', 'Für dich', 'Suche']) {
    await expect(nav.getByRole('link', { name: label })).toBeVisible()
  }
  await expect(page.getByTestId('server-status')).toContainText(/Flixnet \d+\.\d+\.\d+/)
})

test('navigating via the sidebar changes the page', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('navigation', { name: 'Hauptnavigation' }).getByRole('link', { name: 'Filme' }).click()

  await expect(page).toHaveURL(/\/movies$/)
  await expect(page.getByRole('heading', { level: 1, name: 'Filme' })).toBeVisible()
})

test('deep links survive a reload (SPA fallback)', async ({ page }) => {
  await page.goto('/shows')
  await expect(page.getByRole('heading', { level: 1, name: 'Serien' })).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', { level: 1, name: 'Serien' })).toBeVisible()
})

test('sidebar collapses to an icon rail and remembers it', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: 'Seitenleiste einklappen' }).click()
  await expect(page.getByRole('button', { name: 'Seitenleiste ausklappen' })).toBeVisible()

  await page.reload()
  await expect(page.getByRole('button', { name: 'Seitenleiste ausklappen' })).toBeVisible()
})

test('unknown API routes are JSON 404s, not the app', async ({ request }) => {
  const response = await request.get('/api/v1/does-not-exist')
  expect(response.status()).toBe(404)
})
