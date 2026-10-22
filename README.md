# DJ-Bridge

Lumières Philips Hue pilotées par le son (platines, synthé) et le tempo (Ableton Link),
avec la latence la plus faible possible. Écrit en Rust, sortie via l'Entertainment API de Hue
(flux UDP chiffré en DTLS, 60 paquets/s).

## Phase 1 : sortie Hue et mesure de latence

Prérequis : un bridge Hue carré (v2), une ampoule couleur, et une **zone de divertissement**
créée dans l'app Hue (Paramètres > Zones de divertissement) contenant l'ampoule.

```bash
cargo build --release
```

1. Associer le bridge (appuyer sur son bouton quand c'est demandé) :
   `cargo run --release -- pair` (ou `-- pair --ip 192.168.x.y`)
2. Vérifier la zone choisie : `cargo run --release -- areas`
3. Tester le flux (arc-en-ciel 10 s + régularité d'envoi) : `cargo run --release -- test`
4. Calibrer la latence : `cargo run --release -- latency`
   - Chaque seconde, un tic part dans les enceintes et un flash blanc vers la lampe, au même
     instant. La lampe ayant du retard, on entend le tic avant de voir le flash.
   - ↑ fait partir le flash plus tôt (+5 ms), ↓ plus tard ; →/← par pas de 1 ms. Monter jusqu'à
     ce que tic et flash tombent ensemble, puis Entrée pour enregistrer (Échap : quitter sans
     enregistrer).
   - Mesure précise : avance à 0 ms, filmer la lampe au ralenti (240 i/s) ; l'écart entre
     le tic et le flash sur la vidéo est la latence de la chaîne Hue (1 image = 4,2 ms).
   - Utiliser une sortie audio filaire (le Bluetooth ajoute une latence non rapportée).

Si `cargo` est introuvable (terminal ouvert avant l'installation de Rust), lancer directement
l'exécutable compilé : `.\target\release\dj-bridge.exe latency`, ou ouvrir un nouveau terminal.

Les identifiants du bridge et l'avance calibrée sont stockés dans
`%APPDATA%\dj-bridge\config.json`, hors du dépôt.

## Feuille de route

1. Sortie Hue + mesure de latence ✅
2. Entrée audio : Focusrite 2i2 (WASAPI puis ASIO), banc de filtres, détection de kick ← *prochaine étape*
3. Mode synthé live : Ableton Link + MIDI, flashs anticipés de la latence mesurée
4. Mode DJ : détection de tempo sur l'audio + anticipation ; bonus : réseau de l'OPUS-QUAD
5. Modes d'effets, palettes, interface `egui`
