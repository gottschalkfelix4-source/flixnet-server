# Flixnet – eigener Media Server in Rust

## Kontext

Felix nutzt aktuell Jellyfin (davor Plex und Emby). Jellyfin ist solide, entwickelt sich bei KI-Funktionen aber nicht weiter. Silo Server hat die gewünschten KI-Funktionen, ist aber zu jung: Go + Postgres, Repo erst seit Mai 2026, kein Release, „APIs may change“.

Ziel ist ein eigener, performanter Media Server in **Rust**:
- Funktionsumfang wie Plex/Jellyfin/Emby
- KI-Funktionen auf Silo-Niveau und darüber hinaus
- Live TV und DVR
- UI im Stil von Silo/Plex, als eigenes Design
- Native iOS/Android-Apps folgen später; die API wird von Anfang an dafür gebaut.

Der Projektordner `D:\Ai Projekte\Claude Code\Flixnet-Server` ist leer; Flixnet ist Greenfield.

### Getroffene Entscheidungen

| Thema | Entscheidung |
|---|---|
| Frontend | React 19 + Vite |
| Jellyfin-kompatible API | ja, als eigene Phase. Damit funktionieren Infuse, Swiftfin, Findroid und Streamyfin schon vor den eigenen Apps. |
| KI | Hybrid: lokal per ONNX, dazu optional ein OpenAI-kompatibler Endpoint (Ollama/OpenAI) |
| Hardware | Intel iGPU mit QuickSync auf Unraid. Der Windows-Dev-PC hat eine NVIDIA-GPU, daran wird der NVENC-Pfad getestet. |

## Recherche: Kernerkenntnisse

- **Jellyfin-Schmerzpunkte, die wir vermeiden:**
  - SQLite-Locks beim Scannen
  - langsame, gekoppelte Scans
  - Clients holen ganze Bibliotheken statt serverseitig zu filtern und zu pagen
  - Watch-State geht beim Umbenennen verloren
  - ungeschützte Stream- und Bild-Endpoints (CVE-2023-49096: ffmpeg-Argument-Injection)
  - API-Brüche zwischen Minor-Versionen
- **Plex:** alles hinter Paywall (HW-Transcoding, Intro-Skip, DVR, Remote). Cloud-Login ist Pflicht, es gab Datenlecks 2022 und 2025.
  → Bei uns ist alles kostenlos, lokal und ohne Cloud-Zwang.
- **Silo (siloserver.org):**
  - Recommendations: Embedding-Text pro Titel, pgvector, Taste-Profil als gewichteter Zentroid mit 180-Tage-Halbwertszeit, Co-Watch, MMR mit λ=0.7, Onboarding.
  - Erklärungen nur regelbasiert.
  - Für jede KI-Funktion ist ein externer Endpoint nötig.
- **Lücken, mit denen wir uns abheben:**
  - Szenensuche über Untertitel, Whisper-Transkripte und Keyframes, mit Sprung zum Timestamp
  - Gruppenmodus „Was schauen wir heute?“ über mehrere Haushaltsprofile
  - LLM-Erklärungen, die auf der eigenen Watch-History beruhen
  - spoilerfreie Recaps
  - automatische Themen-Collections
  - alle Basis-KI-Funktionen laufen **komplett lokal, ohne Endpoint**
- **Dim** (Rust-Media-Server) ist eingeschlafen. Die Lehre daraus: Transcoding, Client-Kompatibilität und Metadaten-Matching fressen die Zeit, also strikte Meilensteine.

## Architektur

### Tech-Stack

- **Backend:**
  - axum 0.8, tokio, tower-http
  - utoipa 5 + utoipa-axum: OpenAPI unter `/api/v1/openapi.json`, später Codegen für Swift/Kotlin
  - sqlx 0.9 auf SQLite (WAL, 1 Writer-Connection, separater Read-Pool), Schema Postgres-portabel
  - FTS5 für die Suche, argon2 für Passwörter
  - Device-Tokens, in der DB nur als SHA-256 gespeichert; signierte, ablaufende Media-URLs
- **Medien:**
  - ffmpeg/ffprobe nur als CLI-Prozess (jellyfin-ffmpeg 7 im Docker-Image), nie gelinkt
  - typisierter `FfmpegArgs`-Builder: keine User-Strings in argv
  - HLS mit fMP4 (TS als Fallback); Playlists erzeugt der Server, bei einem Seek wird ffmpeg mit `-ss`/`-start_number` neu gestartet
- **KI:**
  - ort/fastembed: multilingual-e5-base (int8) für Deutsch und Englisch; CLIP bzw. SigLIP für Bilder
  - whisper-rs für Untertitel, sqlite-vec hinter einem `VectorIndex`-Trait
  - optionaler OpenAI-kompatibler LLM-Client
- **Frontend:**
  - React 19, Vite, TS, Tailwind 4, Radix, TanStack Query/Router
  - hls.js, embla-carousel, framer-motion
  - openapi-typescript-Client; die SPA wird per rust-embed ins Binary eingebettet
- **Ports:** 8097 für Flixnet, 8098 für die Jellyfin-Compat-Schicht (standardmäßig aus). So gibt es keinen Konflikt mit einem parallel laufenden Jellyfin.

### Projektstruktur

```
Cargo.toml (Workspace)
crates/
  flixnet-db/        sqlx-Pools, migrations/, Queries, FTS5/vec-Helper
  flixnet-core/      Config (figment: toml + FLIXNET_* env), Fehler, IDs, Filter-DSL,
                     DeviceProfile, EventBus, Job-Runtime, Auth-Service
  flixnet-library/   FS-Walker/Diff, notify-Watcher, Dateinamen-Parser, NFO,
                     Metadaten-Provider (TMDB/TVDB/fanart/MusicBrainz), Matcher, Bild-Cache+Blurhash
  flixnet-media/     ffprobe/ffmpeg-Wrapper, HW-Erkennung, Decision-Engine, TranscodeManager,
                     HLS, Untertitel, Trickplay, Kapitelbilder, Intro/Credits-Erkennung
  flixnet-api/       axum-Router /api/v1, Auth-Extractor, DTOs, WebSocket
  flixnet-server/    Binary (serve | openapi | migrate | admin), Verdrahtung, eingebettete SPA
  später: flixnet-ai/ (Feature-Flag), flixnet-livetv/, flixnet-jellyfin/
xtask/               fixtures | openapi | dev | docker | seed  (plattformunabhängig, kein Bash)
web/src/             api/ app/ routes/ components/{ui,media}/ player/ features/ lib/ styles/  e2e/
docker/              Dockerfile, unraid-template.xml
docs/roadmap.md, CLAUDE.md
```

Abhängigkeitsrichtung: `db ← core ← {library, media, ai, livetv} ← api ← server`

### Kern-Datenmodell

**Bibliothek**
- `libraries`, `library_paths`
- `items`: ein Typbaum für movie, series, season, episode, artist, album, track, collection, recording.
  - `content_key` ist eine stabile Identität wie `tmdb:movie:603`, mit Hash-Fallback.
  - `rating_rank` dient dem Jugendschutz, `locked_fields` schützt manuelle Änderungen.
- `media_files` (mehrere Versionen pro Item, `quick_hash` für Rename-Erkennung), `media_streams` (inkl. HDR/DoVi), `chapters`, `trickplay`
- `segments` (intro, credits, recap)
- `provider_ids`, `people`, `credits`, Genres/Tags/Studios
- `images` (Poster, Backdrop, Logo; mit Blurhash), `collection_items`, `playlists` (mit `smart_filter_json`)

**Nutzer**
- `households`, `users` (PIN, `max_rating_rank`, Policy), `user_library_access`, `devices`, `access_tokens`, `invitations`
- `user_item_data`: über **content_key** statt Item-ID verschlüsselt. Damit überlebt der Watch-State Umbenennen und Rescans.
- `watch_events`: append-only. Liefert die Basis für Recommendations, Trakt und den History-Import.

**Infrastruktur**
- `jobs` (persistent, Priorität, Pool, Heartbeat, `dedupe_key`), `scheduled_tasks`, `settings`, `activity_log`, `provider_cache`

**KI**
- `embedding_models`, `transcript_chunks` + FTS
- vec0-Tabellen pro Modell, getrennt für Items, Chunks und Frames
- `keyframes`, `taste_profiles`

**Live TV**
- `tuner_hosts`, `channels`, `epg_sources`, `programs` + FTS, `recording_rules`, `recordings`

### Interne Architektur

- **Job-Queue in SQLite.**
  - Jobs werden per `UPDATE … RETURNING` atomar geclaimt.
  - Pools: io=8, probe=4, cpu=cores/4, gpu=1–2, ai=1.
  - Prioritäten: interaktiv 100, neue Items 50, Backfill 10.
  - Bei Absturz werden Jobs neu eingereiht, Fehler mit Backoff wiederholt.
  - Die Pools cpu, gpu und ai pausieren, während jemand transkodiert schaut.
- **Scan in Stufen:** schneller FS-Diff → ffprobe → Metadaten → schwere Analysen (Trickplay, Fingerprints, Embeddings) als Hintergrundjobs.
  - Dazu ein notify-Watcher plus periodischer Rescan, weil Unraid-shfs inotify-Events verliert.
- **DB-Regel:** nie eine Schreibtransaktion über externe I/O halten; geschrieben wird in Batches von etwa 200 Zeilen.
- **EventBus** (tokio broadcast) → `/api/v1/ws` mit Rechteprüfung → im Web invalidiert TanStack Query die betroffenen Daten.
- **Decision-Engine** als reine Funktion `decide(MediaInfo, DeviceProfile, UserPolicy, Request) → PlaybackPlan`.
  - Ergebnis ist DirectPlay, Remux oder Transcode, jeweils mit Gründen. Die Gründe sieht man im Dashboard.
  - Testbar als Tabellen-Test über eine Matrix aus Medien und Profilen.
- **TranscodeManager:**
  - Sessions mit eigenem Verzeichnis unter `/transcode`.
  - Liegt ein angefragtes Segment außerhalb des Lookahead, wird ffmpeg neu gestartet.
  - Läuft ffmpeg zu weit voraus, wird es gestoppt und bei Bedarf fortgesetzt.
  - Nach 60 s Leerlauf räumt ein Reaper die Session ab; es gibt Limits pro GPU und pro Nutzer.
  - Remux nutzt einen Keyframe-Index.
- **HW-Erkennung beim Start:** Testencode mit 1 Frame pro Encoder (QSV, VAAPI, NVENC, AMF), dazu Pipeline- und Tonemap-Tests. Die Ergebnisse stehen auf einer Diagnoseseite; ohne Hardware wird automatisch in Software transkodiert.
- **Plugins:** anfangs keine In-Process-Plugins. Erweiterungen laufen über Webhooks, WebSocket und Plugin-Tokens; WASM kommt nur, wenn ein konkreter Bedarf entsteht.

## UI-Design (eigener Stil, angelehnt an Silo/Plex)

- **Farben und Schrift:** fast schwarze, neutrale Flächen (etwa #121214, #18181c, #202026), **eine** eigene Akzentfarbe, eine moderne Sans-Schrift, Radius 12px, weiche Schatten.
- **Navigation:** linke Sidebar (Home, Filme, Serien, Musik, Live TV, Für dich, Suche), einklappbar zur Icon-Leiste.
- **Home:**
  - Hero-Karussell über die volle Breite: Backdrop mit Verläufen nach unten und links, Titel-Logo, Metazeile, Format-Badges (4K, HDR, DV, Atmos).
  - Darunter Reihen: Weiterschauen mit Fortschritt, Nächste Folge, Neu hinzugefügt, KI-Reihen.
  - Nutzer können Reihen anordnen und ausblenden.
- **Bibliothek:** virtualisiertes Poster-Grid mit Sortieren, Filtern, Shuffle und A–Z-Leiste.
- **Detailseite:** Backdrop, Auswahl für Version, Audio und Untertitel, Play-Button, Cast-Reihe mit runden Avataren, Staffel-Tabs mit Episodenkarten, „Mehr wie das“.
- **Player:** eigenes Overlay mit Trickplay-Vorschau beim Hovern, Intro/Credits-Skip, Autoplay der nächsten Episode und einem Einstellungs-Sheet für Qualität, Audio, Untertitel und Tempo.
- **Live TV:** Guide-Grid, virtualisiert in beide Richtungen.

## Roadmap

Größen: S ≈ 1–2 Sessions, M ≈ 3–5, L ≈ 6–10, XL > 10. Pro Claude-Code-Session ist **ein Meilenstein** dran. Abgehakt in `docs/roadmap.md` wird erst, wenn das „Fertig wenn“-Kriterium erfüllt ist.

**Phase 0 – Grundgerüst (M)**
- **Voraussetzung:** rustup + MSVC Build Tools installieren (Cargo fehlt aktuell) und jellyfin-ffmpeg 7 win64 portable bereitstellen. Node, Docker und ffmpeg 8.1 sind bereits vorhanden.
- **0.1** Workspace, axum, tracing, Config, `GET /api/v1/system/info`. *Fertig wenn:* `cargo test` und `clippy -D warnings` grün sind.
- **0.2** flixnet-db: Migrationen, WAL, Pools.
- **0.3** OpenAPI und Scalar-UI, `flixnet-server openapi > web/openapi.json`.
- **0.4** Web-Shell (Sidebar-Layout, Theme, generierter Client), per rust-embed ausgeliefert. *Fertig wenn:* der Playwright-Smoke-Test grün ist.
- **0.5** `xtask fixtures` (Testvideos per lavfi), Dockerfile, GitHub Actions (amd64 + arm64 nach GHCR), `CLAUDE.md`. *Fertig wenn:* der Container auf Unraid läuft.

**Phase 1 – Auth und Nutzer (S–M)**
- Setup-Assistent, Login über Device-Token, Profile mit PIN („Wer schaut?“), Bibliotheksrechte, Einladungen.
- *Fertig wenn:* ein Test alle OpenAPI-Operationen durchgeht und ohne Token überall 401 bekommt.

**Phase 2 – Scannen und Metadaten (L, Kernrisiko)**
- Dateinamen-Parser mit 300+ Testfällen (German.DL, Staffel, S01E01-E02, `{tmdb-…}`)
- Job-Runtime; FS-Diff → ffprobe; NFO und lokale Artworks
- TMDB (de-DE mit Fallback en-US) mit Confidence-Score und manuellem „Identifizieren“; Bild-Cache
- Items-API mit Filter, Sortierung, Paging und FTS5. *Fertig wenn:* p95 unter 50 ms bei 50k Items.
- Watcher und Rename-Erkennung. *Fertig wenn:* nach einem Rename bleibt der Watch-State erhalten.
- TVDB, fanart.tv, Personen

**Phase 3 – Browsing-UI (M)**
- Home mit Hero und Reihen, Bibliotheks-Grid, Detailseiten für Film, Serie und Staffel, Suche.

**Phase 4 – Wiedergabe (XL, höchstes Risiko)**
1. Direct Play mit Range-Requests.
2. DeviceProfile und Decision-Engine.
3. HLS-Transcode in Software. *Fertig wenn:* HEVC 10-Bit läuft in Firefox, ein Seek auf 80 % startet in unter 3 s, und nach dem Schließen läuft kein ffmpeg-Prozess mehr.
4. Remux und Audio-Transcode.
5. QSV/VAAPI und HDR→SDR-Tonemapping. *Fertig wenn:* auf Unraid läuft 4K HDR schneller als 2× Echtzeit (prüfen mit intel_gpu_top).
6. Untertitel (WebVTT, PGS eingebrannt).
7. Fortschritt, Weiterschauen, Nächste Folge, Sessions im Dashboard.
8. Eigener Player mit Trickplay und Kapitelbildern.

**Phase 5 – Analyse und Organisation (M)**
- Intro/Credits-Erkennung per Chromaprint über die Episoden einer Staffel, ergänzt durch black/silence detect, plus Skip-Buttons. *Fertig wenn:* das Intro im synthetischen Fixture auf ±1 s genau erkannt wird.
- OpenSubtitles
- Collections (auch automatisch aus TMDB), Playlists, Smart Playlists
- Jugendschutz über FSK/MPAA

**Phase 6 – KI (L)**
- Model-Manager: lädt Modelle beim ersten Bedarf nach `/config/models` und prüft sha256.
- Item-Embeddings. *Fertig wenn:* ein Golden-Set aus 50 deutschen und englischen Queries Recall@5 ≥ 0.8 erreicht.
- Hybrid-Suche: BM25 plus Vektor, kombiniert per RRF.
- Recommendations: Taste-Cluster mit Zeit-Decay, Co-Watch, MMR, Onboarding, dazu ein Offline-Eval.
- LLM (optional):
  - natürliche Sprache → Filter-DSL per JSON-Schema-Tool-Call, ohne LLM Fallback auf die semantische Suche
  - Erklärungen auf Basis der eigenen History
- Szenensuche über Untertitel-Chunks; Whisper als Opt-in-Job pro Bibliothek.
- Gruppenmodus.
- Chat-Assistent mit Tools, spoilerfreie Recaps (nur Chunks vor der aktuellen Position), automatische Themen-Collections. Später Keyframe-Suche mit SigLIP und OpenVINO.

**Phase 7 – Live TV und DVR (L)**
- Tuner-Trait, M3U/Xtream → HLS. Getestet wird gegen eine lokale Fake-IPTV-Quelle per ffmpeg.
- HDHomeRun: Discovery und Lineup, gegen ein Fake-HDHR getestet.
- XMLTV als Streaming-Parser mit Fuzzy-Matching auf Sender und einer UI zum manuellen Zuordnen; Guide-Grid.
- Ein gemeinsamer Upstream pro Sender mit rollierendem HLS-Fenster (Timeshift/Pause) und Tuner-Zählung.
- DVR: Einzel- und Serienregeln, Vor- und Nachlauf, Konflikterkennung, neustartsichere Aufnahmen, die als Bibliotheks-Items erscheinen.
- Schedules Direct, tvheadend.

**Phase 8 – Jellyfin-Kompatibilität (L)**
- Eigener Listener auf Port 8098: Auth, Views/Items, PlaybackInfo, Stream/master.m3u8, Sessions-Reporting, Resume/NextUp, Bilder.
- Snapshot-Tests gegen einen echten Jellyfin-Container.
- *Fertig wenn:* Infuse, Swiftfin, Findroid und Streamyfin browsen, abspielen und den Fortschritt zurückmelden.

**Phase 9 – Breite und Feinschliff (L)**
- Musik: MusicBrainz und Cover Art Archive, gapless, LRC-Lyrics, später Sonic Radio per Audio-Embeddings
- SyncPlay/Watch Together, Chromecast-Sender
- Trakt, Webhooks, Benachrichtigungen (ntfy/Discord/SMTP)
- Dashboard: Bandbreite, Logs, Tasks
- Backups per `VACUUM INTO`, Import der History aus Plex/Jellyfin
- OIDC, Unraid-CA-Template
- Hörbücher, Fotos, DLNA

**Später – native Apps**
- iOS/tvOS in Swift (AVPlayer) und Android/TV in Kotlin (Media3), jeweils mit Clients aus dem OpenAPI-Codegen.

## Deployment (Unraid)

- **Image:** Multi-Stage-Docker. Gebaut wird mit `rust:slim`, die Runtime ist Debian slim mit jellyfin-ffmpeg 7, `intel-media-va-driver-non-free` und mesa.
- **Pfade:**
  - `/config` → `/mnt/cache/appdata/flixnet`. Direkter Cache-Pfad, **nicht** `/mnt/user`, denn SQLite auf shfs ist riskant.
  - `/media:ro`
  - `/transcode` → RAM/tmpfs
- **Container-Einstellungen:** `--device=/dev/dri`, PUID 99, PGID 100, GID der render-Gruppe. Host-Netzwerk ist optional, für HDHomeRun-Discovery und DLNA.
- **API-Keys:** TMDB, TVDB und OpenSubtitles trägt der Nutzer selbst in den Einstellungen ein.

## Verifikation

- **Unit-Tests (insta-Snapshots):** Parser, Decision-Engine, Playlist-Mathematik, ffmpeg-argv, EPG-Matcher, DVR-Konflikte, Recommendation-Mathematik.
- **Integrationstests:** `tower::oneshot` gegen eine Temp-DB; wiremock mit aufgezeichneten TMDB/TVDB-Antworten. Tests, die ffmpeg brauchen, laufen hinter dem Feature `media-tests`.
- **Fixtures (`cargo xtask fixtures`):**
  - Testsrc in h264/aac, HEVC Main10 HDR10, AV1, VP9
  - Multi-Audio deu/eng, srt/ass, Kapitel, interlaced TS
  - eine Staffel mit gemeinsamem Intro
  - echte deutsche Release-Namen
- **E2E:** Playwright gegen das Release-Binary (Setup → Login → Browse → Play → Seek → Resume); `xtask seed --items 50000` für Benchmarks.
- **Manuell:** QSV-Checkliste auf Unraid (intel_gpu_top, Diagnoseseite); jede Phase endet mit einem Deploy auf Unraid.
- **CI:** fmt, clippy, test, Web-Build, oasdiff gegen API-Brüche, cargo audit/deny, Multi-Arch-Images.

## Top-Risiken

1. **Transcoding und Client-Kompatibilität.** Gegenmittel: reine Decision-Engine mit Fixture-Matrix, erst Software-x264, dann Hardware. Ziel sind Chrome, Firefox und Safari.
2. **Remux-Segmente sitzen nicht auf Keyframes.** Gegenmittel: Keyframe-Index, sonst eine von ffmpeg erzeugte Playlist.
3. **QSV in Docker** (Treiber, render-GID). Gegenmittel: Selbsttest, Diagnoseseite, automatischer Software-Fallback.
4. **SQLite-Contention.** Gegenmittel: ein Writer, kurze Batches, I/O außerhalb der Transaktionen.
5. **Metadaten-Fehlmatches.** Gegenmittel: Confidence-Score, Review-Queue, NFO und ID-Hinweise im Pfad, gesperrte Felder.
6. **Scope-Drift beim Vibecoden.** Gegenmittel: harte „Fertig wenn“-Kriterien, Roadmap im Repo, OpenAPI-Diff in der CI.
7. **KI-Last kollidiert mit der Wiedergabe.** Gegenmittel: der ai-Pool läuft mit 1 Job und pausiert beim Transkodieren; int8-Modelle; Whisper nur als Opt-in.
