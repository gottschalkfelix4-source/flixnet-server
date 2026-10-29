import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

const settingsKey = ['settings', 'subtitles']

export function useSubtitleSettings() {
  return useQuery({
    queryKey: settingsKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/subtitles')),
  })
}

export function useUpdateSubtitleSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateSubtitleSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/subtitles', { body })),
    onSuccess: (data) => queryClient.setQueryData(settingsKey, data),
  })
}

/** OpenSubtitles search for a movie or episode (runs once a language is chosen). */
export function useSubtitleSearch(itemId: number, language: string | null, fileId?: number) {
  return useQuery({
    queryKey: ['subtitle-search', itemId, language, fileId ?? null],
    enabled: language !== null,
    queryFn: async () =>
      unwrap(
        await api.GET('/api/v1/items/{id}/subtitles/search', {
          params: { path: { id: itemId }, query: { language: language ?? 'de', file_id: fileId } },
        }),
      ),
    staleTime: 5 * 60_000,
    retry: false,
  })
}

export function useDownloadSubtitle() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ itemId, ...body }: { itemId: number } & Schemas['DownloadSubtitleRequest']) =>
      unwrap(
        await api.POST('/api/v1/items/{id}/subtitles/download', {
          params: { path: { id: itemId } },
          body,
        }),
      ),
    // The new track shows up in item details and playback info.
    onSuccess: (_, { itemId }) =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: ['items', 'detail', itemId] }),
        queryClient.invalidateQueries({ queryKey: ['playback', itemId] }),
      ]),
  })
}
