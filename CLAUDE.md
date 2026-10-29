# Flixnet – Hinweise für Claude

Selbst gehosteter Media Server (Rust-Backend, React-Web-UI), Ziel: Plex/Jellyfin-Funktionsumfang + lokale KI + Live TV.
Architektur: `docs/architecture.md`. Fortschritt: `docs/roadmap.md`.

## Arbeitsweise
- Ein Meilenstein aus `docs/roadmap.md` pro Session. Erst abhaken, wenn sein „Fertig wenn“ nachweislich erfüllt ist.
- Vor dem Abschluss: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; im Web `npm run lint` und `npm run build`.
- Nach API-Änderungen OpenAPI neu erzeugen (`cargo run -p flixnet-server -- openapi --output web/openapi.json`; nicht `>` nutzen, PowerShell 5.1 schreibt sonst UTF-16), mit `npx @redocly/cli@2 lint web/openapi.json` prüfen (0 Fehler) und den TS-Client regenerieren.
- Jede neue Route bekommt `#[utoipa::path]` mit eindeutiger `operation_id` (camelCase) und wird über `OpenApiRouter`/`routes!` registriert, nie über `axum::Router` direkt.
- Neue Crates nur, wenn eine Abhängigkeit es erzwingt.
- Git: Repo `gottschalkfelix4-source/flixnet-server` (privat). Abgeschlossene, grüne Arbeitsschritte direkt auf `main` committen und pushen; nichts committen, wenn Tests oder clippy rot sind.

## Dev-Loop
- Backend: `cargo run -p flixnet-server` (Port 8097, Daten in `dev-config/`). Debug-Builds lesen `web/dist` von der Platte.
- Frontend: in `web/` `npm run dev` (Vite auf 5173, Proxy für `/api`) bzw. `npm run build` für das eingebettete UI.
- E2E: `npm run build && npm run test:e2e` in `web/` (startet den Server selbst mit leerer Temp-DB auf Port 18097; anderer Port per `FLIXNET_E2E_PORT`).
- TypeScript bleibt auf 5.9, bis typescript-eslint und openapi-typescript TS 6/7 unterstützen.
- UI-Texte sind deutsch; Design-Tokens stehen in `web/src/styles/index.css`.

## Harte Regeln
- ffmpeg/ffprobe nur über den typisierten `FfmpegArgs`-Builder starten, nie über eine Shell, nie mit rohen Strings aus Requests.
- Keine Schreibtransaktion über externe I/O (HTTP, ffprobe, Dateisystem) hinweg halten. Erst berechnen, dann in Batches schreiben.
- Jede Route ist authentifiziert, außer einer expliziten Whitelist (system/info, Login, Setup).
- Nutzerdaten (Watch-State, Bewertungen) hängen am `content_key`, nicht an Item-ID oder Pfad.
- API ist versioniert unter `/api/v1`; keine Breaking Changes ohne neue Version.
- Zeiten als Unix-Millisekunden (UTC) in INTEGER-Spalten; Schema bleibt Postgres-portabel.

## Umgebung
- Dev: Windows 11, Port 8097. Produktion: Docker auf Unraid mit Intel QuickSync (`/dev/dri`); `/config` liegt auf `/mnt/cache/appdata/flixnet`, nicht auf `/mnt/user`.
