import { useQuery } from '@tanstack/react-query'
import { api, unwrap } from '../client'

export function useSystemInfo() {
  return useQuery({
    queryKey: ['system', 'info'],
    queryFn: async () => unwrap(await api.GET('/api/v1/system/info')),
    staleTime: 5 * 60_000,
  })
}
