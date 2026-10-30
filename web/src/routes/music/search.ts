/** URL search parameters of the music page (`/music?tab=…`). */
export const MUSIC_TABS = ['albums', 'artists', 'genres', 'tracks'] as const
export type MusicTab = (typeof MUSIC_TABS)[number]
export const ALBUM_SORTS = ['title', 'date_added', 'year'] as const

export interface MusicSearch {
  tab?: MusicTab
  genre?: string
  sort?: (typeof ALBUM_SORTS)[number]
  library?: number
  /** Titel tab: favorites only. */
  favorites?: boolean
}

export function validateMusicSearch(search: Record<string, unknown>): MusicSearch {
  const out: MusicSearch = {}
  if (MUSIC_TABS.includes(search.tab as MusicTab)) out.tab = search.tab as MusicTab
  if (typeof search.genre === 'string' && search.genre) out.genre = search.genre
  if (ALBUM_SORTS.includes(search.sort as never)) out.sort = search.sort as MusicSearch['sort']
  const library = Number(search.library)
  if (search.library !== undefined && Number.isInteger(library) && library > 0) out.library = library
  if (search.favorites === true || search.favorites === 'true') out.favorites = true
  return out
}
