import * as RadixDialog from '@radix-ui/react-dialog'
import clsx from 'clsx'
import { Camera, ChevronLeft, ChevronRight, Download, Info, MapPin, Pause, Play, X } from 'lucide-react'
import { type ReactNode, useCallback, useEffect, useState } from 'react'
import type { Photo } from '@/api/queries/photos'

/** Seconds per picture in the slideshow. */
const SLIDE_SECONDS = 5
const dateFormat = new Intl.DateTimeFormat('de', { dateStyle: 'full', timeStyle: 'short', timeZone: 'UTC' })

interface LightboxProps {
  photos: Photo[]
  index: number
  onIndex: (index: number) => void
  onClose: () => void
}

/**
 * Full-screen viewer: ← / → (or the buttons) step through the pictures, Space starts
 * and stops the slideshow, I shows the details, Esc closes. Clips play inline.
 */
export function Lightbox({ photos, index, onIndex, onClose }: LightboxProps) {
  const photo = photos[index]
  const [slideshow, setSlideshow] = useState(false)
  const [details, setDetails] = useState(false)
  const count = photos.length
  const step = useCallback((delta: number) => onIndex((index + delta + count) % count), [index, count, onIndex])

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.ctrlKey || event.metaKey || event.altKey) return
      if (event.key === 'ArrowRight') step(1)
      else if (event.key === 'ArrowLeft') step(-1)
      else if (event.key === ' ') setSlideshow((on) => !on)
      else if (event.key === 'i' || event.key === 'I') setDetails((on) => !on)
      else return
      event.preventDefault()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [step])

  // Slideshow: the next picture every few seconds (clips play to their end first).
  useEffect(() => {
    if (!slideshow || photo?.kind === 'video') return
    const timer = window.setTimeout(() => step(1), SLIDE_SECONDS * 1000)
    return () => window.clearTimeout(timer)
  }, [slideshow, step, photo])

  // The neighbours load ahead, so stepping feels instant.
  useEffect(() => {
    for (const neighbour of [photos[(index + 1) % count], photos[(index - 1 + count) % count]]) {
      if (neighbour?.large_url && neighbour.kind === 'photo') new Image().src = neighbour.large_url
    }
  }, [photos, index, count])

  if (!photo) return null
  const camera = photo.camera
  const cameraLine = [camera?.make && !camera.model?.startsWith(camera.make) ? camera.make : null, camera?.model]
    .filter(Boolean)
    .join(' ')
  const exposure = [
    camera?.f_number ? `ƒ/${camera.f_number}` : null,
    camera?.exposure ? `${camera.exposure} s` : null,
    camera?.iso ? `ISO ${camera.iso}` : null,
    camera?.focal_length ? `${camera.focal_length} mm` : null,
  ]
    .filter(Boolean)
    .join(' · ')

  return (
    <RadixDialog.Root open onOpenChange={(open) => !open && onClose()}>
      <RadixDialog.Portal>
        <RadixDialog.Content
          className="fixed inset-0 z-50 flex flex-col bg-black/95 text-white focus:outline-none"
          data-testid="lightbox"
          aria-describedby={undefined}
        >
          <header className="flex items-center gap-3 p-3">
            <RadixDialog.Title className="min-w-0 flex-1 truncate text-sm font-semibold" data-testid="lightbox-title">
              {photo.title}
            </RadixDialog.Title>
            <span className="text-xs text-white/60 tabular-nums" data-testid="lightbox-position">
              {index + 1} / {count}
            </span>
            <LightboxButton label={slideshow ? 'Diashow anhalten' : 'Diashow starten'} onClick={() => setSlideshow(!slideshow)}>
              {slideshow ? <Pause className="size-5" /> : <Play className="size-5" />}
            </LightboxButton>
            <LightboxButton label="Details" onClick={() => setDetails(!details)}>
              <Info className="size-5" />
            </LightboxButton>
            {photo.original_url && (
              <a
                href={photo.original_url}
                download
                aria-label="Original herunterladen"
                className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
              >
                <Download className="size-5" />
              </a>
            )}
            <RadixDialog.Close aria-label="Schließen" className="flex size-10 items-center justify-center rounded-full hover:bg-white/15">
              <X className="size-6" />
            </RadixDialog.Close>
          </header>

          <div className="relative flex min-h-0 flex-1 items-center justify-center">
            {photo.kind === 'video' && photo.original_url ? (
              <video
                key={photo.id}
                src={photo.original_url}
                poster={photo.large_url ?? undefined}
                controls
                autoPlay
                playsInline
                onEnded={() => slideshow && step(1)}
                className="max-h-full max-w-full"
                data-testid="lightbox-video"
              />
            ) : photo.large_url ? (
              <img
                key={photo.id}
                src={photo.large_url}
                alt={photo.title}
                className="max-h-full max-w-full object-contain"
                data-testid="lightbox-image"
              />
            ) : (
              <p className="text-white/60">Dieses Bild lässt sich nicht anzeigen.</p>
            )}
            {count > 1 && (
              <>
                <button
                  type="button"
                  onClick={() => step(-1)}
                  aria-label="Vorheriges Foto"
                  className="absolute left-2 flex size-12 items-center justify-center rounded-full bg-black/40 hover:bg-black/70"
                >
                  <ChevronLeft className="size-7" />
                </button>
                <button
                  type="button"
                  onClick={() => step(1)}
                  aria-label="Nächstes Foto"
                  className="absolute right-2 flex size-12 items-center justify-center rounded-full bg-black/40 hover:bg-black/70"
                >
                  <ChevronRight className="size-7" />
                </button>
              </>
            )}
          </div>

          <footer
            className={clsx('space-y-1 px-4 pb-4 text-sm text-white/80', !details && 'hidden')}
            data-testid="lightbox-details"
          >
            <p>{dateFormat.format(photo.taken_at)}</p>
            {photo.width && photo.height && (
              <p>
                {photo.width} × {photo.height} Pixel
              </p>
            )}
            {(cameraLine || exposure) && (
              <p className="flex items-center gap-1.5">
                <Camera className="size-4" aria-hidden="true" />
                {[cameraLine, camera?.lens, exposure].filter(Boolean).join(' · ')}
              </p>
            )}
            {photo.location && (
              <p className="flex items-center gap-1.5">
                <MapPin className="size-4" aria-hidden="true" />
                <a
                  className="underline hover:text-white"
                  target="_blank"
                  rel="noreferrer"
                  href={`https://www.openstreetmap.org/?mlat=${photo.location.latitude}&mlon=${photo.location.longitude}#map=15/${photo.location.latitude}/${photo.location.longitude}`}
                >
                  {photo.location.latitude.toFixed(5)}, {photo.location.longitude.toFixed(5)}
                </a>
              </p>
            )}
          </footer>
        </RadixDialog.Content>
      </RadixDialog.Portal>
    </RadixDialog.Root>
  )
}

function LightboxButton({ label, onClick, children }: { label: string; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
    >
      {children}
    </button>
  )
}
