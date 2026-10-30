import type { Schemas } from '@/api/client'

/** Job kinds as tasks ("what this job does"), for the job list. */
export const KIND_LABELS: Record<string, string> = {
  'maintenance.cleanup_jobs': 'Aufgabenverlauf aufräumen',
  'library.scan': 'Bibliothek scannen',
  'library.scan_all': 'Alle Bibliotheken scannen',
  'media.probe': 'Mediendatei analysieren',
  'media.trickplay': 'Vorschaubilder erzeugen',
  'media.chapter_images': 'Kapitelbilder erzeugen',
  'media.thumbnails_backfill': 'Fehlende Vorschau- und Kapitelbilder suchen',
  'metadata.refresh': 'Metadaten aktualisieren',
  'notify.deliver': 'Benachrichtigung zustellen',
  'maintenance.backup': 'Datenbank sichern',
  'trakt.sync_all': 'Mit Trakt abgleichen',
}

/** Job kinds as ongoing activity ("what the server is doing"), for the activity menu. */
const ACTIVITY_LABELS: Record<string, string> = {
  'library.scan_all': 'Alle Bibliotheken werden gescannt',
  'media.probe': 'Mediendateien werden analysiert',
  'metadata.refresh': 'Metadaten werden aktualisiert',
  'media.trickplay': 'Vorschaubilder werden erzeugt',
  'media.chapter_images': 'Kapitelbilder werden erzeugt',
  'media.thumbnails_backfill': 'Fehlende Vorschaubilder werden gesucht',
  'media.segments': 'Intros und Abspann werden erkannt',
  'media.segments_backfill': 'Intros und Abspann werden gesucht',
  'subtitles.auto_download': 'Untertitel werden geladen',
  'audiobooks.analyze': 'Hörbücher werden analysiert',
  'photos.clip': 'Fotos werden analysiert',
  'ai.embed_items': 'KI-Index wird aufgebaut',
  'ai.scene_index': 'Szenensuche wird indiziert',
  'ai.scene_backfill': 'Szenensuche wird ergänzt',
  'ai.transcribe': 'Dialoge werden transkribiert',
  'ai.theme_collections': 'Themen-Sammlungen werden erstellt',
  'ai.download_model': 'KI-Modell wird geladen',
  'livetv.refresh_guide': 'Programmführer wird aktualisiert',
  'livetv.import_channels': 'Sender werden importiert',
  'maintenance.backup': 'Datenbank wird gesichert',
  'trakt.sync_all': 'Trakt wird abgeglichen',
}

export function activityTitle(a: Schemas['ActivityDto']): string {
  if (a.kind === 'library.scan') return `„${a.library_name ?? 'Bibliothek'}“ wird gescannt`
  return ACTIVITY_LABELS[a.kind] ?? KIND_LABELS[a.kind] ?? a.kind
}
