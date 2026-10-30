import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap } from '../client'

// Polled until the WebSocket event stream arrives (Phase 2.3+).
const POLL_MS = 3000

export function useJobs() {
  return useQuery({
    queryKey: ['jobs', 'list'],
    queryFn: async () => unwrap(await api.GET('/api/v1/jobs', { params: { query: { limit: 50 } } })),
    refetchInterval: POLL_MS,
  })
}

/** What the server is busy with (administrators); polled faster while something runs. */
export function useServerActivity(enabled: boolean) {
  return useQuery({
    queryKey: ['jobs', 'activity'],
    queryFn: async () => unwrap(await api.GET('/api/v1/jobs/activity')),
    enabled,
    refetchInterval: (query) => (query.state.data?.length ? 2000 : 8000),
  })
}

export function useTasks() {
  return useQuery({
    queryKey: ['jobs', 'tasks'],
    queryFn: async () => unwrap(await api.GET('/api/v1/tasks')),
    refetchInterval: POLL_MS * 5,
  })
}

function useInvalidateJobs() {
  const queryClient = useQueryClient()
  return () => queryClient.invalidateQueries({ queryKey: ['jobs'] })
}

export function useRunTask() {
  const invalidate = useInvalidateJobs()
  return useMutation({
    mutationFn: async (name: string) =>
      unwrap(await api.POST('/api/v1/tasks/{name}/run', { params: { path: { name } } })),
    onSuccess: invalidate,
  })
}

export function useCancelJob() {
  const invalidate = useInvalidateJobs()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/jobs/{id}/cancel', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useRetryJob() {
  const invalidate = useInvalidateJobs()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/jobs/{id}/retry', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}
