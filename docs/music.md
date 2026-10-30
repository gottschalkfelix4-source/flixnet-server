# Musik

Musikbibliotheken (Typ `music`) neben Filmen und Serien: Tags statt Dateinamen, MusicBrainz und Cover Art Archive, ein seitenübergreifender Player mit lückenloser Wiedergabe, ReplayGain und synchronen Liedtexten.

## Datenmodell

- Items bilden die Kette **Künstler → Album → Titel** (`artist`, `album`, `track`, Migration `0021_music.sql`).
  - Alben und Titel haben `parent_id` = Elternteil und `series_id` = Album-Künstler, damit alles eines Künstlers ein Index-Lookup ist.
  - CDs sind keine Items: Disc-Nummer = `parent_index_number`, Titelnummer = `index_number`.
- Tags liegen pro Datei in `audio_tags` (Interpreten, Nummern, Jahr, Genres, Compilation, Explicit, ReplayGain, MusicBrainz-IDs, Cover/Lyrics-Flags, Dauer).
- Schlüssel (`content_key`):
  - Künstler immer über den Namen (`local:artist:<name>`), damit getaggte und ungetaggte Dateien beim selben Künstler landen.
  - Alben über die MusicBrainz-Release-ID, sonst Album-Künstler + Albumname.
  - Titel über die MusicBrainz-Release-Track-ID, sonst Album + CD + Nummer + Titel. Mehrere Dateien mit gleichem Schlüssel (FLAC und MP3) werden Versionen eines Titels.
- Identität (`identity_key`) und damit Favoriten und Wiedergabezähler: `mb:track:…`, `mb:release:…`, `mb:artist:…`, sobald IDs bekannt sind (aus Tags oder MusicBrainz), sonst der Schlüssel.

## Scanner: lofty statt ffprobe für Tags

- **Entscheidung:** Tags liest [lofty](https://crates.io/crates/lofty) direkt im Scan.
  - Ein ffprobe-Prozess pro Datei würde den ersten Scan großer Sammlungen um Minuten verlängern.
  - lofty liest nur die Tag-Blöcke und liefert einheitlich über FLAC, MP3, MP4/ALAC, Ogg/Opus und WAV: Album-Künstler, CD/Titelnummern, MusicBrainz-IDs, ReplayGain/R128, eingebettete Bilder und Liedtexte.
  - ffprobe läuft danach wie bei jeder Datei (Streams, exakte Dauer).
- Die Tags eines Batches werden **vor** dessen Schreibtransaktion gelesen (auf einem Blocking-Thread).
- Dateien ohne lesbare Tags (WMA, kaputte Tags) fallen auf die Ordnerstruktur `Interpret/Album[/CD1]/01 - Titel.ext` zurück.
- Compilations:
  - Flag `compilation` → „Verschiedene Interpreten“.
  - Ohne Flag und ohne Album-Künstler: ein Album in einem Ordner mit mehreren Interpreten gilt als Compilation. ffmpeg und manche Tagger schreiben das Flag bei MP3 als unbekanntes `TXXX`. „A feat. B“ zählt als A.
- Re-Tags: Eine geänderte Datei wird neu eingeordnet, leere Alben und Künstler werden entfernt.
- Audio bekommt keine Trickplay-, Kapitelbild- oder Intro-Jobs.

## Metadaten

- **Lokal:**
  - Album-Cover `cover`/`folder`/`front`/`album` im Albumordner (auch über CD-Ordnern), sonst das eingebettete Cover. Der Bild-Cache liest es bei Bedarf aus den Tags.
  - Künstlerbilder `artist`/`folder` im Künstlerordner. Ohne eigenes Bild zeigt ein Künstler das Cover eines Albums.
  - Jahr, Genres und Laufzeit eines Albums ergeben sich aus seinen Titeln.
- **MusicBrainz** (abschaltbar unter Einstellungen → Metadaten, ohne Schlüssel):
  - Release per ID aus den Tags oder per Suche. Übernommen wird ein Treffer nur mit Score ≥ 0,85 aus Titel, Interpret, MusicBrainz-Score und Titelzahl.
  - Übernommen werden Veröffentlichungsdatum, Genres (wenn die Tags keine haben) und die Release-Track-/Recording-IDs der Titel.
  - Künstler per ID oder eindeutigem exakten Namen.
  - Höchstens **eine Anfrage pro Sekunde** pro API-Wurzel und prozessweit, mit User-Agent `Flixnet/<Version> ( https://github.com/gottschalkfelix4-source/flixnet-server )`. 503/429 wird einmal nach `Retry-After` wiederholt.
  - Antworten (auch 404) landen im Provider-Cache: Lookups 30 Tage, Suchen 7 Tage.
- **Cover Art Archive:** Front-Cover (500 px) des Releases, sonst der Release-Group.
- **fanart.tv** (mit vorhandenem Schlüssel): Künstlerbild, Hintergrund, Logo, Banner.
- **Tests und E2E:** Die Basis-URLs lassen sich über `FLIXNET_MUSICBRAINZ_URL` und `FLIXNET_COVERART_URL` umbiegen.

## Jugendschutz

- **Entscheidung:** Musik hat kein Altersfreigabe-System.
  - Unbewertete Künstler, Alben und Titel zählen deshalb als „ab 0“, auch wenn ein Profil unbewertete Inhalte ausblendet. Sonst wäre für eingeschränkte Profile alle Musik verschwunden.
  - Titel mit Explicit-Kennzeichnung (iTunes-Advisory `1`/`4`) bekommen die Altersfreigabe 16.
- Bibliotheksrechte gelten wie überall: Ein Profil sieht nur Musik aus Bibliotheken, die es sehen darf. Das gilt für Raster, Album- und Künstlerseiten, Titellisten, Liedtexte, Wiedergabequellen, signierte Streams und Scrobbles.
- Gesperrte Genres wirken auf die eigenen Genres des Items.
  - Ein Album ist ausgeblendet, wenn eines seiner (aus den Titeln zusammengefassten) Genres gesperrt ist.
  - Ein Titel ist ausgeblendet, wenn eines seiner eigenen Genres gesperrt ist.

## Wiedergabe

- `POST /music/playback` entscheidet je Titel und Gerät:
  - **Direct Play:** die Originaldatei über `/stream/{file_id}`.
  - **Umwandlung:** AAC 256 kbit/s in `.m4a` über `/music/stream/{file_id}`, bei ALAC in Chromium, WMA, APE usw.
  - Die Entscheidung ist der Audio-Zweig der Decision-Engine (`flixnet_media::audio::decide_audio`), gleiche Gründe wie bei Video.
- Umgewandelt wird einmal vollständig in einen Cache (`<config>/cache/audio`, älteste Dateien ab 2 GB gelöscht) und dann mit Range-Requests ausgeliefert.
  - Exakte Dauer, echtes Spulen.
  - Das Vorladen für lückenloses Abspielen funktioniert wie bei Originaldateien.
- **Lückenlos:** Zwei `<audio>`-Elemente wechseln sich ab. Während eines spielt, ist der nächste Titel im anderen schon geladen und startet beim `ended` des ersten. Gemessen im E2E: < 400 ms.
- **ReplayGain:** Titel oder Album, als Web-Audio-Gain mit Clipping-Schutz über den Peak. Bei Zufallswiedergabe gilt immer der Titelwert.
- **Liedtexte:** `.lrc` neben der Datei (synchron, mit `[offset:]`, mehreren Zeitstempeln je Zeile), sonst eingebettete Texte (synchron, wenn sie LRC-Zeitstempel enthalten).
- **Media Session:** Titel, Interpret, Album und Cover für Sperrbildschirm und Medientasten.
- **Warteschlange:** Sie gehört dem Gerät (localStorage) und übersteht Neuladen.
- **Wiedergabezähler:** `POST /music/tracks/{id}/scrobble` zählt ab der Hälfte des Titels oder vier Minuten (Last.fm-Regel). Die Hördauer zählt ohne Pausen und Sprünge.
  - Zählt ein Scrobble, steigen Zähler und „zuletzt gehört“, und ein Eintrag in `watch_events` entsteht.
  - „Beliebte Titel“ eines Künstlers summiert die Zähler aller Profile.

## Offen

- Jellyfin-Kompatibilität für Musik (Musikbibliotheken erscheinen dort noch nicht sinnvoll).
- WMA-Tags (lofty liest kein ASF): nur Ordnerstruktur.
- Sonic Radio per Audio-Embeddings (Phase 9, später).
