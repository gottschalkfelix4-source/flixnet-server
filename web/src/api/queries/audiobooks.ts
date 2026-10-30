import { useQuery } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type AudiobookCard = Schemas['AudiobookCardDto']
export type Audiobook = Schemas['AudiobookDetailDto']

export function useAudiobooks(search?: string) {
  return useQuery({
    queryKey: ['audiobooks', 'list', search ?? ''],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/audiobooks', { params: { query: { search: search || undefined, limit: 200 } } })),
  })
}

export function useAudiobook(id: number) {
  return useQuery({
    queryKey: ['audiobooks', 'detail', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/audiobooks/{id}', { params: { path: { id } } })),
  })
}

/** Stores the listening position; `keepalive` lets it finish while the page closes. */
export function saveAudiobookProgress(id: number, positionMs: number, token: string | undefined, keepalive = false) {
  if (!token) return Promise.resolve(null)
  return fetch(`/api/v1/audiobooks/${id}/progress`, {
    method: 'PUT',
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ position_ms: Math.max(0, Math.round(positionMs)) }),
    keepalive,
  }).catch(() => null)
}
