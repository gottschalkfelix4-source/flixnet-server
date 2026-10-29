import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type Playlist = Schemas['PlaylistDto']
export type PlaylistDetail = Schemas['PlaylistDetailDto']
export type SmartFilter = Schemas['SmartFilterDto']

export const playlistsQuery = queryOptions({
  queryKey: ['playlists', 'list'],
  queryFn: async () => unwrap(await api.GET('/api/v1/playlists')),
  staleTime: 30_000,
})

export function usePlaylists(enabled = true) {
  return useQuery({ ...playlistsQuery, enabled })
}

export function playlistQuery(id: number) {
  return queryOptions({
    queryKey: ['playlists', 'detail', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/playlists/{id}', { params: { path: { id } } })),
    staleTime: 10_000,
  })
}

export function usePlaylist(id: number) {
  return useQuery(playlistQuery(id))
}

function useInvalidatePlaylists() {
  const queryClient = useQueryClient()
  return () => queryClient.invalidateQueries({ queryKey: ['playlists'] })
}

export function useCreatePlaylist() {
  const invalidate = useInvalidatePlaylists()
  return useMutation({
    mutationFn: async (body: Schemas['CreatePlaylistRequest']) =>
      unwrap(await api.POST('/api/v1/playlists', { body })),
    onSuccess: invalidate,
  })
}

export function useUpdatePlaylist() {
  const invalidate = useInvalidatePlaylists()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdatePlaylistRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/playlists/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeletePlaylist() {
  const invalidate = useInvalidatePlaylists()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/playlists/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useAddToPlaylist() {
  const invalidate = useInvalidatePlaylists()
  return useMutation({
    mutationFn: async ({ id, itemIds }: { id: number; itemIds: number[] }) =>
      unwrap(
        await api.POST('/api/v1/playlists/{id}/items', {
          params: { path: { id } },
          body: { item_ids: itemIds },
        }),
      ),
    onSuccess: invalidate,
  })
}

export function useRemoveFromPlaylist() {
  const invalidate = useInvalidatePlaylists()
  return useMutation({
    mutationFn: async ({ id, entryId }: { id: number; entryId: number }) =>
      unwrap(
        await api.DELETE('/api/v1/playlists/{id}/items/{entry_id}', {
          params: { path: { id, entry_id: entryId } },
        }),
      ),
    onSuccess: invalidate,
  })
}

/** Reorders optimistically: the list moves at once, the server confirms. */
export function useReorderPlaylist() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ id, entryIds }: { id: number; entryIds: number[] }) =>
      unwrap(
        await api.PUT('/api/v1/playlists/{id}/order', {
          params: { path: { id } },
          body: { entry_ids: entryIds },
        }),
      ),
    onMutate: async ({ id, entryIds }) => {
      const key = playlistQuery(id).queryKey
      await queryClient.cancelQueries({ queryKey: key })
      const previous = queryClient.getQueryData(key)
      if (previous) {
        const byId = new Map(previous.entries.map((e) => [e.entry_id, e]))
        const moved = entryIds.flatMap((e) => byId.get(e) ?? [])
        const rest = previous.entries.filter((e) => e.entry_id == null || !entryIds.includes(e.entry_id))
        queryClient.setQueryData(key, { ...previous, entries: [...moved, ...rest] })
      }
      return { previous, key }
    },
    onError: (_error, _vars, context) => {
      if (context?.previous) queryClient.setQueryData(context.key, context.previous)
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['playlists'] }),
  })
}
