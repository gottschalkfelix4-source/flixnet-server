import { expect, test as setup } from '@playwright/test'
import { ADMIN, ADMIN_STATE } from './fixtures'

// Runs once against the empty server: walks the first-run wizard through the UI and
// stores the signed-in browser state for all other tests.
setup('first-run setup creates the admin account', async ({ page }) => {
  await page.goto('/')
  await expect(page).toHaveURL(/\/setup$/)
  await expect(page.getByRole('heading', { name: 'Flixnet einrichten' })).toBeVisible()

  await page.getByLabel('Name').fill(ADMIN.name)
  await page.getByLabel('Passwort', { exact: true }).fill('kurz')
  await page.getByLabel('Passwort wiederholen').fill('kurz')
  await page.getByRole('button', { name: 'Konto anlegen' }).click()
  await expect(page.getByRole('alert')).toContainText('mindestens 8 Zeichen')

  await page.getByLabel('Passwort', { exact: true }).fill(ADMIN.password)
  await page.getByLabel('Passwort wiederholen').fill(ADMIN.password)
  await page.getByRole('button', { name: 'Konto anlegen' }).click()

  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByTestId('current-user')).toHaveText(ADMIN.name)

  // The wizard is closed for good once an account exists.
  await page.goto('/setup')
  await expect(page).toHaveURL(/\/$/)

  await page.context().storageState({ path: ADMIN_STATE })
})
