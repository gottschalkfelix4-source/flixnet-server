import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { type Page, expect, test } from '@playwright/test'
import { absoluteUrl, castBase, castMedia, castProfile, isLocalOnly } from '../src/lib/cast'
import { bestOffset, clockSample, expectedPosition, socketUrl, startDelay, type GroupState } from '../src/lib/syncplay'

const MOVIE = 'Übertragungsfilm'

// ---- Unit tests of the pure helpers (no browser) ------------------------------------

test.describe('cast helpers', () => {
  test('profile for the Default Media Receiver', () => {
    const profile = castProfile()
    expect(profile.containers).toContain('mp4')
    expect(profile.video_codecs.map((v) => v.codec)).toEqual(['h264', 'vp8'])
    expect(profile.video_codecs[0]).toMatchObject({ max_width: 1920, max_height: 1080, max_bit_depth: 8, hdr: [] })
    expect(profile.audio_codecs.every((a) => a.max_channels === 2)).toBe(true)
    expect(profile.audio_codecs.map((a) => a.codec)).not.toContain('ac3')
  })

  test('absolute URLs from the LAN address', () => {
    expect(castBase('http://192.168.1.10:8097/', 'http://localhost:5173')).toBe('http://192.168.1.10:8097')
    expect(castBase(null, 'http://192.168.1.10:8097')).toBe('http://192.168.1.10:8097')
    expect(castBase('  ', 'http://nas:8097')).toBe('http://nas:8097')
    expect(absoluteUrl('https://flix.example.org/flixnet', '/api/v1/stream/3?sig=a')).toBe(
      'https://flix.example.org/flixnet/api/v1/stream/3?sig=a',
    )
    expect(absoluteUrl('http://a:1', 'http://b:2/x')).toBe('http://b:2/x')
    for (const local of ['http://localhost:8097', 'http://127.0.0.1:18097', 'http://[::1]:8097', 'http://app.localhost', 'kaputt']) {
      expect(isLocalOnly(local), local).toBe(true)
    }
    expect(isLocalOnly('http://192.168.188.129:8097')).toBe(false)
  })

  test('direct play or HLS for the receiver', () => {
    const direct = castMedia(
      { plan: { method: 'direct_play' }, direct_url: '/api/v1/stream/7?u=1&exp=2&sig=s', hls_url: null, container: 'mp4' },
      'http://192.168.1.10:8097',
      65_000,
    )
    expect(direct).toEqual({
      url: 'http://192.168.1.10:8097/api/v1/stream/7?u=1&exp=2&sig=s',
      contentType: 'video/mp4',
      hls: false,
      startSeconds: 65,
    })
    expect(castMedia({ plan: { method: 'direct_play' }, direct_url: '/s', container: 'webm' }, 'http://h', 0).contentType).toBe(
      'video/webm',
    )
    const hls = castMedia(
      {
        plan: { method: 'transcode' },
        direct_url: '/api/v1/stream/7',
        hls_url: '/api/v1/transcode/0123456789abcdef0123456789abcdef/main.m3u8',
        container: 'mkv',
      },
      'http://192.168.1.10:8097',
      30_500,
    )
    expect(hls.url).toBe(
      'http://192.168.1.10:8097/api/v1/transcode/0123456789abcdef0123456789abcdef/main.m3u8?start_ms=30500',
    )
    expect(hls.contentType).toBe('application/x-mpegURL')
    expect(hls.hls).toBe(true)
  })

  test('syncplay clock and positions', () => {
    // Sent at 1000, answered with server time 5050, received at 1100: offset +4000.
    const sample = clockSample(1000, 5050, 1100)
    expect(sample).toEqual({ offset: 4000, rtt: 100 })
    expect(bestOffset([{ offset: 10, rtt: 300 }, sample, { offset: -5, rtt: 150 }])).toBe(4000)
    expect(bestOffset([])).toBe(0)
    const state: GroupState = {
      group_id: 'g',
      item_id: 1,
      playing: true,
      position_ms: 10_000,
      anchor_ms: 50_000,
      server_ms: 49_500,
      waiting: false,
      members: [],
      revision: 3,
    }
    expect(expectedPosition(state, 49_000)).toBe(10_000)
    expect(expectedPosition(state, 52_500)).toBe(12_500)
    expect(startDelay(state, 49_600)).toBe(400)
    expect(startDelay({ ...state, playing: false }, 0)).toBe(0)
    expect(expectedPosition({ ...state, playing: false }, 99_999)).toBe(10_000)
    expect(socketUrl({ protocol: 'https:', host: 'flix:8097' }, 'abc')).toBe('wss://flix:8097/api/v1/syncplay/groups/abc/ws')
  })
})

// ---- The cast flow against a stand-in for the Cast SDK ----------------------------------

/** A minimal Google Cast sender SDK: one device "Wohnzimmer-TV" that records loads. */
function installCastMock() {
  type Listener = () => void
  const w = window as unknown as Record<string, unknown>
  const loads: unknown[] = []
  w.__castLoads = loads
  const listeners: Listener[] = []
  const notify = () => listeners.forEach((l) => l())
  const player = { isConnected: false, isPaused: true, currentTime: 0, duration: 0, playerState: null }
  let session: { loadMedia: (r: unknown) => Promise<void>; getCastDevice: () => { friendlyName: string } } | null = null
  const context = {
    setOptions(options: unknown) {
      w.__castOptions = options
    },
    getCastState: () => (session ? 'CONNECTED' : 'NOT_CONNECTED'),
    async requestSession() {
      session = {
        async loadMedia(request: unknown) {
          loads.push(JSON.parse(JSON.stringify(request)))
          const r = request as { currentTime: number }
          Object.assign(player, { isConnected: true, isPaused: false, currentTime: r.currentTime, duration: 30 })
          notify()
        },
        getCastDevice: () => ({ friendlyName: 'Wohnzimmer-TV' }),
      }
    },
    getCurrentSession: () => session,
    endCurrentSession() {
      session = null
      player.isConnected = false
      notify()
    },
    addEventListener() {},
    removeEventListener() {},
  }
  function Ctor(this: Record<string, unknown>, a?: unknown, b?: unknown) {
    this.a = a
    this.b = b
  }
  w.chrome = {
    cast: {
      AutoJoinPolicy: { ORIGIN_SCOPED: 'origin_scoped' },
      Image: function (this: Record<string, unknown>, url: string) {
        this.url = url
      },
      media: {
        DEFAULT_MEDIA_RECEIVER_APP_ID: 'CC1AD845',
        MediaInfo: function (this: Record<string, unknown>, contentId: string, contentType: string) {
          this.contentId = contentId
          this.contentType = contentType
        },
        GenericMediaMetadata: Ctor,
        LoadRequest: function (this: Record<string, unknown>, media: unknown) {
          this.media = media
        },
        StreamType: { BUFFERED: 'BUFFERED' },
        HlsSegmentFormat: { FMP4: 'fmp4' },
        HlsVideoSegmentFormat: { FMP4: 'fmp4' },
      },
    },
  }
  w.cast = {
    framework: {
      CastContext: { getInstance: () => context },
      CastContextEventType: { CAST_STATE_CHANGED: 'caststatechanged', SESSION_STATE_CHANGED: 'sessionstatechanged' },
      CastState: { NO_DEVICES_AVAILABLE: 'NO_DEVICES_AVAILABLE', CONNECTED: 'CONNECTED' },
      SessionState: { SESSION_STARTED: 'SESSION_STARTED', SESSION_RESUMED: 'SESSION_RESUMED', SESSION_ENDED: 'SESSION_ENDED' },
      RemotePlayer: function () {
        return player
      },
      RemotePlayerController: function () {
        return {
          addEventListener: (_: string, l: Listener) => listeners.push(l),
          removeEventListener() {},
          playOrPause() {
            player.isPaused = !player.isPaused
            notify()
          },
          seek: notify,
          stop() {},
        }
      },
      RemotePlayerEventType: { ANY_CHANGE: 'anyChanged' },
    },
  }
}

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

test('casting loads the movie with absolute LAN URLs and shows a remote control', async ({ page, baseURL }) => {
  const root = mkdtempSync(join(tmpdir(), 'flixnet-e2e-cast-'))
  mkdirSync(join(root, `${MOVIE} (2024)`))
  execFileSync('ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-y',
    '-f', 'lavfi', '-i', 'testsrc2=s=320x180:r=10:d=20',
    '-f', 'lavfi', '-i', 'sine=duration=20',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart',
    join(root, `${MOVIE} (2024)`, `${MOVIE}.2024.mp4`),
  ])
  await page.addInitScript(installCastMock)
  await page.addInitScript(() => localStorage.setItem('flixnet.player.volume', JSON.stringify({ volume: 1, muted: true })))
  await page.goto('/')
  const library = (await api(page, 'POST', '/api/v1/libraries', { name: 'Übertragung', kind: 'movies', paths: [root] })) as { id: number }
  await expect
    .poll(
      async () => {
        const l = (await api(page, 'GET', `/api/v1/libraries/${library.id}`)) as { counts: { movies: number; unprobed: number } }
        return l.counts.movies === 1 && l.counts.unprobed === 0
      },
      { timeout: 60_000 },
    )
    .toBe(true)
  const id = ((await api(page, 'GET', `/api/v1/items?library_id=${library.id}&kind=movie`)) as { items: { id: number }[] }).items[0]!.id

  // Without a LAN address, a page on 127.0.0.1 cannot be cast: the player says so.
  await api(page, 'PUT', '/api/v1/settings/network', { public_url: null })
  await page.goto(`/play/${id}?start=0`)
  await page.getByTestId('cast-button').click()
  await expect(page.getByRole('alert')).toContainText('LAN-Adresse')
  await page.getByRole('button', { name: 'OK' }).click()

  // With the address set in the settings UI, the receiver gets absolute URLs.
  const lan = `http://192.168.50.5:${new URL(baseURL!).port}`
  await page.goto('/settings?tab=network')
  await page.getByLabel('Adresse').fill(lan)
  await page.getByRole('button', { name: 'Speichern' }).first().click()
  await expect(page.getByText('Gespeichert')).toBeVisible()

  await page.goto(`/play/${id}?start=0`)
  await page.getByTestId('cast-button').click()
  await expect(page.getByTestId('cast-device')).toHaveText('Wird auf Wohnzimmer-TV abgespielt')
  const loads = (await page.evaluate(() => (window as unknown as { __castLoads: unknown[] }).__castLoads)) as {
    media: { contentId: string; contentType: string; metadata: { title: string } }
    autoplay: boolean
  }[]
  expect(loads).toHaveLength(1)
  expect(loads[0]!.media.contentId.startsWith(`${lan}/api/v1/stream/`)).toBe(true)
  expect(loads[0]!.media.contentId).toMatch(/[?&]sig=[0-9a-f]{32}/)
  expect(loads[0]!.media.contentType).toBe('video/mp4')
  expect(loads[0]!.media.metadata.title).toBe(MOVIE)
  expect(loads[0]!.autoplay).toBe(true)
  expect(await page.getByTestId('player-video').evaluate((v: HTMLVideoElement) => v.paused)).toBe(true)

  // Remote control.
  await page.getByTestId('cast-play-pause').click()
  await expect(page.getByTestId('cast-play-pause')).toHaveAccessibleName('Abspielen')
  await page.getByRole('button', { name: 'Übertragung beenden' }).click()
  await expect(page.getByTestId('cast-overlay')).toBeHidden()

  await api(page, 'PUT', '/api/v1/settings/network', { public_url: null })
})
