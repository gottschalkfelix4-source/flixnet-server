# Flixnet auf Unraid

Flixnet läuft auf Unraid als Docker-Container (`ghcr.io/gottschalkfelix4-source/flixnet-server`).
Die Vorlage liegt in [`docker/unraid-template.xml`](../docker/unraid-template.xml).

## Installation

**Über die Vorlage (empfohlen):**
1. Unraid → *Docker* → *Add Container* → bei *Template* die URL
   `https://raw.githubusercontent.com/gottschalkfelix4-source/flixnet-server/main/docker/unraid-template.xml`
   eintragen (oder die Datei nach `/boot/config/plugins/dockerMan/templates-user/my-Flixnet.xml` kopieren).
2. Pfade prüfen:

   | Container | Host (Vorschlag) | Hinweis |
   |---|---|---|
   | `/config` | `/mnt/cache/appdata/flixnet` | **direkt auf dem Cache-Pool**, nicht `/mnt/user/…` – SQLite auf shfs führt zu Lock-Fehlern |
   | `/media` | `/mnt/user/` | nur lesend (`ro,slave`) |
   | `/transcode` | `/tmp/flixnet-transcode` | liegt im RAM, schont die Platten |
   | `/backups` | `/mnt/user/backups/flixnet` | optional, Ziel der Sicherungen (siehe unten) |
   | `/dev/dri` | `/dev/dri` | Intel QuickSync / VAAPI |

3. Port: Web-Oberfläche auf Container-Port 8097 (Host-Vorschlag 8099). Die Jellyfin-kompatible API (8098)
   ist optional und mit `FLIXNET_JELLYFIN__ENABLED=true` einzuschalten.
4. Container starten, `http://<server>:8099/` öffnen und im Assistenten das Admin-Konto anlegen.

**Per Kommandozeile (gleichwertig):**

```sh
docker run -d --name flixnet \
  -p 8099:8097 \
  -v /mnt/cache/appdata/flixnet:/config \
  -v /mnt/user/:/media:ro,slave \
  -v /tmp/flixnet-transcode:/transcode \
  -v /mnt/user/backups/flixnet:/backups \
  --device /dev/dri \
  -e PUID=99 -e PGID=100 \
  ghcr.io/gottschalkfelix4-source/flixnet-server:latest
```

## Intel QuickSync (QSV)

- `/dev/dri` an den Container durchreichen. Der Entrypoint ermittelt die Gruppen von `renderD*`/`card*`
  selbst und gibt sie dem Flixnet-Prozess (PUID/PGID bleiben 99/100) – eine render-GID muss nicht
  eingetragen werden.
- Beim Start testet Flixnet jeden Encoder mit einem einzelnen Frame; das Ergebnis steht unter
  *Einstellungen → Transcoding*. Ohne funktionierende GPU wird automatisch in Software transkodiert.
- Prüfen während einer Transcode-Wiedergabe: auf dem Host `intel_gpu_top` (Plugin *Intel GPU TOP*);
  *Render/3D* und *Video* sollten Last zeigen. Unter *Einstellungen → Übersicht* steht die
  Bandbreite pro Wiedergabe, unter *Wiedergaben* die Methode (Direct Play / Remux / Transcode).
- Erzwingen oder abschalten: `FLIXNET_MEDIA__HARDWARE_ACCELERATION=qsv|vaapi|none` (Standard `auto`).

## Update

1. *Docker* → Flixnet → *Check for Updates* / *Apply Update* (oder `docker pull` + Container neu erstellen).
2. Datenbank-Migrationen laufen beim Start automatisch. Vor großen Updates vorher unter
   *Einstellungen → Sicherung* auf **Jetzt sichern** klicken.
3. Die API bleibt unter `/api/v1` stabil; Apps müssen nach einem Update nicht neu angemeldet werden.

## Sicherung und Wiederherstellung

- Flixnet sichert täglich (geplante Aufgabe *Datenbank und Konfiguration sichern*) per `VACUUM INTO`
  einen konsistenten Schnappschuss der Datenbank – im laufenden Betrieb, ohne Sperren – plus
  `flixnet.toml`. Standardordner ist `/config/backups`; die letzten 7 bleiben erhalten.
- Ordner und Anzahl unter *Einstellungen → Sicherung* ändern. Für Sicherungen außerhalb des
  Cache-Pools `/backups` mappen und dort als Ordner `/backups` eintragen.
- Gesichert werden Nutzer, Gesehen-Status, Bewertungen, Einstellungen (inkl. API-Schlüssel und
  Tokens – die Dateien also wie Passwörter behandeln). Bild-Cache, Vorschaubilder und Medien
  werden nicht gesichert; sie entstehen neu.
- Zusätzlich empfiehlt sich das Plugin *Appdata Backup* für den ganzen `/config`-Ordner.

**Wiederherstellen** (Server muss gestoppt sein):

```sh
docker stop flixnet
docker run --rm -v /mnt/cache/appdata/flixnet:/config -v /mnt/user/backups/flixnet:/backups \
  ghcr.io/gottschalkfelix4-source/flixnet-server:latest \
  flixnet-server restore /backups/flixnet-backup-20261030-030000-123.db --with-config
docker start flixnet
```

`restore` prüft die Sicherung (Integritätscheck), legt die bisherige Datenbank als
`flixnet.db.before-restore-<zeit>` daneben und spielt fehlende Migrationen ein. `--with-config`
stellt auch `flixnet.toml` wieder her. Außerhalb von Docker: `flixnet-server restore <datei>`
mit derselben Konfiguration wie der Server (`--config` bzw. `FLIXNET_CONFIG`).

## Integrationen

- **Benachrichtigungen** (*Einstellungen → Benachrichtigungen*): Webhooks (JSON, optional
  HMAC-signiert: Header `X-Flixnet-Signature: t=<unix>,v1=<hex>` über `"<t>.<body>"`), ntfy,
  Discord, E-Mail über SMTP. Zustellungen laufen als Hintergrundaufgaben mit Wiederholung.
- **Trakt** (*Einstellungen → Trakt*): einmalig eine API-App auf trakt.tv anlegen (Redirect URI
  `urn:ietf:wg:oauth:2.0:oob`), Client ID/Secret eintragen; jedes Profil verbindet sich per Code.
- **Single Sign-on** (*Einstellungen → Anmeldung (SSO)*): beim Anbieter (Authelia, Authentik,
  Keycloak) einen OIDC-Client anlegen, Weiterleitungs-URL `http(s)://<flixnet>/login/oidc`.
  Hinter einem Reverse Proxy muss Flixnet unter derselben Adresse erreichbar sein, die im Browser steht.
- **Import aus Plex/Jellyfin** (*Einstellungen → Import*): erst Vorschau, dann importieren.

## Community Applications

Die Vorlage ist CA-kompatibel (Container-Version 2, `TemplateURL`, `Icon`, `Support`, `Project`,
Kategorien, `Changes`). Für die Aufnahme in Community Applications gilt:

1. Das Repository muss **öffentlich** sein (aktuell privat) – oder es gibt ein eigenes, öffentliches
   Template-Repository, z. B. `flixnet-unraid-templates` mit:
   ```
   flixnet-unraid-templates/
     flixnet.xml          # Kopie von docker/unraid-template.xml
     icon.png             # 512×512, per raw.githubusercontent.com erreichbar
     README.md
   ```
   `TemplateURL` und `Icon` zeigen dann auf dieses Repository.
2. Das Image auf GHCR muss öffentlich abrufbar sein (*Package settings → Change visibility*).
3. Ein Support-Thread im Unraid-Forum (Docker Containers) anlegen und als `<Support>` eintragen.
4. Repository über das CA-Formular einreichen (*Apps → Settings → Submit*).

XML-Prüfung vor jedem Commit: `python -c "import xml.dom.minidom; xml.dom.minidom.parse('docker/unraid-template.xml')"`.
