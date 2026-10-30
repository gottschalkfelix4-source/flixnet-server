# Jellyfin-Clients mit Flixnet verbinden

Flixnet bringt eine Jellyfin-kompatible API mit. Damit funktionieren vorhandene
Jellyfin-Apps (Infuse, Swiftfin, Findroid, Streamyfin, Jellyfin für Android TV) schon
vor den eigenen Flixnet-Apps: anmelden, Bibliotheken durchsuchen, abspielen (direkt
oder per HLS-Transcode) und den Fortschritt zurückmelden.

Die Schicht läuft auf einem **eigenen Port (8098)** und ist **standardmäßig aus**. Ein
parallel laufendes Jellyfin (8096) stört sie nicht.

## Einschalten

`flixnet.toml`:

```toml
[jellyfin]
enabled = true
bind = "0.0.0.0:8098"     # Standard
# server_name = "Flixnet" # Name, den die Apps anzeigen
```

oder per Umgebungsvariable `FLIXNET_JELLYFIN__ENABLED=true` (Unraid-Template:
„Jellyfin-API“ = `true`, „Jellyfin-Port“ unter „Erweitert“). Im Log steht danach
`Jellyfin-compatible API is listening`.

## Client einrichten

1. In der App einen Jellyfin-Server hinzufügen: `http://<server>:8098`
   (Infuse: „Jellyfin“ als Freigabetyp wählen).
2. Mit **Flixnet-Name und -Passwort** anmelden. Profile ohne Passwort (nur per
   Profilwechsel erreichbar) können sich in Jellyfin-Apps nicht anmelden; Quick
   Connect gibt es nicht, und die öffentliche Nutzerliste ist leer.
3. Jede App bekommt ein eigenes Flixnet-Geräte-Token; es erscheint in Flixnet unter den
   Geräten des Nutzers und lässt sich dort widerrufen.

Es gelten dieselben Rechte wie in Flixnet: freigegebene Bibliotheken, Altersgrenze,
Watch-State am `content_key`. Was in einer Jellyfin-App gesehen wird, erscheint in
Flixnet unter „Weiterschauen“ und „Nächste Folge“ und im Sessions-Dashboard.

## Was unterstützt wird

- Anmeldung (`Users/AuthenticateByName`), Token über `Authorization: MediaBrowser …`,
  `X-Emby-Authorization`, `X-Emby-Token`, `X-MediaBrowser-Token` oder `api_key`.
- Bibliotheken als Views, Filme, Serien, Staffeln, Folgen; Filter, Sortierung,
  Paging, Suche, Genres, Personen, „Neu hinzugefügt“, ähnliche Titel.
- `PlaybackInfo` mit dem DeviceProfile der App: Direct Play (Range-Requests) oder
  HLS (fMP4) über Flixnets Transcode-Sitzungen, Untertitel als WebVTT/SRT.
- Fortschritt (`Sessions/Playing…`), gesehen/ungesehen, Favoriten, Weiterschauen,
  Nächste Folge, Bilder mit Größenanpassung.

Die vollständige Endpoint-Liste steht in der Moduldoku von `crates/flixnet-jellyfin`
(`src/lib.rs`). Die Schicht ist bewusst **nicht** Teil von Flixnets OpenAPI.

## Was (noch) nicht geht

- Die offiziellen Jellyfin-Apps für Android/iOS (nicht TV), die die Web-Oberfläche
  des Servers laden: Flixnet liefert kein jellyfin-web aus.
- Musik, Live TV, Sammlungen/Playlists über die Jellyfin-API, SyncPlay,
  Fernsteuerung anderer Geräte, WebSocket-Benachrichtigungen, Trickplay, Quick
  Connect, Nutzer-Avatare, Bearbeiten von Metadaten.
- Ton wird nicht einzeln transkodiert (`Audio/{id}/universal` liefert die Datei).

## Sicherheit

- Ohne Token erreichbar sind nur Server-Info, Ping, Branding, Quick-Connect-Status,
  die (leere) öffentliche Nutzerliste und die Anmeldung.
- Bilder laden die Apps ohne Zugangsdaten; der `tag` in der URL ist eine Signatur
  über das Item und erlaubt nur dessen Bilder. Ohne gültigen Tag braucht es ein Token.
- Direkt-Streams und Untertitel: signierte URLs aus `PlaybackInfo` oder ein Token.
  HLS-Playlists und -Segmente: die 128-Bit-Sitzungs-ID im Pfad.

## Abgleich mit einem echten Jellyfin (Oktober 2026)

Verglichen mit `jellyfin/jellyfin:12.4.20261026-211555` (Jellyfin 12.4.0) in Docker,
beide Server auf derselben Bibliothek (vier Filme mit/ohne NFO-IDs, eine Serie mit
drei Folgen) und mit demselben Watch-State. Geprüft wurde jede Antwort beider Server
gegen Jellyfins eigenes OpenAPI-Dokument (`/api-docs/openapi.json`, Pflichtfelder,
Typen, `nullable`, Enums, UUID-/Datumsformate – das, woran das Kotlin-SDK von
Findroid und das Swift-SDK von Swiftfin beim Deserialisieren scheitern würden),
dazu ein Feld-für-Feld-Vergleich zusammengehöriger Objekte und ein Durchlauf mit dem
offiziellen TypeScript-SDK (`@jellyfin/sdk` 1.0.2: Discovery, Anmeldung, Views,
Items, Staffeln/Folgen, Latest, Resume, NextUp, PlaybackInfo, Stream, Bild,
MediaSegments, Playstate, Favoriten).

**Stimmt überein (0 Schemafehler):** `System/Info/Public`, `System/Info`,
`Users/AuthenticateByName`, `Users/Me`, `Users/{id}`, `UserViews` (+ `Users/{id}/Views`),
`Items` mit ParentId/IncludeItemTypes/Recursive/Fields/SortBy/SortOrder/StartIndex/
Limit/Filters/SearchTerm/Ids (+ `Users/{id}/Items`), `Items/{id}`
(+ `Users/{uid}/Items/{id}`), `Shows/{id}/Seasons`, `Shows/{id}/Episodes` (auch mit
`SeasonId`, `StartItemId`), `Items/Latest`, `Shows/NextUp`, `UserItems/Resume`,
`Items/{id}/PlaybackInfo` (GET und POST mit Findroids Profil und einem typischen
Mobil-Profil), `Items/{id}/Similar|Ancestors|Intros|SpecialFeatures|LocalTrailers`,
`Genres`, `Persons`, `Search/Hints`, `DisplayPreferences`, `UserItems/{id}/UserData`,
`MediaSegments/{id}`. Direkt-Streams verhalten sich wie bei Jellyfin (206 mit
`Content-Range` für `bytes=a-b`, `bytes=-n` und offene Ranges, `HEAD`,
`Accept-Ranges`, `Last-Modified`), ebenso Bilder mit `tag`, `maxWidth`,
`fillWidth`/`fillHeight` und `Sessions/Playing|Progress|Stopped` (204).

**Behoben nach dem Abgleich:**

- Findroid schickt ein Profil ohne Direct-Play- und ohne Transcoding-Profile
  („Direct play all“) und spielt danach immer die Datei selbst. Flixnet hat dafür
  jedes Mal eine Transcode-Sitzung gestartet; jetzt gilt so ein Profil als „spielt
  alles“ (Direct Play). Ein Profil ganz ohne Video-Transcoding-Profil bekommt – wie
  von Jellyfin – keine Transcode-URL mehr (`SupportsTranscoding=false`).
- `Shows/NextUp` ließ eine angefangene nächste Folge weg; Jellyfin zeigt sie
  (`EnableResumable`, Standard `true`), jetzt auch Flixnet.
- `UserData`: `PlayCount` war nur 0/1 und `LastPlayedDate` fehlte in Listen; Serien
  und Staffeln hatten kein `UnplayedItemCount`/`PlayedPercentage` (Findroid und
  Swiftfin zeigen damit die „ungesehen“-Zahl) und kein `RecursiveItemCount`.
- `MediaSegments/{id}` fehlte (404): Intro/Abspann/Rückblick aus Flixnets
  Segmenterkennung kommen jetzt als `Intro`/`Outro`/`Recap` (Intro überspringen).
- `POST Devices/Options` (Findroid ändert damit den Gerätenamen, ohne Fehler
  abzufangen), `Items/{id}/ThemeMedia` (leer), `Items/Filters` und `Items/Filters2`
  (Genres; Swiftfins Filterauswahl) fehlten.
- HLS: Unter Windows landete das Init-Segment außerhalb des Sitzungsordners, sobald
  der Konfig-Pfad `/` und `\` mischte (z. B. `FLIXNET_PATHS__CONFIG_DIR=D:/flixnet`);
  jede Transcode-Wiedergabe lief dann nach 30 s in einen Fehler. Betraf auch Flixnets
  eigene Web-Wiedergabe, nicht Docker/Linux.

**Bewusst anders:**

- Version: Flixnet meldet `10.11.0` (die API-Generation dieser Schicht), Jellyfin
  `12.4.0`. Das TypeScript-SDK stuft das als „veraltet, aber unterstützt“ ein
  (Mindestversion 10.10.0).
- HLS ist immer fMP4 (`TranscodingContainer=mp4`, `#EXT-X-MAP`, 6-s-Segmente), auch
  wenn das Profil `ts` verlangt; Jellyfin liefert dann MPEG-TS. Der Master-Playlist
  fehlen `CODECS`/`FRAME-RATE`.
- Mindestposition für „Weiterschauen“ ist Flixnets (2 min / 5 %, gesehen ab 90 %),
  nicht Jellyfins (5 %, 90 %, Titel unter 5 min gelten nach dem Stopp als gesehen).
- `Items/Latest` für Serienbibliotheken liefert die Serie, Jellyfin bei genau einer
  neuen Folge die Folge selbst.
- Bilder ohne gültigen `tag` brauchen ein Token (Jellyfin liefert sie offen aus);
  Image-Tags sind länger als 32 Zeichen (Signatur + Bild-ID). Blurhashes nur, wenn
  Flixnet einen berechnet hat.
- `Sessions` listet nur die eigene Sitzung ohne `NowPlayingItem`; laufende
  Wiedergaben stehen in Flixnets Sessions-Dashboard. `Ancestors` endet beim View,
  ohne Jellyfins „Media Folders“-Wurzel.
- Viele optionale Felder fehlen (`ChannelId`, `LockData`, `PlayAccess`, `Tags`,
  `Trickplay`, `AirDays`, `ParentPrimaryImage*`, Stream-Details wie `RefFrames`,
  `TimeBase`, `IsAVC`); keines ist im Schema Pflicht. Flixnet liefert umgekehrt ein
  paar zusätzliche (`CanDelete`, `Etag`, `DisplayPreferencesId`, …).
- Nicht vorhanden (404, Clients fangen das ab): `Studios`, `Items/Counts`,
  `Items/{id}/Images` (Liste), Trickplay, `Localization/*`, `System/Configuration`,
  `Playlists`, `Web/*`.

**Noch nicht mit den echten Apps geprüft:** Findroid (Kotlin-SDK), Swiftfin
(Swift-SDK), Infuse, Streamyfin und Jellyfin für Android TV selbst – insbesondere
Wiedergabe-Details der nativen Player (HLS-fMP4, Untertitel-Auswahl, Seek in
HLS), Swiftfins eigene Profil-Erzeugung und Infuses Bibliotheks-Sync.
