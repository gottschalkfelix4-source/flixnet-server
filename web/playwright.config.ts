import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { defineConfig, devices } from '@playwright/test'

// E2E tests run against the real Rust server serving the built UI (`npm run build` first),
// with a throwaway config dir so every run starts from an empty database.
// FLIXNET_E2E_PORT lets several checkouts run their E2E suites at the same time.
const port = Number(process.env.FLIXNET_E2E_PORT ?? 18097)
const stubPort = port + 1000
const webhookPort = port + 1002
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
  projects: [
    { name: 'setup', testMatch: /global\.setup\.ts/ },
    {
      name: 'signed-in',
      testIgnore: /(auth|profiles)\.spec\.ts/,
      dependencies: ['setup'],
      use: { ...devices['Desktop Chrome'], storageState: 'e2e/.auth/admin.json' },
    },
    {
      name: 'signed-out',
      testMatch: /(auth|profiles)\.spec\.ts/,
      dependencies: ['setup'],
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: [
    {
      // Stand-in for OpenSubtitles.com (e2e/subtitles.spec.ts).
      command: 'node e2e/opensubtitles-stub.mjs',
      url: `http://127.0.0.1:${stubPort}/health`,
      reuseExistingServer: false,
      env: { STUB_PORT: String(stubPort) },
    },
    {
      // Receiver for webhook notifications (e2e/operations.spec.ts).
      command: 'node e2e/webhook-stub.mjs',
      url: `http://127.0.0.1:${webhookPort}/health`,
      reuseExistingServer: false,
      env: { STUB_PORT: String(webhookPort) },
    },
    {
      command: 'cargo run -q -p flixnet-server -- serve',
      cwd: '..',
      url: `http://127.0.0.1:${port}/api/v1/system/info`,
      timeout: 300_000,
      reuseExistingServer: false,
      env: {
        FLIXNET_SERVER__BIND: `127.0.0.1:${port}`,
        FLIXNET_PATHS__CONFIG_DIR: configDir,
        FLIXNET_CONFIG: join(configDir, 'flixnet.toml'),
        FLIXNET_OPENSUBTITLES_URL: `http://127.0.0.1:${stubPort}/api/v1`,
      },
    },
  ],
})
