import { useQuery } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type PlaybackInfo = Schemas['PlaybackInfoDto']

export function usePlaybackInfo(id: number) {
  return useQuery({
    queryKey: ['playback', id],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/items/{id}/playback-info', { params: { path: { id } } })),
    // Signed URLs stay valid for days; no need to refetch while watching.
    staleTime: Infinity,
    retry: false,
  })
}
