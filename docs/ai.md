# KI in Flixnet (Phase 6: Embeddings, Suche, Empfehlungen, Gruppenmodus)

Code: `crates/flixnet-ai`, Routen in `crates/flixnet-api/src/ai.rs`, Migration `0018_ai.sql`.

## Embeddings

- **Modell:** multilingual-e5-base, int8-quantisierter ONNX-Export von Xenova (Revision `1ec9243`, `onnx/model_int8.onnx`, 278 MB) plus `tokenizer.json`. Deutsch und Englisch; Texte bekommen die e5-Präfixe `passage: ` bzw. `query: `, Mean-Pooling, L2-normiert, 768 Dimensionen.
- **ONNX Runtime:** Das Crate `ort` (2.0.0-rc.15) läuft mit `load-dynamic`: Beim Bauen wird nichts heruntergeladen oder gelinkt, die Bibliothek wird zur Laufzeit geladen. Der Model-Manager holt beim ersten Bedarf die offizielle CPU-Version 1.31.1 von Microsoft (linux-x64, linux-aarch64, win-x64) und entpackt nur die Bibliothek. Eigene Bibliothek: `ai.onnxruntime_path` oder `ORT_DYLIB_PATH`.
  - Dadurch braucht das Docker-Image (Debian trixie) **keine zusätzlichen Pakete** und das Dockerfile bleibt unverändert; Modell und Runtime landen in `/config/models` (etwa 300 MB).
  - Jede Datei ist auf einen SHA-256 festgelegt. Downloads laufen in `*.part`, werden geprüft und erst dann umbenannt; eine `*.sha256`-Markerdatei erspart das Neu-Hashen beim Start. Fortschritt über den Job `ai.download_model` (io-Pool).
- **Alternativen:** OpenAI-kompatibler Endpoint (`POST {base}/embeddings`; OpenAI, Ollama, LM Studio …) mit Adresse, Modell und Schlüssel in den Einstellungen; wird beim Speichern mit einer Probe-Anfrage getestet. „Aus“ schaltet auf reine Volltextsuche.
- **Nur für Tests:** `FLIXNET_AI__EMBEDDER=stub` (bzw. `[ai] embedder = "stub"`) erzwingt einen deterministischen Hashing-Embedder mit kleinem Begriffslexikon. Ihn nutzen die E2E-Tests und CI, damit nie ein Modell geladen wird.
- **Embedding-Text pro Film/Serie:** Titel (Originaltitel), Jahr, Typ, Genres, Tagline, Beschreibung, TMDB-Keywords (`item_keywords`, neu per `append_to_response=keywords`), die ersten sechs Darsteller und Regie/Creator. Gespeichert wird ein SHA-256 aus Modell und Text, nur Geändertes wird neu berechnet.

## Vektorspeicher: BLOB + Brute Force statt sqlite-vec

`item_embeddings` speichert pro (Modell, Item) einen f32-BLOB (little endian). Gesucht wird hinter dem Trait `VectorIndex` mit einem In-Memory-Brute-Force-Index (zusammenhängende f32, Skalarprodukt auf Einheitsvektoren): 20k Titel × 768 Dimensionen ≈ 60 MB RAM und wenige Millisekunden pro Suche, ausreichend bis etwa 100k Titel. Embeddings gibt es nur für Filme und Serien, nicht für Folgen.

sqlite-vec wurde verworfen. Es ist eine C-Erweiterung, die per unsafe FFI neben dem von sqlx gebündelten SQLite registriert werden müsste, und zwar auf Windows, Linux-Docker und CI. Der Workspace verbietet `unsafe`, und bei Bibliotheksgrößen bringt die Erweiterung keinen Vorteil. Das Schema bleibt Postgres-portabel: BLOB wird zu bytea, pgvector wäre später eine weitere `VectorIndex`-Implementierung.

## Jobs und Last

- `ai.embed_items` läuft im ai-Pool (1 Worker) mit Backfill-Priorität. Auslöser:
  - ein entprellter Listener auf „Metadaten-Refresh fertig“, „Scan fertig“ und `LibraryChanged` (60 s, beim Stub 2 s)
  - der stündliche Task `ai_embeddings`
  - der Admin-Button
- **Vorrang für die Wiedergabe:** Vor jedem Batch zu 16 Titeln wartet der Job, solange eine Transcode-Session ffmpeg laufen hat oder ein Live-TV-Stream aktiv ist (`playback_busy_probe`). ONNX nutzt höchstens 4 Threads (Hälfte der Kerne).
- Keine Schreibtransaktion über die Modellrechnung: erst rechnen, dann in einer kurzen Transaktion pro Batch schreiben.
- Beim ersten Bedarf (Embedding-Job und lokales Modell fehlt) wird der Download eingereiht, außer `auto_download` ist aus. Der Download reiht danach selbst einen Embedding-Lauf ein.

## Hybrid-Suche

`GET /items?search=` mit Relevanz-Sortierung (der Standard bei einer Suche):

1. FTS5/bm25 liefert die Top 200.
2. Das Vektor-Ranking liefert höchstens 40 Treffer, nur unter den für das Profil sichtbaren Filmen und Serien.
3. Beide Listen werden per Reciprocal Rank Fusion (k = 60) kombiniert.

Welche Vektortreffer mitzählen:

- Sie liegen höchstens eine Modell-Marge unter dem besten Treffer (e5: 0,06, gemessen auf dem Golden Set) und über einer Rauschgrenze (nur beim Stub).
- Hat die Volltextsuche etwas gefunden, muss ein zusätzlicher Vektortreffer inhaltlich besser passen als der beste Stichworttreffer. „Matrix“ zeigt Matrix und nicht zehn ähnliche Filme; „Film über Zeitreisen mit Dinosauriern“ bekommt trotzdem semantische Treffer.

Ohne Vektoren fällt die Suche auf reinen Volltext zurück.

## Empfehlungen (`/recommendations*`, „Für dich“)

- **Signale:** `user_item_data` und `watch_events`; Folgen zählen für ihre Serie, viele Folgen wachsen nur sublinear. Gewichte:
  - gesehen: 1, Wiederholungen bis +0,5
  - angefangen: 0,3 bis 0,8 je nach Anteil
  - Favorit: +1
  - Bewertung unter 4 hebt das Signal auf
  - Onboarding-Auswahl (`taste_seeds`): 1
  - Halbwertszeit: 180 Tage
- **Taste-Cluster:** gewichtetes sphärisches k-means (k-means++, deterministisch). k = round(√(n/2)), begrenzt auf 1 bis 5.
- **Score:** bester Cluster (schwere Cluster leicht bevorzugt) + 0,2 × Genre-Affinität + 0,12 × Co-Watch. Co-Watch heißt: Was Haushaltsprofile mit überlappender Historie gesehen haben, gewichtet mit Ochiai.
- Danach diversifiziert MMR mit λ = 0,7 über die besten 150. Gesehenes und Angefangenes fällt raus.
- **Erklärungen (regelbasiert):** „Weil du „X“ gesehen hast“, „… magst“ (Favorit oder Onboarding), „… angefangen hast“, „Im Haushalt beliebt“.
- **Startseite:** Reihen „Für dich“ und bis zu zwei „Weil du „X“ gesehen hast“. Auf der Detailseite „Mehr wie das“ über `GET /items/{id}/similar`, ohne KI wie bisher nach Genre.
- Ohne Historie zeigt „Für dich“ das Onboarding: gut bewertete Titel, per MMR über die Bibliothek gestreut.

## Gruppenmodus (`POST /recommendations/group`, Seite `/group`)

- Das aufrufende Profil ist immer dabei. Erlaubt sind sichtbare Haushaltsprofile, höchstens 8; versteckte oder unbekannte Profile geben 404.
- **Jugendschutz:** der strengste der Gruppe. Das heißt die niedrigste Altersgrenze, Unbewertetes nur, wenn alle Mitglieder mit Altersgrenze es erlauben, und die Vereinigung aller gesperrten Genres.
- **Bibliotheken:** nur die Schnittmenge der Mitglieder.
- Optional bleibt alles weg, was irgendein Mitglied gesehen oder angefangen hat.
- **Score:** 0,5 × Mittelwert + 0,5 × Minimum der min-max-normierten Mitgliederscores (Least Misery), danach MMR.
- Abgestimmt wird auf einem Gerät mit Daumen hoch/runter; die Liste sortiert sich nach Stimmen um, nichts wird gespeichert.

## Messwerte

- **Golden Set:** 50 Queries (25 de, 25 en) über 156 synthetische Titel (`crates/flixnet-ai/tests/fixtures`). Mit multilingual-e5-base int8 erreicht Recall@5 0,967 (nur Vektoren) und 0,960 (hybrid; de 0,92, en 1,00). Die 156 Titel sind in etwa 3 s eingebettet (Windows, Debug-Build).
  - Ausführen: `cargo test -p flixnet-ai --test golden -- --ignored --nocapture` (lädt das Modell nach `target/ai-models`).
- **Offline-Eval der Empfehlungen:** 7 Profile, 70/30-Hold-out. Recall@10 liegt bei 0,78 (e5) bzw. 0,78 (Stub); die Baselines erreichen 0,02 (Popularität) und 0,07 (Zufall).
  - Ausführen: `cargo test -p flixnet-ai --test recommend_eval -- --include-ignored --nocapture`.
- Die Profile der Eval sind über Genres definiert, das begünstigt die Genre-Affinität. Echte Werte zeigen sich erst mit echter Historie.
