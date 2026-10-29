import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

const metadataKey = ['settings', 'metadata']

export function useMetadataSettings() {
  return useQuery({
    queryKey: metadataKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/metadata')),
  })
}

export function useUpdateMetadataSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateMetadataSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/metadata', { body })),
    onSuccess: (data) => queryClient.setQueryData(metadataKey, data),
  })
}
