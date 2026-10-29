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
