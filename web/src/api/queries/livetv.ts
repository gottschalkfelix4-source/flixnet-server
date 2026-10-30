import { keepPreviousData, useMutation, useQueries, useQuery, useQueryClient } from '@tanstack/react-query'
import { browserProfile } from '@/lib/deviceProfile'
import { api, unwrap, type Schemas } from '../client'

export type Tuner = Schemas['TunerDto']
export type TunerKind = Schemas['TunerKindDto']
export type EpgSource = Schemas['EpgSourceDto']
export type EpgChannel = Schemas['EpgChannelDto']
export type Channel = Schemas['ChannelDto']
export type Program = Schemas['ProgramDto']
export type LiveSession = Schemas['LiveSessionDto']
export type DiscoveredTuner = Schemas['DiscoveredTunerDto']

const MINUTE = 60_000

/**
 * Channels (in guide order) with what is on now and next; refreshed every minute, or
 * every few seconds where imports and matching are watched (settings).
 */
export function useChannels(includeDisabled = false, refetchMs = MINUTE) {
  return useQuery({
    queryKey: ['livetv', 'channels', includeDisabled],
    queryFn: async () =>
      unwrap(
        await api.GET('/api/v1/livetv/channels', {
          params: { query: { include_disabled: includeDisabled } },
        }),
      ),
    refetchInterval: refetchMs,
  })
}

/** Rows of the guide, loaded page by page as the grid scrolls. */
export const GUIDE_PAGE = 30

export function useGuidePages(pages: number[], startMs: number, endMs: number) {
  return useQueries({
    queries: pages.map((page) => ({
      queryKey: ['livetv', 'guide', startMs, endMs, page],
      queryFn: async () =>
        unwrap(
          await api.GET('/api/v1/livetv/guide', {
            params: {
              query: { start_ms: startMs, end_ms: endMs, offset: page * GUIDE_PAGE, limit: GUIDE_PAGE },
            },
          }),
        ),
      staleTime: 5 * MINUTE,
    })),
  })
}

export function useProgramSearch(q: string) {
  return useQuery({
    queryKey: ['livetv', 'search', q],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/programs', { params: { query: { q, limit: 30 } } })),
    enabled: q.trim().length >= 2,
    placeholderData: keepPreviousData,
  })
}

/**
 * Starts watching a channel (the server answers once the stream runs). Not cached:
 * every player mount gets its own viewer session, which the player ends on close.
 */
export function useWatchChannel(channelId: number) {
  return useQuery({
    queryKey: ['livetv', 'watch', channelId],
    queryFn: async () =>
      unwrap(
        await api.POST('/api/v1/livetv/channels/{id}/watch', {
          params: { path: { id: channelId } },
          body: { device_profile: browserProfile() },
        }),
      ),
    staleTime: Infinity,
    gcTime: 0,
    retry: false,
    refetchOnWindowFocus: false,
  })
}

// ---- Administration -----------------------------------------------------------------

function useInvalidateLiveTv() {
  const queryClient = useQueryClient()
  return () => queryClient.invalidateQueries({ queryKey: ['livetv'] })
}

/** Tuners; polled while an import is pending (no import yet). */
export function useTuners() {
  return useQuery({
    queryKey: ['livetv', 'tuners'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/tuners')),
    refetchInterval: (query) => (query.state.data?.some((t) => t.last_import_at == null) ? 1500 : 10_000),
  })
}

export function useCreateTuner() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (body: Schemas['CreateTunerRequest']) => unwrap(await api.POST('/api/v1/livetv/tuners', { body })),
    onSuccess: invalidate,
  })
}

export function useUpdateTuner() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateTunerRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/livetv/tuners/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeleteTuner() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/livetv/tuners/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useImportTuner() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/livetv/tuners/{id}/import', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useDiscoverTuners() {
  return useMutation({
    mutationFn: async (address?: string) =>
      unwrap(await api.POST('/api/v1/livetv/tuners/discover', { body: address ? { address } : {} })),
  })
}

/** Guide sources; polled while a refresh is pending. */
export function useEpgSources() {
  return useQuery({
    queryKey: ['livetv', 'epg-sources'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/epg-sources')),
    refetchInterval: (query) => (query.state.data?.some((s) => s.last_refresh_at == null) ? 1500 : 15_000),
  })
}

export function useCreateEpgSource() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (body: Schemas['CreateEpgSourceRequest']) =>
      unwrap(await api.POST('/api/v1/livetv/epg-sources', { body })),
    onSuccess: invalidate,
  })
}

export function useUpdateEpgSource() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateEpgSourceRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/livetv/epg-sources/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeleteEpgSource() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/livetv/epg-sources/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useRefreshEpgSource() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/livetv/epg-sources/{id}/refresh', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

/** Guide channels; `version` changes whenever a source finished a refresh. */
export function useEpgChannels(version: string) {
  return useQuery({
    queryKey: ['livetv', 'epg-channels', version],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/epg-channels')),
    placeholderData: keepPreviousData,
  })
}

export function useSetChannelEpg() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['SetChannelEpgRequest'] & { id: number }) =>
      unwrap(await api.PUT('/api/v1/livetv/channels/{id}/epg', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useUpdateChannel() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateChannelRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/livetv/channels/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useRematchChannels() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async () => unwrap(await api.POST('/api/v1/livetv/channels/match')),
    onSuccess: invalidate,
  })
}

export function useLiveStreams() {
  return useQuery({
    queryKey: ['livetv', 'streams'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/streams')),
    refetchInterval: 5000,
  })
}

// ---- Formatting ---------------------------------------------------------------------

const clock = new Intl.DateTimeFormat('de', { hour: '2-digit', minute: '2-digit' })
const weekday = new Intl.DateTimeFormat('de', { weekday: 'long', day: 'numeric', month: 'long' })

/** "20:15" */
export function formatClock(ms: number): string {
  return clock.format(ms)
}

/** "20:15 – 21:45" */
export function formatSlot(program: Pick<Program, 'start_ms' | 'end_ms'>): string {
  return `${formatClock(program.start_ms)} – ${formatClock(program.end_ms)}`
}

/** "Donnerstag, 29. Oktober" */
export function formatDay(ms: number): string {
  return weekday.format(ms)
}

/** Share of a program that has run (0–1). */
export function progress(program: Pick<Program, 'start_ms' | 'end_ms'>, now: number): number {
  const length = program.end_ms - program.start_ms
  return length > 0 ? Math.min(1, Math.max(0, (now - program.start_ms) / length)) : 0
}

export const TUNER_KIND_LABELS: Record<TunerKind, string> = {
  m3u: 'M3U-Playlist',
  xtream: 'Xtream Codes',
  hdhomerun: 'HDHomeRun',
}

// ---- Recordings (DVR) ---------------------------------------------------------------

export type Recording = Schemas['RecordingDto']
export type RecordingStatus = Schemas['RecordingStatus']
export type RecordingRule = Schemas['RecordingRuleDto']
export type LiveTvSettingsData = Schemas['LiveTvSettingsDto']

/** The client's offset from UTC in minutes (time windows, air dates). */
export function utcOffsetMinutes(): number {
  return -new Date().getTimezoneOffset()
}

/** Recordings; polled quickly while one runs, starts soon or waits for the library. */
export function useRecordings() {
  return useQuery({
    queryKey: ['livetv', 'recordings'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/recordings')),
    refetchInterval: (query) => {
      const soon = Date.now() + 2 * MINUTE
      const busy = query.state.data?.some(
        (r) =>
          r.status === 'recording' ||
          (r.status === 'scheduled' && r.start_ms - r.padding_before_s * 1000 < soon) ||
          (r.status === 'completed' && r.item_id == null),
      )
      return busy ? 2000 : 30_000
    },
  })
}

export function useRecordingRules() {
  return useQuery({
    queryKey: ['livetv', 'recording-rules'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/recording-rules')),
  })
}

export function useCreateRecording() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (programId: number) =>
      unwrap(
        await api.POST('/api/v1/livetv/recordings', {
          body: { program_id: programId, utc_offset_min: utcOffsetMinutes() },
        }),
      ),
    onSuccess: invalidate,
  })
}

export function useCreateRecordingRule() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (body: Schemas['CreateRuleRequest']) =>
      unwrap(
        await api.POST('/api/v1/livetv/recording-rules', {
          body: { utc_offset_min: utcOffsetMinutes(), ...body },
        }),
      ),
    onSuccess: invalidate,
  })
}

export function useUpdateRecordingRule() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateRuleRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/livetv/recording-rules/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeleteRecordingRule() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/livetv/recording-rules/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

/** Cancels a scheduled recording or deletes a finished one. */
export function useDeleteRecording() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/livetv/recordings/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useStopRecording() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/livetv/recordings/{id}/stop', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

export function useLiveTvSettings() {
  return useQuery({
    queryKey: ['livetv', 'settings'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/settings')),
  })
}

export function useUpdateLiveTvSettings() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateLiveTvSettings']) => unwrap(await api.PUT('/api/v1/livetv/settings', { body })),
    onSuccess: invalidate,
  })
}

export function useLiveTvProfiles() {
  return useQuery({
    queryKey: ['livetv', 'profiles'],
    queryFn: async () => unwrap(await api.GET('/api/v1/livetv/profiles')),
  })
}

export function useSetLiveTvAccess() {
  const invalidate = useInvalidateLiveTv()
  return useMutation({
    mutationFn: async ({ id, allowed }: { id: number; allowed: boolean }) =>
      unwrap(
        await api.PUT('/api/v1/livetv/profiles/{id}', { params: { path: { id } }, body: { live_tv_allowed: allowed } }),
      ),
    onSuccess: invalidate,
  })
}

export const RECORDING_STATUS_LABELS: Record<RecordingStatus, string> = {
  scheduled: 'Geplant',
  recording: 'Läuft',
  completed: 'Fertig',
  failed: 'Fehlgeschlagen',
  cancelled: 'Übersprungen',
}

/** "1,2 GB" */
export function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000
    unit += 1
  }
  return `${value.toLocaleString('de', { maximumFractionDigits: unit > 1 ? 1 : 0 })} ${units[unit]}`
}
