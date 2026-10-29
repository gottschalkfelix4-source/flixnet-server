import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export function useLibraries() {
  return useQuery({
    queryKey: ['libraries'],
    queryFn: async () => unwrap(await api.GET('/api/v1/libraries')),
    // While a first scan is pending or files are being analysed, keep the counters moving.
    refetchInterval: (query) =>
      query.state.data?.some((l) => l.counts.unprobed > 0 || l.last_scan_at == null) ? 2000 : false,
  })
}

function useInvalidateLibraries() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ['libraries'] }),
      queryClient.invalidateQueries({ queryKey: ['jobs'] }),
    ])
}

export function useCreateLibrary() {
  const invalidate = useInvalidateLibraries()
  return useMutation({
    mutationFn: async (body: Schemas['CreateLibraryRequest']) =>
      unwrap(await api.POST('/api/v1/libraries', { body })),
    onSuccess: invalidate,
  })
}

export function useUpdateLibrary() {
  const invalidate = useInvalidateLibraries()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateLibraryRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/libraries/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeleteLibrary() {
  const invalidate = useInvalidateLibraries()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/libraries/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useScanLibrary() {
  const invalidate = useInvalidateLibraries()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/libraries/{id}/scan', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useDirectories(path: string | null) {
  return useQuery({
    queryKey: ['fs', 'directories', path],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/fs/directories', { params: { query: { path: path ?? undefined } } })),
    placeholderData: keepPreviousData,
  })
}

export function useLibraryAccess(userId: number | null) {
  return useQuery({
    queryKey: ['users', userId, 'libraries'],
    queryFn: async () =>
      unwrap(await api.GET('/api/v1/users/{id}/libraries', { params: { path: { id: userId! } } })),
    enabled: userId !== null,
  })
}

export function useSetLibraryAccess() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['LibraryAccess'] & { id: number }) =>
      unwrap(await api.PUT('/api/v1/users/{id}/libraries', { params: { path: { id } }, body })),
    onSuccess: (_, { id }) => queryClient.invalidateQueries({ queryKey: ['users', id, 'libraries'] }),
  })
}
