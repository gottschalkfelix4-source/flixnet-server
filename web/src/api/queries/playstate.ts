import { useQuery } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type ActiveSession = Schemas['ActiveSessionDto']

// Under ['items', …] so invalidating items (watched, favorite, playback stop) refreshes them.

/** "Weiterschauen": movies and episodes with a resume point, newest first. */
export function useResumeItems(limit = 20) {
  return useQuery({
    queryKey: ['items', 'resume', limit],
    queryFn: async () => unwrap(await api.GET('/api/v1/items/resume', { params: { query: { limit } } })),
    staleTime: 10_000,
  })
}

/** "Nächste Folge": the next episode of recently watched series. */
export function useNextUp(limit = 20) {
  return useQuery({
    queryKey: ['items', 'next-up', limit],
    queryFn: async () => unwrap(await api.GET('/api/v1/items/next-up', { params: { query: { limit } } })),
    staleTime: 10_000,
  })
}

/** The episode a series' play button starts (`null`: the series has no episodes). */
export function useSeriesNextUp(seriesId: number, enabled = true) {
  return useQuery({
    queryKey: ['items', 'series-next-up', seriesId],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/items/{id}/next-up', { params: { path: { id: seriesId } } })) ?? null,
    enabled,
    staleTime: 10_000,
  })
}

/** Active playback sessions (administrators), refreshed every few seconds. */
export function useActiveSessions() {
  return useQuery({
    queryKey: ['sessions', 'active'],
    queryFn: async () => unwrap(await api.GET('/api/v1/sessions/active')),
    refetchInterval: 3000,
  })
}
