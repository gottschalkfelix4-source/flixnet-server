import { type Track, type TrackSource, fetchSources, scrobble } from '@/api/queries/music'
import { readStored, writeStored } from '@/lib/storage'

/**
 * The music player: one instance for the whole app, so playback continues while the
 * user browses. Gapless: two `<audio>` elements take turns — while one plays, the next
 * track is already loaded and buffered in the other, and starts the moment the first
 * one ends. Both run through Web Audio gain nodes for ReplayGain (track or album) and
 * the volume. The queue lives in this browser (localStorage), per device.
 */

export type RepeatMode = 'off' | 'all' | 'one'
export type ReplayGainMode = 'off' | 'track' | 'album'

/** What the queue keeps of a track (enough to show it without asking the server). */
export interface QueueTrack {
  id: number
  title: string
  artist: string
  album_id: number | null
  album_title: string | null
  album_artist_id: number | null
  duration_ms: number | null
  cover_url: string | null
  has_lyrics: boolean
}

export interface PlayerState {
  queue: QueueTrack[]
  /** Index into `queue` of the current track. */
  index: number
  playing: boolean
  /** Waiting for data (loading, buffering, converting). */
  loading: boolean
  /** Seconds. */
  currentTime: number
  duration: number
  volume: number
  muted: boolean
  shuffle: boolean
  repeat: RepeatMode
  replayGain: ReplayGainMode
  error: string | null
}

const QUEUE_KEY = 'flixnet.music.queue'
const PREFS_KEY = 'flixnet.music.prefs'

interface StoredQueue {
  queue: QueueTrack[]
  index: number
  position: number
  shuffle: boolean
  original: QueueTrack[] | null
}

interface StoredPrefs {
  volume: number
  muted: boolean
  repeat: RepeatMode
  replayGain: ReplayGainMode
}

export function toQueueTrack(t: Track): QueueTrack {
  return {
    id: t.id,
    title: t.title,
    artist: t.artist,
    album_id: t.album_id ?? null,
    album_title: t.album_title ?? null,
    album_artist_id: t.album_artist_id ?? null,
    duration_ms: t.duration_ms ?? null,
    cover_url: t.cover_url ?? null,
    has_lyrics: t.has_lyrics,
  }
}

function shuffled<T>(items: T[]): T[] {
  const out = [...items]
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1))
    const swap = out[i] as T
    out[i] = out[j] as T
    out[j] = swap
  }
  return out
}

/** Linear gain for a ReplayGain value, limited so the peak does not clip. */
export function replayGainFactor(source: TrackSource | undefined, mode: ReplayGainMode): number {
  if (!source || mode === 'off') return 1
  const rg = source.replaygain
  const db = mode === 'album' ? (rg.album_gain_db ?? rg.track_gain_db) : (rg.track_gain_db ?? rg.album_gain_db)
  if (db == null) return 1
  const peak = mode === 'album' ? (rg.album_peak ?? rg.track_peak) : (rg.track_peak ?? rg.album_peak)
  const gain = 10 ** (db / 20)
  return peak && peak > 0 ? Math.min(gain, 1 / peak) : gain
}

interface Deck {
  audio: HTMLAudioElement
  gain: GainNode | null
  /** Queue index loaded into this element (-1 = none). */
  index: number
  trackId: number | null
}

class MusicPlayer {
  private state: PlayerState
  private listeners = new Set<() => void>()
  private decks: [Deck, Deck] | null = null
  private active: 0 | 1 = 0
  private get idleIndex(): 0 | 1 {
    return this.active === 0 ? 1 : 0
  }
  private context: AudioContext | null = null
  private master: GainNode | null = null
  private sources = new Map<number, TrackSource>()
  /** Queue before shuffling, to restore the order. */
  private original: QueueTrack[] | null = null
  /** Position to continue from after a reload (seconds). */
  private resumeAt = 0
  /** Listening time of the current track (ms, pauses and seeks excluded). */
  private listened = 0
  private lastTime = 0
  private lastSave = 0
  private failures = 0

  constructor() {
    const stored = readStored<StoredQueue | null>(QUEUE_KEY, null)
    const prefs = readStored<Partial<StoredPrefs>>(PREFS_KEY, {})
    const queue = Array.isArray(stored?.queue) ? stored.queue : []
    const index = stored && stored.index >= 0 && stored.index < queue.length ? stored.index : 0
    this.original = stored?.original ?? null
    this.resumeAt = stored?.position ?? 0
    this.state = {
      queue,
      index,
      playing: false,
      loading: false,
      currentTime: this.resumeAt,
      duration: (queue[index]?.duration_ms ?? 0) / 1000,
      volume: typeof prefs.volume === 'number' ? Math.min(1, Math.max(0, prefs.volume)) : 1,
      muted: prefs.muted ?? false,
      shuffle: stored?.shuffle ?? false,
      repeat: prefs.repeat ?? 'off',
      replayGain: prefs.replayGain ?? 'track',
      error: null,
    }
    window.addEventListener('pagehide', () => this.saveQueue(true))
    this.setupMediaSession()
  }

  // ---- Store ------------------------------------------------------------------------

  subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  getState = () => this.state

  private set(patch: Partial<PlayerState>) {
    this.state = { ...this.state, ...patch }
    for (const listener of this.listeners) listener()
  }

  private saveQueue(force = false) {
    const now = Date.now()
    if (!force && now - this.lastSave < 5000) return
    this.lastSave = now
    writeStored(QUEUE_KEY, {
      queue: this.state.queue,
      index: this.state.index,
      position: this.state.currentTime,
      shuffle: this.state.shuffle,
      original: this.original,
    } satisfies StoredQueue)
  }

  private savePrefs() {
    const { volume, muted, repeat, replayGain } = this.state
    writeStored(PREFS_KEY, { volume, muted, repeat, replayGain } satisfies StoredPrefs)
  }

  get current(): QueueTrack | null {
    return this.state.queue[this.state.index] ?? null
  }

  // ---- Audio graph ------------------------------------------------------------------

  private ensureDecks(): [Deck, Deck] {
    if (this.decks) return this.decks
    const make = (n: number): Deck => {
      const audio = document.createElement('audio')
      audio.preload = 'auto'
      audio.dataset.flixnetAudio = String(n)
      audio.hidden = true
      document.body.appendChild(audio)
      audio.addEventListener('ended', () => this.onEnded(n))
      audio.addEventListener('timeupdate', () => this.onTime(n))
      audio.addEventListener('playing', () => n === this.active && this.set({ loading: false, playing: true, error: null }))
      audio.addEventListener('waiting', () => n === this.active && this.set({ loading: true }))
      audio.addEventListener('pause', () => n === this.active && !audio.ended && this.set({ playing: false }))
      audio.addEventListener('durationchange', () => {
        if (n === this.active && Number.isFinite(audio.duration)) this.set({ duration: audio.duration })
      })
      audio.addEventListener('error', () => n === this.active && this.onError())
      return { audio, gain: null, index: -1, trackId: null }
    }
    this.decks = [make(0), make(1)]
    return this.decks
  }

  /** Web Audio for ReplayGain; created on the first play (browsers require a gesture). */
  private ensureContext() {
    if (this.context || typeof AudioContext === 'undefined') return
    try {
      const context = new AudioContext()
      const master = context.createGain()
      master.connect(context.destination)
      for (const deck of this.ensureDecks()) {
        const source = context.createMediaElementSource(deck.audio)
        deck.gain = context.createGain()
        source.connect(deck.gain).connect(master)
      }
      this.context = context
      this.master = master
      this.applyVolume()
    } catch {
      // Without Web Audio: plain elements, no ReplayGain.
      this.context = null
    }
  }

  private applyVolume() {
    const volume = this.state.muted ? 0 : this.state.volume
    if (this.master) {
      this.master.gain.value = volume
      for (const deck of this.decks ?? []) deck.audio.volume = 1
    } else {
      for (const deck of this.decks ?? []) deck.audio.volume = volume
    }
  }

  private applyGain(deck: Deck) {
    if (!deck.gain || deck.trackId === null) return
    // Album gain only makes sense while the album plays in order.
    const mode = this.state.replayGain === 'album' && this.state.shuffle ? 'track' : this.state.replayGain
    deck.gain.gain.value = replayGainFactor(this.sources.get(deck.trackId), mode)
  }

  private async sourcesFor(ids: number[]): Promise<void> {
    const missing = ids.filter((id) => !this.sources.has(id))
    if (missing.length === 0) return
    for (const source of await fetchSources(missing)) this.sources.set(source.track_id, source)
  }

  /** Loads queue entry `index` into a deck (no play). Returns false if not playable. */
  private async load(deck: Deck, index: number): Promise<boolean> {
    const track = this.state.queue[index]
    if (!track) return false
    if (deck.trackId === track.id && deck.index === index) return true
    // Also fetch the following entry, so preloading it later needs no round trip.
    const after = this.state.queue[index + 1]
    await this.sourcesFor(after ? [track.id, after.id] : [track.id])
    const source = this.sources.get(track.id)
    if (!source) return false
    deck.index = index
    deck.trackId = track.id
    deck.audio.dataset.trackId = String(track.id)
    deck.audio.src = source.url
    deck.audio.load()
    this.applyGain(deck)
    return true
  }

  private nextIndex(from = this.state.index): number | null {
    const { queue, repeat } = this.state
    if (repeat === 'one') return from
    if (from + 1 < queue.length) return from + 1
    return repeat === 'all' && queue.length > 0 ? 0 : null
  }

  /** Buffers the next track in the idle deck. */
  private preloadNext() {
    const next = this.nextIndex()
    if (next === null || next === this.state.index) return
    const decks = this.ensureDecks()
    const idle = decks[this.idleIndex]
    void this.load(idle, next).catch(() => undefined)
  }

  // ---- Events -------------------------------------------------------------------------

  private onTime(n: number) {
    if (n !== this.active) return
    const audio = this.ensureDecks()[n].audio
    const time = audio.currentTime
    const delta = time - this.lastTime
    // Count only continuous playback (seeks jump, pauses stand still).
    if (!audio.paused && delta > 0 && delta < 2) this.listened += delta * 1000
    this.lastTime = time
    this.set({ currentTime: time })
    this.saveQueue()
    this.updatePositionState()
  }

  private onEnded(n: number) {
    if (n !== this.active) return
    const finished = this.current
    this.report(finished)
    const next = this.nextIndex()
    if (next === null) {
      this.set({ playing: false, currentTime: 0 })
      this.ensureDecks()[n].audio.currentTime = 0
      this.saveQueue(true)
      return
    }
    if (next === this.state.index) {
      // Repeat one.
      const audio = this.ensureDecks()[n].audio
      audio.currentTime = 0
      void audio.play()
      return
    }
    void this.switchTo(next, 0)
  }

  private onError() {
    this.failures += 1
    const track = this.current
    this.set({ loading: false, error: `„${track?.title ?? 'Titel'}“ lässt sich nicht abspielen.` })
    const next = this.nextIndex()
    // Skip broken tracks, but do not spin through a queue of nothing but errors.
    if (next !== null && next !== this.state.index && this.failures < Math.min(5, this.state.queue.length)) {
      void this.switchTo(next, 0)
    } else {
      this.set({ playing: false })
    }
  }

  /** Reports the listening time of a track that stops being the current one. */
  private report(track: QueueTrack | null) {
    const listened = this.listened
    this.listened = 0
    this.lastTime = 0
    if (track && listened > 1000) void scrobble(track.id, listened).catch(() => undefined)
  }

  /** Makes `index` current and plays it from `position` seconds (gapless if preloaded). */
  private async switchTo(index: number, position: number) {
    const decks = this.ensureDecks()
    const idle = decks[this.idleIndex]
    const current = decks[this.active]
    this.set({ index, currentTime: position, duration: (this.state.queue[index]?.duration_ms ?? 0) / 1000, error: null })
    this.saveQueue(true)
    this.updateMetadata()
    let deck: Deck
    if (idle.index === index && idle.trackId === this.state.queue[index]?.id) {
      deck = idle
      this.active = this.idleIndex
      current.audio.pause()
    } else {
      current.audio.pause()
      deck = current
      this.set({ loading: true })
      deck.index = -1
      if (!(await this.load(deck, index))) {
        this.onError()
        return
      }
    }
    this.lastTime = position
    if (position > 0) deck.audio.currentTime = position
    else if (deck.audio.currentTime > 0) deck.audio.currentTime = 0
    this.applyGain(deck)
    try {
      await deck.audio.play()
      this.failures = 0
      this.set({ playing: true })
    } catch (error) {
      if ((error as DOMException)?.name !== 'AbortError') this.set({ playing: false, loading: false })
      return
    }
    this.preloadNext()
  }

  // ---- Commands -----------------------------------------------------------------------

  /** Replaces the queue and plays entry `start`. */
  playTracks(tracks: Track[] | QueueTrack[], start = 0, options: { shuffle?: boolean } = {}) {
    if (tracks.length === 0) return
    this.report(this.current)
    let queue = tracks.map((t) => ('library_id' in t ? toQueueTrack(t as Track) : (t as QueueTrack)))
    let index = Math.min(Math.max(0, start), queue.length - 1)
    const shuffle = options.shuffle ?? this.state.shuffle
    this.original = null
    if (shuffle) {
      this.original = queue
      const first = (options.shuffle ? queue[Math.floor(Math.random() * queue.length)] : queue[index]) as QueueTrack
      queue = [first, ...shuffled(queue.filter((t) => t !== first))]
      index = 0
    }
    this.invalidateDecks()
    this.set({ queue, index, shuffle })
    this.ensureContext()
    void this.context?.resume()
    void this.switchTo(index, 0)
  }

  /** Adds tracks after the current one ("Als Nächstes") or at the end. */
  enqueue(tracks: Track[], next = false) {
    const items = tracks.map(toQueueTrack)
    if (this.state.queue.length === 0) {
      this.playTracks(tracks)
      return
    }
    const queue = [...this.state.queue]
    queue.splice(next ? this.state.index + 1 : queue.length, 0, ...items)
    if (this.original) this.original = [...this.original, ...items]
    this.set({ queue })
    this.invalidateIdle()
    this.saveQueue(true)
    this.preloadNext()
  }

  jumpTo(index: number) {
    if (index < 0 || index >= this.state.queue.length) return
    this.report(this.current)
    this.ensureContext()
    void this.context?.resume()
    void this.switchTo(index, 0)
  }

  removeAt(index: number) {
    if (index === this.state.index || index < 0 || index >= this.state.queue.length) return
    const queue = this.state.queue.filter((_, i) => i !== index)
    const removed = this.state.queue[index]
    if (this.original) this.original = this.original.filter((t) => t !== removed)
    this.invalidateDecks(true)
    this.set({ queue, index: index < this.state.index ? this.state.index - 1 : this.state.index })
    this.saveQueue(true)
    this.preloadNext()
  }

  clear() {
    this.report(this.current)
    for (const deck of this.decks ?? []) {
      deck.audio.pause()
      deck.audio.removeAttribute('src')
      deck.audio.load()
      deck.index = -1
      deck.trackId = null
    }
    this.original = null
    this.set({ queue: [], index: 0, playing: false, currentTime: 0, duration: 0, error: null })
    this.saveQueue(true)
    if ('mediaSession' in navigator) navigator.mediaSession.metadata = null
  }

  toggle() {
    if (this.state.playing) this.pause()
    else this.play()
  }

  play() {
    if (!this.current) return
    this.ensureContext()
    void this.context?.resume()
    const deck = this.ensureDecks()[this.active]
    if (deck.trackId === this.current.id && deck.index === this.state.index) {
      void deck.audio.play().then(() => this.preloadNext(), () => this.set({ playing: false }))
    } else {
      // After a reload: load the remembered track and continue where it stopped.
      const position = this.resumeAt
      this.resumeAt = 0
      void this.switchTo(this.state.index, position)
    }
  }

  pause() {
    this.decks?.[this.active].audio.pause()
    this.set({ playing: false })
    this.saveQueue(true)
  }

  next() {
    const next = this.nextIndex()
    const index = this.state.repeat === 'one' ? this.state.index + 1 : next
    if (index === null || index >= this.state.queue.length) {
      if (this.state.repeat !== 'off' && this.state.queue.length > 0) this.jumpTo(0)
      return
    }
    this.jumpTo(index)
  }

  previous() {
    const audio = this.decks?.[this.active].audio
    if (audio && audio.currentTime > 3) {
      this.seek(0)
      return
    }
    if (this.state.index > 0) this.jumpTo(this.state.index - 1)
    else this.seek(0)
  }

  seek(seconds: number) {
    const deck = this.decks?.[this.active]
    const target = Math.max(0, Math.min(seconds, this.state.duration || seconds))
    if (deck && deck.trackId === this.current?.id) {
      deck.audio.currentTime = target
      this.lastTime = target
    } else {
      this.resumeAt = target
    }
    this.set({ currentTime: target })
  }

  setVolume(volume: number) {
    this.set({ volume: Math.min(1, Math.max(0, volume)), muted: false })
    this.applyVolume()
    this.savePrefs()
  }

  toggleMute() {
    this.set({ muted: !this.state.muted })
    this.applyVolume()
    this.savePrefs()
  }

  setRepeat(repeat: RepeatMode) {
    this.set({ repeat })
    this.savePrefs()
    this.invalidateIdle()
    this.preloadNext()
  }

  cycleRepeat() {
    this.setRepeat(this.state.repeat === 'off' ? 'all' : this.state.repeat === 'all' ? 'one' : 'off')
  }

  setReplayGain(replayGain: ReplayGainMode) {
    this.set({ replayGain })
    this.savePrefs()
    for (const deck of this.decks ?? []) this.applyGain(deck)
  }

  toggleShuffle() {
    const current = this.current
    if (!current) {
      this.set({ shuffle: !this.state.shuffle })
      return
    }
    if (this.state.shuffle) {
      const queue = this.original ?? this.state.queue
      this.original = null
      this.set({ shuffle: false, queue, index: Math.max(0, queue.indexOf(current)) })
    } else {
      this.original = this.state.queue
      const rest = shuffled(this.state.queue.filter((_, i) => i !== this.state.index))
      this.set({ shuffle: true, queue: [current, ...rest], index: 0 })
    }
    // The active deck still plays `current`; renumber it, drop the preloaded one.
    const deck = this.decks?.[this.active]
    if (deck) deck.index = this.state.index
    this.invalidateIdle()
    this.saveQueue(true)
    for (const d of this.decks ?? []) this.applyGain(d)
    this.preloadNext()
  }

  /** The queue changed under the decks: their indices no longer hold. */
  private invalidateDecks(keepActive = false) {
    for (const [n, deck] of (this.decks ?? []).entries()) {
      if (keepActive && n === this.active) {
        deck.index = this.state.queue.findIndex((t) => t.id === deck.trackId)
        continue
      }
      deck.index = -1
    }
  }

  private invalidateIdle() {
    const idle = this.decks?.[this.idleIndex]
    if (idle) idle.index = -1
  }

  // ---- Media Session (lock screen, hardware keys) ---------------------------------

  private setupMediaSession() {
    if (!('mediaSession' in navigator)) return
    const session = navigator.mediaSession
    const handlers: [MediaSessionAction, MediaSessionActionHandler][] = [
      ['play', () => this.play()],
      ['pause', () => this.pause()],
      ['stop', () => this.pause()],
      ['previoustrack', () => this.previous()],
      ['nexttrack', () => this.next()],
      ['seekbackward', (d) => this.seek(this.state.currentTime - (d.seekOffset ?? 10))],
      ['seekforward', (d) => this.seek(this.state.currentTime + (d.seekOffset ?? 10))],
      ['seekto', (d) => d.seekTime != null && this.seek(d.seekTime)],
    ]
    for (const [action, handler] of handlers) {
      try {
        session.setActionHandler(action, handler)
      } catch {
        // Action not supported by this browser.
      }
    }
  }

  private updateMetadata() {
    if (!('mediaSession' in navigator) || typeof MediaMetadata === 'undefined') return
    const track = this.current
    if (!track) return
    const artwork = track.cover_url
      ? [{ src: new URL(track.cover_url, window.location.origin).href, sizes: '342x342', type: 'image/jpeg' }]
      : []
    navigator.mediaSession.metadata = new MediaMetadata({
      title: track.title,
      artist: track.artist,
      album: track.album_title ?? '',
      artwork,
    })
  }

  private updatePositionState() {
    if (!('mediaSession' in navigator) || !navigator.mediaSession.setPositionState) return
    const { duration, currentTime } = this.state
    if (!duration || !Number.isFinite(duration)) return
    try {
      navigator.mediaSession.setPositionState({
        duration,
        position: Math.min(currentTime, duration),
        playbackRate: 1,
      })
    } catch {
      // Inconsistent values during track changes.
    }
  }
}

export const musicPlayer = new MusicPlayer()
