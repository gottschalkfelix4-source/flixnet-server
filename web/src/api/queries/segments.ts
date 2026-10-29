import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type MediaSegment = Schemas['MediaSegmentDto']
export type SegmentKind = Schemas['SegmentKind']

/** Intro, credits and recap of an item's files (episode page, admin view). */
export function useMediaSegments(itemId: number, enabled = true) {
  return useQuery({
    queryKey: ['segments', itemId],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/items/{id}/segments', { params: { path: { id: itemId } } })),
    enabled,
  })
}

export function useSetSegment(itemId: number) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ kind, ...body }: Schemas['SetSegmentRequest'] & { kind: SegmentKind }) =>
      unwrap(
        await api.PUT('/api/v1/items/{id}/segments/{kind}', { params: { path: { id: itemId, kind } }, body }),
      ),
    onSuccess: (segments) => queryClient.setQueryData(['segments', itemId], segments),
  })
}

export function useDeleteSegment(itemId: number) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (kind: SegmentKind) =>
      unwrap(await api.DELETE('/api/v1/items/{id}/segments/{kind}', { params: { path: { id: itemId, kind } } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['segments', itemId] }),
  })
}

export function useAnalyzeSegments(itemId: number) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () =>
      unwrap(await api.POST('/api/v1/items/{id}/segments/analyze', { params: { path: { id: itemId } } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['jobs'] }),
  })
}

// ---- Personal playback preferences ------------------------------------------

const preferencesKey = ['users', 'me', 'preferences']

export function usePreferences() {
  return useQuery({
    queryKey: preferencesKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/users/me/preferences')),
    staleTime: 60_000,
  })
}

export function useUpdatePreferences() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdatePreferencesRequest']) =>
      unwrap(await api.PATCH('/api/v1/users/me/preferences', { body })),
    onSuccess: (preferences) => queryClient.setQueryData(preferencesKey, preferences),
  })
}
