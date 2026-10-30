import { useQuery } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type Trickplay = Schemas['TrickplayDto']
export type Chapter = Schemas['PlaybackChapterDto']
export type NextEpisode = Schemas['ItemCardDto']

/**
 * Seek-bar previews of a media file; `null` while none exist (they are made in the
 * background after a scan), so the player simply shows no picture.
 */
export function useTrickplay(fileId: number | undefined) {
  return useQuery({
    queryKey: ['trickplay', fileId],
    queryFn: async () => {
      const result = await api.GET('/api/v1/files/{file_id}/trickplay', { params: { path: { file_id: fileId! } } })
      return result.response.status === 404 ? null : unwrap(result)
    },
    enabled: fileId !== undefined,
    staleTime: 5 * 60_000,
    // Made in the background: look again now and then until they exist.
    refetchInterval: (query) => (query.state.data === null ? 30_000 : false),
    retry: false,
  })
}

/** Chapters of a media file (empty without chapters). */
export function useChapters(fileId: number | undefined) {
  return useQuery({
    queryKey: ['chapters', fileId],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/files/{file_id}/chapters', { params: { path: { file_id: fileId! } } })),
    enabled: fileId !== undefined,
    staleTime: 5 * 60_000,
    retry: false,
  })
}

/** The episode after this one (`null`: the last one, or not an episode). */
export function useNextEpisode(itemId: number, enabled: boolean) {
  return useQuery({
    queryKey: ['next-episode', itemId],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/items/{id}/next-episode', { params: { path: { id: itemId } } })) ?? null,
    enabled,
    staleTime: 60_000,
    retry: false,
  })
}
