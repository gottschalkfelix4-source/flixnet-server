import { type ChildProcess, spawn } from 'node:child_process'
import { type Server, createServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import { type Page, expect, test } from '@playwright/test'

// A fake IPTV provider on a free local port: an M3U playlist, an XMLTV guide with
// programs around the current time, and endless real-time MPEG-TS streams that
// ffmpeg generates per connection (H.264/AAC, so Chromium plays them copied).

const SLOT = 30 * 60_000

/** `YYYYMMDDhhmmss +0100` (CET, to exercise time zone offsets). */
function xmltvTime(ms: number): string {
  const d = new Date(ms + 3_600_000)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getUTCFullYear()}${p(d.getUTCMonth() + 1)}${p(d.getUTCDate())}${p(d.getUTCHours())}${p(d.getUTCMinutes())}${p(d.getUTCSeconds())} +0100`
}

function guide(): string {
  const now = Date.now()
  const first = now - (now % SLOT) - 2 * SLOT
  let xml = '<?xml version="1.0" encoding="UTF-8"?>\n<tv>\n'
  xml += '<channel id="testeins.de"><display-name>Testkanal Eins</display-name></channel>\n'
  xml += '<channel id="testzwei.de"><display-name>Testkanal Zwei</display-name></channel>\n'
  for (const [id, name] of [
    ['testeins.de', 'Eins'],
    ['testzwei.de', 'Zwei'],
  ]) {
    for (let n = 0; n < 12; n++) {
      const start = first + n * SLOT
      const running = start <= now && now < start + SLOT
      const title = running && id === 'testeins.de' ? 'Die Testschau' : `${name} am Abend ${n}`
      xml += `<programme start="${xmltvTime(start)}" stop="${xmltvTime(start + SLOT)}" channel="${id}">`
      xml += `<title lang="de">${title}</title><desc lang="de">Alles über ${title} &amp; mehr.</desc>`
      xml += '<category lang="de">Magazin</category></programme>\n'
    }
  }
  return `${xml}</tv>\n`
}

let server: Server
let base = ''
const children = new Set<ChildProcess>()

test.beforeAll(async () => {
  server = createServer((request, response) => {
    const url = request.url ?? '/'
    if (url === '/playlist.m3u') {
      response.setHeader('Content-Type', 'audio/x-mpegurl')
      response.end(
        [
          '#EXTM3U',
          '#EXTINF:-1 tvg-id="testeins.de" tvg-chno="1" group-title="Test",Testkanal Eins HD',
          `${base}/stream/1.ts`,
          '#EXTINF:-1 tvg-chno="2" group-title="Test",DE: Testkanal Zwei',
          `${base}/stream/2.ts`,
          '',
        ].join('\n'),
      )
    } else if (url === '/guide.xml') {
      response.setHeader('Content-Type', 'application/xml')
      response.end(guide())
    } else if (url.startsWith('/stream/')) {
      const ffmpeg = spawn(
        'ffmpeg',
        [
          '-hide_banner', '-loglevel', 'error', '-nostdin', '-re',
          '-f', 'lavfi', '-i', 'testsrc2=s=640x360:r=25',
          '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
          '-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency', '-g', '25', '-pix_fmt', 'yuv420p',
          '-c:a', 'aac', '-b:a', '96k', '-f', 'mpegts', 'pipe:1',
        ],
        { stdio: ['ignore', 'pipe', 'ignore'] },
      )
      children.add(ffmpeg)
      response.setHeader('Content-Type', 'video/mp2t')
      ffmpeg.stdout.pipe(response)
      const stop = () => {
        ffmpeg.kill('SIGKILL')
        children.delete(ffmpeg)
      }
      request.on('close', stop)
      ffmpeg.on('exit', () => response.end())
    } else {
      response.statusCode = 404
      response.end()
    }
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`
})

test.afterAll(async () => {
  for (const child of children) child.kill('SIGKILL')
  server.closeAllConnections()
  await new Promise((resolve) => server.close(resolve))
})

test.describe.configure({ mode: 'serial' })

/** Admin API call with the stored session token. */
async function api(page: Page, path: string): Promise<unknown> {
  return page.evaluate(async (url) => {
    const token = JSON.parse(localStorage.getItem('flixnet.session') ?? '{}').token
    const response = await fetch(url, { headers: { Authorization: `Bearer ${token}` } })
    return response.json()
  }, path)
}

test('admin adds an M3U tuner and an XMLTV guide; channels get matched', async ({ page }) => {
  await page.goto('/live')
  await expect(page.getByRole('heading', { name: 'Noch keine Sender' })).toBeVisible()

  await page.goto('/settings?tab=livetv')
  await page.getByRole('button', { name: 'Tuner hinzufügen' }).click()
  let dialog = page.getByRole('dialog')
  await dialog.getByLabel('Art').selectOption('m3u')
  await dialog.getByLabel('Name').fill('Test-IPTV')
  await dialog.getByLabel('Playlist (URL oder Datei)').fill(`${base}/playlist.m3u`)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  await expect(dialog).toBeHidden()
  const tuner = page.getByTestId('tuner-list').getByRole('listitem').filter({ hasText: 'Test-IPTV' })
  await expect(tuner.getByTestId('tuner-summary')).toContainText('2 Sender', { timeout: 30_000 })

  await page.getByRole('button', { name: 'Programmquelle hinzufügen' }).click()
  dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name').fill('Test-EPG')
  await dialog.getByLabel('XMLTV-Adresse oder Datei').fill(`${base}/guide.xml`)
  await dialog.getByRole('button', { name: 'Speichern' }).click()
  const source = page.getByTestId('epg-source-list').getByRole('listitem').filter({ hasText: 'Test-EPG' })
  await expect(source.getByTestId('epg-source-summary')).toContainText('24 Sendungen', { timeout: 30_000 })

  // Both channels matched automatically: by tvg-id and by name ("DE: Testkanal Zwei").
  const mapping = page.getByTestId('channel-mapping')
  await expect(mapping.getByTestId('channel-mapping-row')).toHaveCount(2)
  await expect(mapping.getByTestId('channel-mapping-row').filter({ hasText: 'Testkanal Eins HD' }).getByTestId('channel-epg')).toContainText(
    'Testkanal Eins',
    { timeout: 15_000 },
  )
  await expect(mapping.getByTestId('channel-mapping-row').filter({ hasText: 'DE: Testkanal Zwei' }).getByTestId('channel-epg')).toContainText(
    'Testkanal Zwei',
  )

  // Fix a match by hand.
  await mapping.getByTestId('channel-mapping-row').filter({ hasText: 'DE: Testkanal Zwei' }).getByTestId('channel-epg').click()
  dialog = page.getByRole('dialog')
  await dialog.getByLabel('Programm-Sender suchen').fill('eins')
  await dialog.getByTestId('epg-channel-options').getByRole('button', { name: /Testkanal Eins/ }).click()
  await expect(dialog).toBeHidden()
  const zwei = mapping.getByTestId('channel-mapping-row').filter({ hasText: 'DE: Testkanal Zwei' })
  await expect(zwei.getByTestId('channel-epg')).toContainText('manuell')
  // …and hand it back to the matcher.
  await zwei.getByTestId('channel-epg').click()
  await page.getByRole('dialog').getByRole('button', { name: 'Automatisch' }).click()
  await expect(zwei.getByTestId('channel-epg')).toContainText('Testkanal Zwei')
  await expect(zwei.getByTestId('channel-epg')).not.toContainText('manuell')
})

test('the guide shows programs around now and opens details', async ({ page }) => {
  await page.goto('/live')
  const grid = page.getByTestId('guide-grid')
  await expect(grid).toBeVisible()
  await expect(grid.getByTestId('guide-row')).toHaveCount(2)
  await expect(grid.getByTestId('guide-now-line')).toBeVisible()
  const current = grid.getByTestId('guide-program').filter({ hasText: 'Die Testschau' })
  await expect(current).toBeVisible()
  // Only the visible time range is rendered (horizontal virtualization).
  const rendered = await grid.getByTestId('guide-program').count()
  expect(rendered).toBeLessThan(24)

  await current.click()
  const details = page.getByRole('dialog')
  await expect(details.getByRole('heading', { name: 'Die Testschau' })).toBeVisible()
  await expect(details).toContainText('Alles über Die Testschau & mehr.')
  await expect(details.getByRole('link', { name: 'Jetzt ansehen' })).toBeVisible()
  await page.keyboard.press('Escape')

  // Program search.
  await page.getByLabel('Sendungen suchen').fill('Testschau')
  await expect(page.getByTestId('program-results').getByRole('button', { name: /Die Testschau/ })).toBeVisible()

  // The channel view shows what is on now.
  await page.getByRole('tab', { name: 'Sender' }).click()
  await expect(page.getByTestId('channel-list').getByRole('listitem').filter({ hasText: 'Testkanal Eins HD' })).toContainText('Die Testschau')
})

test('watching a channel plays the live stream and stops it on close', async ({ page }) => {
  await page.goto('/live')
  await page.getByTestId('guide-grid').getByTestId('guide-program').filter({ hasText: 'Die Testschau' }).click()
  await page.getByRole('dialog').getByRole('link', { name: 'Jetzt ansehen' }).click()
  await expect(page).toHaveURL(/\/live\/\d+$/)
  await expect(page.getByTestId('live-title')).toContainText('Testkanal Eins HD')
  await expect(page.getByTestId('live-method')).toHaveText('Kopie', { timeout: 30_000 })

  const video = page.getByTestId('live-video')
  await video.evaluate((v: HTMLVideoElement) => {
    v.muted = true
    return v.play().catch(() => {})
  })
  await expect
    .poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 30_000 })
    .toBeGreaterThan(1)
  const playing = await video.evaluate((v: HTMLVideoElement) => v.currentTime)
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 10_000 }).toBeGreaterThan(playing + 1)

  const streams = (await api(page, '/api/v1/livetv/streams')) as { viewers: number[] }[]
  expect(streams).toHaveLength(1)
  expect(streams[0].viewers).toHaveLength(1)

  // Closing the player ends the viewer session, and with it the stream.
  await page.getByRole('button', { name: 'Zurück' }).first().click()
  await expect(page.getByTestId('guide-grid')).toBeVisible()
  await expect.poll(() => api(page, '/api/v1/livetv/streams'), { timeout: 10_000 }).toEqual([])
})
