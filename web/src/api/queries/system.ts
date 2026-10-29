import { useQuery } from '@tanstack/react-query'
import { api, unwrap } from '../client'

export function useSystemInfo() {
  return useQuery({
    queryKey: ['system', 'info'],
    queryFn: async () => unwrap(await api.GET('/api/v1/system/info')),
    staleTime: 5 * 60_000,
  })
}

/** Hardware transcoding diagnostics (administrators). Polls until detection is done. */
export function useTranscodingInfo() {
  return useQuery({
    queryKey: ['system', 'transcoding'],
    queryFn: async () => unwrap(await api.GET('/api/v1/system/transcoding')),
    refetchInterval: (query) => (query.state.data?.detected === false ? 2_000 : false),
  })
}
