import { expect, test } from '@playwright/test'
import { ADMIN } from './fixtures'

// These tests start signed out (no stored session).

test('protected pages redirect to sign-in and back', async ({ page }) => {
  await page.goto('/movies')
  await expect(page).toHaveURL(/\/login\?redirect=%2Fmovies$/)

  await page.getByLabel('Name').fill(ADMIN.name.toLowerCase())
  await page.getByLabel('Passwort').fill(ADMIN.password)
  await page.getByRole('button', { name: 'Anmelden' }).click()

  await expect(page).toHaveURL(/\/movies$/)
  await expect(page.getByRole('heading', { level: 1, name: 'Filme' })).toBeVisible()
})

test('wrong password shows an error', async ({ page }) => {
  await page.goto('/login')
  await page.getByLabel('Name').fill(ADMIN.name)
  await page.getByLabel('Passwort').fill('voellig-falsch')
  await page.getByRole('button', { name: 'Anmelden' }).click()
  await expect(page.getByRole('alert')).toHaveText('Name oder Passwort ist falsch.')
  await expect(page).toHaveURL(/\/login$/)
})

test('sign out returns to the sign-in page and revokes the session', async ({ page }) => {
  await page.goto('/login')
  await page.getByLabel('Name').fill(ADMIN.name)
  await page.getByLabel('Passwort').fill(ADMIN.password)
  await page.getByRole('button', { name: 'Anmelden' }).click()
  await expect(page.getByTestId('current-user')).toHaveText(ADMIN.name)

  const token = await page.evaluate(
    () => JSON.parse(localStorage.getItem('flixnet.session') ?? 'null')?.token as string,
  )
  await page.getByRole('button', { name: /^Profilmenü/ }).click()
  await page.getByRole('menuitem', { name: 'Abmelden' }).click()
  await expect(page).toHaveURL(/\/login$/)

  const me = await page.request.get('/api/v1/users/me', {
    headers: { Authorization: `Bearer ${token}` },
  })
  expect(me.status()).toBe(401)
})

test('a revoked token in storage sends the user to sign-in', async ({ page }) => {
  await page.goto('/login')
  await page.evaluate(() =>
    localStorage.setItem(
      'flixnet.session',
      JSON.stringify({ token: '0'.repeat(64), user: { id: 1, name: 'Felix', is_admin: true } }),
    ),
  )
  await page.goto('/')
  await expect(page).toHaveURL(/\/login$/)
})

// Opened as http://<server-ip>:<port>, the page is not a "secure context": browsers
// then lack crypto.randomUUID and navigator.clipboard (signing up broke on Unraid).
test('signing in works without secure-context APIs', async ({ page }) => {
  await page.addInitScript(() => {
    // @ts-expect-error -- simulate a non-secure context
    delete Crypto.prototype.randomUUID
    Object.defineProperty(navigator, 'clipboard', { value: undefined })
  })
  await page.goto('/login')
  expect(await page.evaluate(() => typeof crypto.randomUUID)).toBe('undefined')
  await page.getByLabel('Name').fill(ADMIN.name)
  await page.getByLabel('Passwort').fill(ADMIN.password)
  await page.getByRole('button', { name: 'Anmelden' }).click()
  await expect(page.getByTestId('current-user')).toHaveText(ADMIN.name)
  const device = await page.evaluate(() => JSON.parse(localStorage.getItem('flixnet.device.id') ?? 'null'))
  expect(device).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/)
})
