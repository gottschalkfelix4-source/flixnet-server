import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type Collection = Schemas['CollectionDto']
export type CollectionDetail = Schemas['CollectionDetailDto']

export const collectionsQuery = queryOptions({
  queryKey: ['collections', 'list'],
  queryFn: async () => unwrap(await api.GET('/api/v1/collections')),
  staleTime: 30_000,
})

export function useCollections() {
  return useQuery(collectionsQuery)
}

export function collectionQuery(id: number) {
  return queryOptions({
    queryKey: ['collections', 'detail', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/collections/{id}', { params: { path: { id } } })),
    staleTime: 30_000,
  })
}

export function useCollection(id: number) {
  return useQuery(collectionQuery(id))
}

/** Collections a movie or series belongs to ("Teil der Sammlung …"). */
export function useItemCollections(itemId: number, enabled = true) {
  return useQuery({
    queryKey: ['collections', 'of-item', itemId],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/items/{id}/collections', { params: { path: { id: itemId } } })),
    enabled,
    staleTime: 30_000,
  })
}

function useInvalidateCollections() {
  const queryClient = useQueryClient()
  return () => queryClient.invalidateQueries({ queryKey: ['collections'] })
}

export function useCreateCollection() {
  const invalidate = useInvalidateCollections()
  return useMutation({
    mutationFn: async (body: Schemas['CreateCollectionRequest']) =>
      unwrap(await api.POST('/api/v1/collections', { body })),
    onSuccess: invalidate,
  })
}

export function useUpdateCollection() {
  const invalidate = useInvalidateCollections()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateCollectionRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/collections/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeleteCollection() {
  const invalidate = useInvalidateCollections()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/collections/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useAddToCollection() {
  const invalidate = useInvalidateCollections()
  return useMutation({
    mutationFn: async ({ id, itemIds }: { id: number; itemIds: number[] }) =>
      unwrap(
        await api.POST('/api/v1/collections/{id}/items', {
          params: { path: { id } },
          body: { item_ids: itemIds },
        }),
      ),
    onSuccess: invalidate,
  })
}

export function useRemoveFromCollection() {
  const invalidate = useInvalidateCollections()
  return useMutation({
    mutationFn: async ({ id, itemId }: { id: number; itemId: number }) =>
      unwrap(
        await api.DELETE('/api/v1/collections/{id}/items/{item_id}', {
          params: { path: { id, item_id: itemId } },
        }),
      ),
    onSuccess: invalidate,
  })
}
