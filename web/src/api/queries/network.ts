import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

const key = ['settings', 'network']

export function useNetworkSettings() {
  return useQuery({
    queryKey: key,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/network')),
  })
}

export function useUpdateNetworkSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateNetworkSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/network', { body })),
    onSuccess: (data) => queryClient.setQueryData(key, data),
  })
}
