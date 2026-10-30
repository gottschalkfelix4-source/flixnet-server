import { CheckCircle2, Download, Fingerprint } from 'lucide-react'
import { useState } from 'react'
import type { ItemDetail } from '@/api/queries/items'
import { useDownloadSubtitle, useSubtitleSearch } from '@/api/queries/subtitles'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { SelectField } from '@/components/ui/Section'
import { SUBTITLE_LANGUAGES } from '@/lib/subtitleLanguages'

/** Admin: find subtitles on OpenSubtitles and add one as a track. */
export function SubtitleSearchDialog({ item, onClose }: { item: ItemDetail; onClose: () => void }) {
  const mainFiles = item.files.filter((f) => f.role === 'main')
  const [language, setLanguage] = useState('de')
  const [fileId, setFileId] = useState<number | undefined>(mainFiles[0]?.id)
  const search = useSubtitleSearch(item.id, language, fileId)
  const download = useDownloadSubtitle()
  const [done, setDone] = useState<number[]>([])
  const label = SUBTITLE_LANGUAGES.find((l) => l.value === language)?.label ?? language

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title="Untertitel suchen"
      description={`Bei OpenSubtitles.com für „${item.title}“.`}
    >
      <div className="mb-4 grid gap-3">
        <SelectField label="Sprache" value={language} onChange={setLanguage} options={SUBTITLE_LANGUAGES} />
        {mainFiles.length > 1 && (
          <SelectField
            label="Version"
            value={String(fileId ?? '')}
            onChange={(v) => setFileId(Number(v))}
            options={mainFiles.map((f) => ({ value: String(f.id), label: f.file_name }))}
          />
        )}
      </div>
      <FormError error={search.error ?? download.error} />
      {download.data && done.length > 0 && (
        <p role="status" className="mb-3 flex items-center gap-2 text-sm text-emerald-400">
          <CheckCircle2 className="size-4" aria-hidden="true" />
          Untertitel gespeichert{download.data.saved_in_library ? '' : ' (im Flixnet-Speicher)'} – die Spur ist jetzt verfügbar.
          {download.data.remaining_downloads != null && ` Heute noch ${download.data.remaining_downloads} Downloads.`}
        </p>
      )}
      <ul className="max-h-[50vh] space-y-2 overflow-y-auto" aria-label="Suchergebnisse">
        {search.isPending && <li className="text-sm text-muted">Suche läuft…</li>}
        {search.data?.results.length === 0 && (
          <li className="text-sm text-muted">Keine Untertitel auf {label} gefunden.</li>
        )}
        {search.data?.results.map((r) => {
          const saved = done.includes(r.subtitle_file_id)
          return (
            <li key={r.subtitle_file_id} className="rounded-lg border border-line/70 p-3" data-testid="subtitle-result">
              <p className="text-sm font-bold break-all">{r.release ?? r.file_name ?? 'Ohne Namen'}</p>
              <p className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted">
                {r.moviehash_match ? (
                  <span className="inline-flex items-center gap-1 font-semibold text-emerald-400">
                    <Fingerprint className="size-3.5" aria-hidden="true" />
                    Passt genau zur Datei
                  </span>
                ) : (
                  <span>Passung {r.match_score} %</span>
                )}
                <span>{r.download_count.toLocaleString('de-DE')} Downloads</span>
                {r.votes > 0 && <span>Bewertung {r.rating.toFixed(1)}</span>}
                {r.fps != null && <span>{r.fps.toLocaleString('de-DE')} fps</span>}
                {r.hearing_impaired && <span>Für Hörgeschädigte</span>}
                {r.foreign_parts_only && <span>Nur fremdsprachige Stellen</span>}
                {r.machine_translated && <span className="text-amber-300">Maschinell übersetzt</span>}
                {r.from_trusted && <span>Vertrauenswürdig</span>}
              </p>
              <div className="mt-2 flex justify-end">
                <Button
                  type="button"
                  variant={saved ? 'ghost' : 'accent'}
                  className="h-9 px-3 text-sm"
                  disabled={saved || download.isPending}
                  loading={download.isPending && download.variables?.subtitle_file_id === r.subtitle_file_id}
                  onClick={() =>
                    download.mutate(
                      {
                        itemId: item.id,
                        subtitle_file_id: r.subtitle_file_id,
                        language,
                        file_id: search.data?.file_id,
                        hearing_impaired: r.hearing_impaired,
                        forced: r.foreign_parts_only,
                        release: r.release,
                      },
                      { onSuccess: () => setDone((d) => [...d, r.subtitle_file_id]) },
                    )
                  }
                >
                  {saved ? (
                    <>
                      <CheckCircle2 className="size-4" aria-hidden="true" />
                      Gespeichert
                    </>
                  ) : (
                    <>
                      <Download className="size-4" aria-hidden="true" />
                      Herunterladen
                    </>
                  )}
                </Button>
              </div>
            </li>
          )
        })}
      </ul>
    </Dialog>
  )
}
