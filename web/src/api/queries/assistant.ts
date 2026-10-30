import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type SceneHit = Schemas['SceneHitDto']
export type NaturalSearch = Schemas['NaturalSearchDto']
export type ChatResponse = Schemas['ChatResponseDto']
export type ChatAction = Schemas['ChatActionDto']

/** Which assistant features exist (LLM configured, scene index filled). */
export function useAssistantStatus() {
  return useQuery({
    queryKey: ['assistant', 'status'],
    queryFn: async () => unwrap(await api.GET('/api/v1/assistant/status')),
    staleTime: 60_000,
  })
}

export function useSceneSearch(q: string, enabled = true) {
  return useQuery({
    queryKey: ['assistant', 'scenes', q],
    queryFn: async () => unwrap(await api.GET('/api/v1/scenes/search', { params: { query: { q, limit: 30 } } })),
    enabled: enabled && q.length > 0,
    placeholderData: keepPreviousData,
  })
}

export function useNaturalSearch(q: string, enabled = true) {
  return useQuery({
    queryKey: ['assistant', 'natural', q],
    queryFn: async () => unwrap(await api.GET('/api/v1/assistant/search', { params: { query: { q, limit: 40 } } })),
    enabled: enabled && q.length > 0,
    // An LLM call: do not repeat it on every focus.
    staleTime: 5 * 60_000,
    placeholderData: keepPreviousData,
  })
}

export function useExplanation(id: number, enabled = true) {
  return useQuery({
    queryKey: ['assistant', 'explain', id],
    queryFn: async () => unwrap(await api.GET('/api/v1/assistant/explain/{id}', { params: { path: { id } } })),
    enabled,
    staleTime: 10 * 60_000,
  })
}

export function useRecap() {
  return useMutation({
    mutationFn: async ({ id, position_ms }: { id: number; position_ms?: number }) =>
      unwrap(
        await api.GET('/api/v1/assistant/recap/{id}', {
          params: { path: { id }, query: position_ms !== undefined ? { position_ms } : {} },
        }),
      ),
  })
}

export interface ChatTurn {
  role: 'user' | 'assistant'
  content: string
  actions?: ChatAction[]
  items?: Schemas['ItemCardDto'][]
}

export function useChat() {
  return useMutation({
    mutationFn: async (messages: ChatTurn[]) =>
      unwrap(
        await api.POST('/api/v1/assistant/chat', {
          body: { messages: messages.map(({ role, content }) => ({ role, content })) },
        }),
      ),
  })
}

// ---- Settings (administrators) ----------------------------------------------------------

const llmKey = ['settings', 'llm']
const scenesKey = ['settings', 'scenes']

export function useLlmSettings() {
  return useQuery({
    queryKey: llmKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/llm')),
  })
}

export function useUpdateLlmSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateLlmSettings']) => unwrap(await api.PUT('/api/v1/settings/llm', { body })),
    onSuccess: (data) => {
      queryClient.setQueryData(llmKey, data)
      void queryClient.invalidateQueries({ queryKey: ['assistant'] })
    },
  })
}

export function useTestLlm() {
  return useMutation({
    mutationFn: async () => unwrap(await api.POST('/api/v1/settings/llm/test')),
  })
}

export function useSceneSettings() {
  return useQuery({
    queryKey: scenesKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/scenes')),
  })
}

export function useUpdateSceneSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateSceneSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/scenes', { body })),
    onSuccess: (data) => queryClient.setQueryData(scenesKey, data),
  })
}
