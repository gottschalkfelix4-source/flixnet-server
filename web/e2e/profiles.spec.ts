import { type Page, expect, test } from '@playwright/test'
import { ADMIN } from './fixtures'

// Profile switching revokes the device's token, so these tests sign in with their own
// browser (own device id) instead of the shared admin session.

async function signIn(page: Page, name = ADMIN.name, password = ADMIN.password) {
  await page.goto('/login')
  await page.getByLabel('Name').fill(name)
  await page.getByLabel('Passwort').fill(password)
  await page.getByRole('button', { name: 'Anmelden' }).click()
  await expect(page.getByTestId('current-user')).toHaveText(name)
}

async function createUser(page: Page, name: string, options: { pin?: string } = {}) {
  await page.goto('/settings?tab=users')
  await page.getByRole('button', { name: 'Nutzer anlegen' }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill(name)
  if (options.pin) await dialog.getByLabel('PIN').fill(options.pin)
  await dialog.getByLabel('Altersfreigabe').selectOption({ label: 'Ab 12 (FSK 12)' })
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()
  await expect(page.getByRole('button', { name: new RegExp(name) })).toBeVisible()
}

test('household profiles: create, switch with and without PIN, back to admin', async ({ page }) => {
  await signIn(page)
  const suffix = Date.now().toString().slice(-5)
  const kid = `Kind${suffix}`
  const teen = `Teen${suffix}`
  await createUser(page, kid)
  await createUser(page, teen, { pin: '2580' })

  // Switch to the kid profile: no PIN.
  await page.getByRole('link', { name: 'Profil wechseln' }).click()
  await expect(page.getByRole('heading', { name: 'Wer schaut?' })).toBeVisible()
  await page.getByRole('button', { name: kid }).click()
  await expect(page.getByTestId('current-user')).toHaveText(kid)

  // Members cannot see the user administration.
  await page.goto('/settings')
  await expect(page.getByRole('link', { name: 'Nutzer', exact: true })).toHaveCount(0)

  // Teen profile needs its PIN.
  await page.goto('/profiles')
  await page.getByRole('button', { name: teen }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('PIN').fill('0000')
  await dialog.getByRole('button', { name: 'Entsperren' }).click()
  await expect(dialog.getByRole('alert')).toHaveText('Die PIN ist falsch.')
  await dialog.getByLabel('PIN').fill('2580')
  await dialog.getByRole('button', { name: 'Entsperren' }).click()
  await expect(page.getByTestId('current-user')).toHaveText(teen)

  // Back to the admin requires the admin password.
  await page.goto('/profiles')
  await page.getByRole('button', { name: ADMIN.name }).click()
  await page.getByRole('dialog').getByLabel('Passwort').fill(ADMIN.password)
  await page.getByRole('dialog').getByRole('button', { name: 'Entsperren' }).click()
  await expect(page.getByTestId('current-user')).toHaveText(ADMIN.name)
})

test('invitation link creates a new account', async ({ page, browser }) => {
  await signIn(page)
  await page.goto('/settings?tab=invitations')
  await page.getByLabel('Notiz').fill('Für Oma')
  await page.getByRole('button', { name: 'Link erstellen' }).click()
  const link = (await page.getByTestId('invite-link').textContent())!
  expect(link).toMatch(/\/invite\/[0-9a-f]{64}$/)

  // Redeem in a separate, signed-out browser.
  const guest = await browser.newContext()
  const guestPage = await guest.newPage()
  const name = `Oma${Date.now().toString().slice(-5)}`
  await guestPage.goto(new URL(link).pathname)
  await expect(guestPage.getByRole('heading', { name: 'Du bist eingeladen' })).toBeVisible()
  await guestPage.getByLabel('Name').fill(name)
  await guestPage.getByLabel('Passwort').fill('oma-passwort-1')
  await guestPage.getByRole('button', { name: 'Konto anlegen' }).click()
  await expect(guestPage.getByTestId('current-user')).toHaveText(name)

  // The link is single-use.
  await guestPage.goto(new URL(link).pathname)
  await expect(guestPage.getByRole('heading', { name: 'Einladung ungültig' })).toBeVisible()
  await guest.close()

  await page.reload()
  await expect(page.getByText(/Eingelöst/)).toBeVisible()
})
