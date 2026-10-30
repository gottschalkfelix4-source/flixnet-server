import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

export type Photo = Schemas['PhotoDto']
export type PhotoAlbum = Schemas['PhotoAlbumDto']

const PAGE = 200

/** The timeline (newest first), page by page; optionally one album. */
export function usePhotoTimeline(albumId?: number) {
  return useInfiniteQuery({
    queryKey: ['photos', 'timeline', albumId ?? null],
    initialPageParam: 0,
    queryFn: async ({ pageParam }) =>
      unwrap(
        await api.GET('/api/v1/photos', {
          params: { query: { album_id: albumId, limit: PAGE, offset: pageParam } },
        }),
      ),
    getNextPageParam: (last) =>
      last.offset + last.items.length < last.total ? last.offset + last.items.length : undefined,
  })
}

export function usePhotoAlbums() {
  return useQuery({
    queryKey: ['photos', 'albums'],
    queryFn: async () => unwrap(await api.GET('/api/v1/photos/albums')),
  })
}
