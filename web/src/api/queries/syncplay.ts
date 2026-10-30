import { useMutation, useQuery } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type SyncPlayGroup = Schemas['SyncPlayGroupDto']

/** Open groups of the household (refreshed while the panel is open). */
export function useSyncPlayGroups(enabled: boolean) {
  return useQuery({
    queryKey: ['syncplay', 'groups'],
    queryFn: async () => unwrap(await api.GET('/api/v1/syncplay/groups')),
    enabled,
    refetchInterval: enabled ? 5000 : false,
  })
}

export function useSyncPlayGroup(id: string | null) {
  return useQuery({
    queryKey: ['syncplay', 'group', id],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/syncplay/groups/{id}', { params: { path: { id: id ?? '' } } })),
    enabled: !!id,
    retry: false,
  })
}

export function useCreateSyncPlayGroup() {
  return useMutation({
    mutationFn: async (body: Schemas['CreateSyncPlayGroup']) =>
      unwrap(await api.POST('/api/v1/syncplay/groups', { body })),
  })
}

export async function joinSyncPlayGroup(code: string) {
  return unwrap(await api.POST('/api/v1/syncplay/join/{code}', { params: { path: { code } } }))
}
