import { keepPreviousData, queryOptions, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'
import type { ItemsParams } from './items'
import { useLibraries } from './libraries'

export type Track = Schemas['TrackDto']
export type AlbumDetail = Schemas['AlbumDetailDto']
export type ArtistDetail = Schemas['ArtistDetailDto']
export type TrackSource = Schemas['TrackSourceDto']
export type Lyrics = Schemas['LyricsDto']

/** Query parameters of `GET /music/tracks`. */
export interface TracksParams {
  library_id?: number
  album_id?: number
  artist_id?: number
  search?: string
  genre?: string
  favorite?: boolean
  sort?: 'index' | 'title' | 'date_added' | 'year' | 'relevance'
  order?: 'asc' | 'desc'
  limit?: number
  offset?: number
}

/** Libraries of kind `music` the user may see. */
export function useMusicLibraries() {
  const libraries = useLibraries()
  return { ...libraries, data: libraries.data?.filter((l) => l.kind === 'music') }
}

export function albumQuery(id: number) {
  return queryOptions({
    queryKey: ['music', 'album', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/music/albums/{id}', { params: { path: { id } } })),
    staleTime: 30_000,
  })
}

export function useAlbum(id: number) {
  return useQuery(albumQuery(id))
}

export function useArtist(id: number) {
  return useQuery({
    queryKey: ['music', 'artist', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/music/artists/{id}', { params: { path: { id } } })),
    staleTime: 30_000,
  })
}

export function tracksQuery(params: TracksParams) {
  return queryOptions({
    queryKey: ['music', 'tracks', params],
    queryFn: async () => unwrap(await api.GET('/api/v1/music/tracks', { params: { query: params } })),
    staleTime: 30_000,
  })
}

export function useTracks(params: TracksParams, enabled = true) {
  return useQuery({ ...tracksQuery(params), enabled, placeholderData: keepPreviousData })
}

/** Grids of artists/albums through `GET /items`, loaded page by page. */
export function useItemPages(params: ItemsParams, enabled = true) {
  const limit = params.limit ?? 60
  return useInfiniteQuery({
    queryKey: ['items', 'pages', params],
    initialPageParam: 0,
    enabled,
    queryFn: async ({ pageParam }) =>
      unwrap(await api.GET('/api/v1/items', { params: { query: { ...params, limit, offset: pageParam } } })),
    getNextPageParam: (last) => (last.offset + last.items.length < last.total ? last.offset + last.items.length : undefined),
  })
}

export function useLyrics(trackId: number | null) {
  return useQuery({
    queryKey: ['music', 'lyrics', trackId],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/music/tracks/{id}/lyrics', { params: { path: { id: trackId ?? 0 } } })),
    enabled: trackId !== null,
    retry: false,
    staleTime: 10 * 60_000,
  })
}

/** Playback sources (signed URLs, Direct Play or AAC) for tracks, in request order. */
export async function fetchSources(trackIds: number[]): Promise<TrackSource[]> {
  if (trackIds.length === 0) return []
  return unwrap(
    await api.POST('/api/v1/music/playback', {
      body: { track_ids: trackIds, profile: audioProfile() },
    }),
  )
}

/** All tracks on an artist's albums, in album order (for "Abspielen" on the artist page). */
export async function fetchArtistTracks(artistId: number): Promise<Track[]> {
  const page = unwrap(
    await api.GET('/api/v1/music/tracks', { params: { query: { artist_id: artistId, limit: 500 } } }),
  )
  // Album by album (oldest first), then disc and track.
  return [...page.items].sort(
    (a, b) =>
      (a.year ?? 0) - (b.year ?? 0) ||
      (a.album_title ?? '').localeCompare(b.album_title ?? '') ||
      (a.disc_number ?? 1) - (b.disc_number ?? 1) ||
      (a.track_number ?? 0) - (b.track_number ?? 0),
  )
}

/** Reports a listen (counts as a play from half the track or four minutes on). */
export async function scrobble(trackId: number, listenedMs: number): Promise<void> {
  await api.POST('/api/v1/music/tracks/{id}/scrobble', {
    params: { path: { id: trackId } },
    body: { listened_ms: Math.round(listenedMs) },
  })
}

export function useSetTrackFavorite() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ id, favorite }: { id: number; favorite: boolean }) =>
      unwrap(
        favorite
          ? await api.PUT('/api/v1/items/{id}/favorite', { params: { path: { id } } })
          : await api.DELETE('/api/v1/items/{id}/favorite', { params: { path: { id } } }),
      ),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['music'] }),
  })
}

/** Probe strings per audio container/codec for `<audio>` (Direct Play of files). */
const AUDIO_FILE_PROBES: { container: string; codec: string; types: string[] }[] = [
  { container: 'flac', codec: 'flac', types: ['audio/flac', 'audio/x-flac'] },
  { container: 'mp3', codec: 'mp3', types: ['audio/mpeg'] },
  { container: 'm4a', codec: 'aac', types: ['audio/mp4; codecs="mp4a.40.2"', 'audio/aac'] },
  { container: 'm4a', codec: 'alac', types: ['audio/mp4; codecs="alac"'] },
  { container: 'aac', codec: 'aac', types: ['audio/aac'] },
  { container: 'ogg', codec: 'vorbis', types: ['audio/ogg; codecs="vorbis"'] },
  { container: 'oga', codec: 'vorbis', types: ['audio/ogg; codecs="vorbis"'] },
  { container: 'opus', codec: 'opus', types: ['audio/ogg; codecs="opus"'] },
  { container: 'ogg', codec: 'opus', types: ['audio/ogg; codecs="opus"'] },
  { container: 'wav', codec: 'pcm_s16le', types: ['audio/wav; codecs="1"', 'audio/wav'] },
  { container: 'wav', codec: 'pcm_s24le', types: ['audio/wav; codecs="1"', 'audio/wav'] },
]

let cachedProfile: Schemas['DeviceProfile'] | null = null

/** What this browser's `<audio>` plays as a file (formats it cannot play are converted to AAC). */
export function audioProfile(): Schemas['DeviceProfile'] {
  if (cachedProfile) return cachedProfile
  const audio = document.createElement('audio')
  const ok = (type: string) => audio.canPlayType(type) !== ''
  const playable = AUDIO_FILE_PROBES.filter((p) => p.types.some(ok))
  cachedProfile = {
    containers: [...new Set(playable.map((p) => p.container))],
    // `m4b` audiobooks and `mp4a` share the MP4 container.
    audio_codecs: [...new Set(playable.map((p) => p.codec))].map((codec) => ({ codec, max_channels: 8 })),
    video_codecs: [],
    multiple_audio_tracks: false,

  }
  return cachedProfile
}
