import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { defineConfig, devices } from '@playwright/test'

// E2E tests run against the real Rust server serving the built UI (`npm run build` first),
// with a throwaway config dir so every run starts from an empty database.
const port = 18097
const configDir = mkdtempSync(join(tmpdir(), 'flixnet-e2e-'))

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: 'retain-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'cargo run -q -p flixnet-server -- serve',
    cwd: '..',
    url: `http://127.0.0.1:${port}/api/v1/system/info`,
    timeout: 300_000,
    reuseExistingServer: false,
    env: {
      FLIXNET_SERVER__BIND: `127.0.0.1:${port}`,
      FLIXNET_PATHS__CONFIG_DIR: configDir,
      FLIXNET_CONFIG: join(configDir, 'flixnet.toml'),
    },
  },
})
