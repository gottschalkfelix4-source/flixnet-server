# Flixnet – Roadmap

Details zu Architektur und Entscheidungen: [architecture.md](architecture.md).

Regel: ein Meilenstein pro Session. Abgehakt wird erst, wenn das „Fertig wenn“-Kriterium nachweislich erfüllt ist.

## Phase 0 – Grundgerüst
- [x] 0.0 Rust-Toolchain (rustup, stable 1.99.1, MSVC) auf dem Dev-PC
- [ ] 0.0b jellyfin-ffmpeg 7 portable auf dem Dev-PC (spätestens vor Phase 4)
- [x] 0.1 Workspace, axum, tracing, Config, `GET /api/v1/system/info` – *fertig wenn* `cargo test` + `cargo clippy -- -D warnings` grün
- [x] 0.2 flixnet-db: Migrationen, WAL, Writer-/Read-Pool, Settings – *fertig wenn* Migrationstest auf Temp-DB grün
- [x] 0.3 OpenAPI (utoipa 6) + Scalar-UI unter `/api/v1/docs`, `flixnet-server openapi --output web/openapi.json` – Redocly-Lint ohne Fehler
- [x] 0.4 Web-Shell (React/Vite, Theme, Sidebar, generierter Client), per rust-embed ausgeliefert – *fertig wenn* Playwright-Smoke grün
- [x] 0.5 `xtask fixtures` (8 Testmedien inkl. HDR10, AV1, VP9, MPEG-2 interlaced, Staffel mit gemeinsamem Intro), Dockerfile (jellyfin-ffmpeg 7.1, PUID/PGID, tini), GitHub Actions (Rust, Web, E2E, GHCR), Unraid-Template – lokal im Container verifiziert
- [ ] 0.5b Container auf Unraid installieren (`docker/unraid-template.xml`) – braucht Felix (GHCR-Paket ist privat → Login auf Unraid oder Paket öffentlich machen)

## Phase 1 – Auth und Nutzer
- [x] 1.1 Setup-Assistent, argon2, Login → Device-Token, Auth-Extractor – *fertig wenn* jede OpenAPI-Operation ohne Token 401 liefert
- [x] 1.2 Haushalt, Profile mit PIN („Wer schaut?“), Nutzerverwaltung, Einladungen, Admin-Rolle, Altersgrenze pro Profil, Geräteverwaltung, Brute-Force-Schutz
  - Bibliotheksrechte pro Nutzer: `all_libraries`-Flag vorhanden, Einzelfreigaben kommen mit den Bibliotheken in 2.3
  - Altersgrenze wird ab 2.6 (Items-API) durchgesetzt

## Phase 2 – Scannen und Metadaten
- [x] 2.1 Dateinamen-Parser (360+ Testfälle: Release-Namen, Plex/Jellyfin-Ordner, ID-Hinweise, S01E02/1x02/Doppelfolgen/Staffel-Folge/Datum/Anime-absolut, Extras, Samples, Mehrteiler, Untertitel-Sidecars)
- [x] 2.2 Job-Runtime + Jobs-API – *fertig wenn* Kill während der Queue → Neustart setzt fort (Test `crash_mid_queue_resumes_after_restart`); Pools, Prioritäten, Dedupe, Retry mit Backoff, Abbruch, Scheduler, Admin-UI „Aufgaben“
- [x] 2.3 FS-Diff → media_files → ffprobe-Jobs – *fertig wenn* Rescan ohne Änderung null Probes macht (Test `scan_builds_items_and_rescan_is_a_no_op`); Bibliotheken-API + UI mit Ordner-Browser, Bibliotheksrechte pro Nutzer, Sidecar-Untertitel, Mehrteiler, Extras, unerreichbare Ordner markieren nichts als fehlend
- [x] 2.4 NFO + lokale Artworks (Kodi-NFO für Film/Serie/Episode, Poster/Fanart/Logo/Banner/Thumbs, Staffel-Poster; NFO-Felder gesperrt gegen Provider; Genres, Studios, Darsteller/Regie)
- [x] 2.5 TMDB-Client (de-DE + en-US-Fallback, Response-Cache, 429-Retry), Matching mit Confidence (Titel/Original/Jahr), Identify (Suche + manuell), Staffeln/Folgen, FSK, Bild-Cache (Resize, Blurhash, signierte URLs), Einstellungs-Tab „Metadaten“ – mit Mock-TMDB getestet
  - **Offen (Felix):** TMDB-API-Schlüssel unter Einstellungen → Metadaten eintragen
- [x] 2.6 Items-API (Filter-DSL, Sortierung, Paging, A–Z, FTS5) – *fertig wenn* p95 < 50 ms bei 50k Items → gemessen p95 ≈ 10–13 ms (Test `p95_under_50ms_on_50k_items`); Altersgrenze (unbewertet = verborgen, Folgen erben von der Serie) und Bibliotheksrechte überall; Detail-API mit Personen, Artwork, Dateien/Streams/Kapiteln
  - Offen: Personenbilder kommen noch direkt von TMDB (später über den Bild-Cache)
- [x] 2.7 Watcher, Rename-Erkennung – *fertig wenn* Watch-State nach Rename erhalten bleibt (Test `watch_state_survives_renames_moves_and_matching`); stabile Identität (Provider-ID bzw. Content-Key, Folgen erben von der Serie), Verschiebe-Erkennung über Größe+mtime, notify-Watcher mit Debounce, Gesehen/Favoriten-API und -Filter
- [x] 2.8 fanart.tv (HD-Logos, Banner, Hintergründe; optionaler Schlüssel), Personen/Credits inkl. Personen-API mit Filmografie
  - TVDB v4 verschoben nach Phase 9 (braucht kostenpflichtiges Abo/PIN; TMDB deckt Serien ab, TVDB bringt v. a. alternative Anime-Reihenfolgen)

## Phase 3 – Browsing-UI
- [x] 3.1 Home (Hero, Reihen), Bibliotheks-Grid, Detailseiten, Suche — virtualisiertes Grid mit A–Z, Sortierung/Genre/Gesehen-Filter; Film-, Serien- (Staffel-Tabs) und Folgenseiten mit Besetzung, Technik und „Mehr wie das“; Personenseite; Identifizieren-Dialog; E2E in `browse.spec.ts`

## Phase 4 – Wiedergabe
- [x] 4.1 Direct Play mit Range-Requests — `playback-info` mit an Nutzer+Datei gebundenen signierten URLs, `/stream/{file_id}` (206/416, Rechte bei jedem Request neu geprüft), Vollbild-Player-Seite; E2E `playback.spec.ts`
- [ ] 4.2 DeviceProfile + Decision-Engine + playback-info
- [ ] 4.3 HLS-Transcode (Software) + TranscodeManager
- [ ] 4.4 Remux mit Keyframe-Index, Audio-Transcode
- [ ] 4.5 QSV/VAAPI/NVENC + HDR→SDR-Tonemapping
- [ ] 4.6 Untertitel (WebVTT, PGS-Burn-in), Spurwechsel
- [ ] 4.7 Fortschritt, Weiterschauen, Nächste Folge, Sessions
- [ ] 4.8 Eigener Player, Trickplay, Kapitelbilder, Autoplay

## Phase 5 – Analyse und Organisation
- [ ] 5.1 Intro/Credits-Erkennung + Skip
- [ ] 5.2 OpenSubtitles
- [ ] 5.3 Collections, Playlists, Smart Playlists
- [ ] 5.4 Jugendschutz

## Phase 6 – KI
- [ ] 6.1 Model-Manager + Item-Embeddings (sqlite-vec)
- [ ] 6.2 Hybrid-Suche (BM25 + Vektor, RRF)
- [ ] 6.3 Recommendations (Taste-Cluster, Co-Watch, MMR, Onboarding)
- [ ] 6.4 LLM: natürliche Sprache → Filter, Erklärungen
- [ ] 6.5 Szenensuche (Untertitel-Chunks, Whisper opt-in)
- [ ] 6.6 Gruppenmodus
- [ ] 6.7 Chat-Assistent, spoilerfreie Recaps, Themen-Collections

## Phase 7 – Live TV und DVR
- [ ] 7.1 Tuner-Trait, M3U/Xtream → HLS
- [ ] 7.2 HDHomeRun
- [ ] 7.3 XMLTV, Sender-Matching, Guide-Grid
- [ ] 7.4 Shared Upstream, Timeshift
- [ ] 7.5 DVR (Regeln, Konflikte, Aufnahmen als Items)
- [ ] 7.6 Schedules Direct, tvheadend

## Phase 8 – Jellyfin-Kompatibilität
- [ ] 8.1 Auth, Views, Items
- [ ] 8.2 PlaybackInfo, Streams
- [ ] 8.3 Sessions, Resume, NextUp, Bilder – *fertig wenn* Infuse, Swiftfin, Findroid, Streamyfin funktionieren

## Phase 9 – Breite und Feinschliff
- [ ] Musik · SyncPlay · Chromecast · Trakt · Webhooks/Benachrichtigungen · Dashboard · Backups · History-Import · OIDC · Unraid-CA-Template · Hörbücher/Fotos/DLNA
