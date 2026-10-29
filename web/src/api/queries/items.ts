import { keepPreviousData, queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type ItemCard = Schemas['ItemCardDto']
export type ItemDetail = Schemas['ItemDetailDto']

/** Query parameters of `GET /items` (all optional). */
export interface ItemsParams {
  library_id?: number
  kind?: string
  parent_id?: number
  series_id?: number
  search?: string
  genre?: string
  year_from?: number
  year_to?: number
  letter?: string
  sort?: 'title' | 'date_added' | 'year' | 'rating' | 'premiere_date' | 'relevance' | 'index'
  order?: 'asc' | 'desc'
  played?: boolean
  favorite?: boolean
  limit?: number
  offset?: number
}

export function itemsQuery(params: ItemsParams) {
  return queryOptions({
    queryKey: ['items', 'list', params],
    queryFn: async () => unwrap(await api.GET('/api/v1/items', { params: { query: params } })),
    staleTime: 30_000,
  })
}

export function useItems(params: ItemsParams, enabled = true) {
  return useQuery({ ...itemsQuery(params), enabled, placeholderData: keepPreviousData })
}

export function itemQuery(id: number) {
  return queryOptions({
    queryKey: ['items', 'detail', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/items/{id}', { params: { path: { id } } })),
    staleTime: 30_000,
  })
}

export function useItem(id: number | null) {
  return useQuery({ ...itemQuery(id ?? 0), enabled: id !== null })
}

export function useLetters(params: ItemsParams) {
  return useQuery({
    queryKey: ['items', 'letters', params],
    queryFn: async () => unwrap(await api.GET('/api/v1/items/letters', { params: { query: params } })),
    staleTime: 60_000,
  })
}

export function useGenres(libraryId?: number) {
  return useQuery({
    queryKey: ['genres', libraryId ?? null],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/genres', { params: { query: { library_id: libraryId } } })),
    staleTime: 5 * 60_000,
  })
}

export function usePerson(id: number) {
  return useQuery({
    queryKey: ['people', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/people/{id}', { params: { path: { id } } })),
  })
}

function useInvalidateItems() {
  const queryClient = useQueryClient()
  return () => queryClient.invalidateQueries({ queryKey: ['items'] })
}

export function useSetPlayed() {
  const invalidate = useInvalidateItems()
  return useMutation({
    mutationFn: async ({ id, played }: { id: number; played: boolean }) =>
      unwrap(
        played
          ? await api.PUT('/api/v1/items/{id}/played', { params: { path: { id } } })
          : await api.DELETE('/api/v1/items/{id}/played', { params: { path: { id } } }),
      ),
    onSuccess: invalidate,
  })
}

export function useSetFavorite() {
  const invalidate = useInvalidateItems()
  return useMutation({
    mutationFn: async ({ id, favorite }: { id: number; favorite: boolean }) =>
      unwrap(
        favorite
          ? await api.PUT('/api/v1/items/{id}/favorite', { params: { path: { id } } })
          : await api.DELETE('/api/v1/items/{id}/favorite', { params: { path: { id } } }),
      ),
    onSuccess: invalidate,
  })
}

export function useRefreshItem() {
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/items/{id}/refresh', { params: { path: { id } } })),
  })
}

export function useIdentifyCandidates(id: number, query: string, enabled: boolean) {
  return useQuery({
    queryKey: ['identify', id, query],
    queryFn: async () =>
      unwrap(
        await api.GET('/api/v1/items/{id}/identify', {
          params: { path: { id }, query: { query: query || undefined } },
        }),
      ),
    enabled,
    retry: false,
  })
}

export function useIdentify() {
  const invalidate = useInvalidateItems()
  return useMutation({
    mutationFn: async ({ id, tmdbId }: { id: number; tmdbId: number }) =>
      unwrap(
        await api.POST('/api/v1/items/{id}/identify', {
          params: { path: { id } },
          body: { tmdb_id: tmdbId },
        }),
      ),
    onSuccess: invalidate,
  })
}
