import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type Recommendation = Schemas['RecommendedItemDto']
export type GroupSuggestion = Schemas['GroupSuggestionDto']
export type AiStatus = Schemas['AiStatusDto']

// Under ['items'] so marking something as watched refreshes the recommendations too.
const recommendationsKey = ['items', 'recommendations']

export function useRecommendations(limit = 40) {
  return useQuery({
    queryKey: [...recommendationsKey, 'list', limit],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/recommendations', { params: { query: { limit } } })),
    staleTime: 60_000,
  })
}

export function useRecommendationRows() {
  return useQuery({
    queryKey: [...recommendationsKey, 'rows'],
    queryFn: async () => unwrap(await api.GET('/api/v1/recommendations/rows')),
    staleTime: 60_000,
  })
}

export function useOnboarding(enabled: boolean) {
  return useQuery({
    queryKey: [...recommendationsKey, 'onboarding'],
    queryFn: async () => unwrap(await api.GET('/api/v1/recommendations/onboarding')),
    enabled,
  })
}

export function useSaveOnboarding() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (itemIds: number[]) =>
      unwrap(await api.PUT('/api/v1/recommendations/onboarding', { body: { item_ids: itemIds } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: recommendationsKey }),
  })
}

export function useGroupRecommendations() {
  return useMutation({
    mutationFn: async (body: Schemas['GroupRequest']) =>
      unwrap(await api.POST('/api/v1/recommendations/group', { body })),
  })
}

export function useSimilarItems(id: number) {
  return useQuery({
    queryKey: ['items', 'similar', id],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/items/{id}/similar', { params: { path: { id }, query: { limit: 20 } } })),
    staleTime: 5 * 60_000,
  })
}

const settingsKey = ['settings', 'ai']
const statusKey = ['ai', 'status']

export function useAiSettings() {
  return useQuery({
    queryKey: settingsKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/ai')),
  })
}

export function useUpdateAiSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateAiSettings']) => unwrap(await api.PUT('/api/v1/settings/ai', { body })),
    onSuccess: (data) => {
      queryClient.setQueryData(settingsKey, data)
      void queryClient.invalidateQueries({ queryKey: statusKey })
    },
  })
}

/** Polls while a download or embedding run is active. */
export function useAiStatus() {
  return useQuery({
    queryKey: statusKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/ai/status')),
    refetchInterval: (query) => (query.state.data?.download || query.state.data?.embedding ? 2000 : 15_000),
  })
}

function useStatusMutation<T>(fn: () => Promise<T>) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: fn,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: statusKey }),
  })
}

export function useDownloadAiModel() {
  return useStatusMutation(async () => unwrap(await api.POST('/api/v1/ai/model')))
}

export function useDeleteAiModel() {
  return useStatusMutation(async () => unwrap(await api.DELETE('/api/v1/ai/model')))
}

export function useStartEmbedding() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (rebuild: boolean) => unwrap(await api.POST('/api/v1/ai/embeddings', { body: { rebuild } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: statusKey }),
  })
}
