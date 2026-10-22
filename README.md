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

## Phase 2 : mode live (entrée audio + horloge de tempo)

Seuls les vrais kicks, détectés dans les basses, déclenchent un flash : les breaks et les temps
creux restent sombres. Les kicks alimentent aussi une horloge de tempo (BPM stable estimé sur
les 16 dernières secondes, ou saisi, et phase calée sur les kicks). Quand les kicks tombent sur
deux temps de suite, le flash du suivant part en avance de la latence calibrée pour s'allumer
avec lui ; s'il ne vient pas (début de break), ce flash est aussitôt annulé. Les kicks hors
temps (roulements de montée, retour après un break) flashent immédiatement, et ne faussent ni
le tempo ni la phase. La lueur suit les basses et la teinte glisse du rouge (son grave) vers
le bleu (son brillant).

1. Lister les entrées audio et MIDI (→ : celle utilisée par défaut, la Focusrite si elle est
   branchée) : `.\target\release\dj-bridge.exe devices`. Avec `--test`, l'entrée est ouverte 3 s
   (sans le bridge) pour vérifier qu'elle reçoit du son et lire l'âge de ses blocs audio.
2. Lancer le mode live : `.\target\release\dj-bridge.exe live`
   - `--bpm 155` : démarrer avec un tempo imposé (sinon détection automatique).
   - `--bpm-min` / `--bpm-max` : plage de la détection (100-190 par défaut).
   - `--flash kicks|reactif|temps` : vrais kicks anticipés (par défaut), chaque kick sans
     anticipation (aucun flash annulé, mais ~55 ms de retard), ou chaque temps de l'horloge.
   - `--input "nom partiel"` : une autre entrée audio.
   - `--audio auto|asio|wasapi` : pilote de capture. Par défaut, ASIO (« Focusrite USB ASIO »)
     s'il est libre, sinon WASAPI ; un pilote ASIO n'accepte souvent qu'un logiciel à la fois,
     d'où le repli quand Ableton l'utilise déjà. En ASIO, la latence d'entrée dépend de la taille
     de tampon réglée dans Focusrite Control : 64 ou 128 échantillons pour la plus faible.
3. Touches en direct :
   - `b` puis chiffres et Entrée : saisir le BPM ; `+` / `-` : BPM ±0,5 ; `a` : BPM automatique.
   - `m` : mode de flash, kicks → réactif → temps.
   - ↑/↓ : seuil de détection des kicks plus haut (moins de kicks) / plus bas (plus de kicks).
   - `r` : enregistre les 40 dernières secondes de l'entrée dans
     `%APPDATA%\dj-bridge\captures\` (à faire juste après un passage qui décroche).
4. Ligne d'état : B/M/A = basses, médiums, aigus ; ● = kick détecté ; ♩ = tempo (auto ou
   man.) ; mode de flash ; ±n ms = écart moyen entre kicks et temps de l'horloge (n ms en
   mode réactif = réaction côté PC) ; ⚠n = coupures audio signalées par le pilote.

Si les barres restent vides : vérifier le gain d'entrée de la 2i2 et, dans Windows,
Paramètres > Confidentialité > Microphone > « Autoriser les applications de bureau ».

Une fois le groove établi (kicks sur 4 temps de suite), la détection attend les kicks sur les
temps : près d'un temps, ses seuils se relâchent (kick noyé dans une basse dense) ; entre les
temps, ils se durcissent (une note de basse ne passe pas pour un kick). Dès qu'un kick manque,
le groove tombe et les seuils redeviennent normaux, pour les breaks et les montées.

### Analyser un passage

`.\target\release\dj-bridge.exe analyze <capture.wav>` rejoue l'enregistrement dans la même
chaîne que le mode live et affiche, par tranches de 4 s : tempo, part du temps en groove, temps
marqués par un kick, kicks hors temps et flashs (anticipés / réactifs / annulés). Les tranches où
moins de la moitié des temps ont un kick sont signalées par ⚠. Options : `--bpm`, `--threshold`,
`--slice` (durée des tranches).

## Phase 3 : mode synthé (Ableton Link + MIDI)

- `live --link` rejoint la session Ableton Link du réseau local (Live, Move, Push…) : le tempo
  et la phase des temps viennent de la session, exacts, sans détection. Les flashs anticipés
  tombent donc pile sur les temps de la session ; avec `--flash temps`, la lampe bat chaque temps
  même sans kick (synthé seul). Les touches de tempo sont alors sans effet : le tempo se change
  depuis la session. `--bpm` ne sert que si l'on crée la session.
- `live --midi` (ou `--midi "nom partiel"`, voir `devices`) écoute les notes du synthé : chaque
  note-on flashe à sa couleur, plus fort quand la vélocité est élevée, et la lueur garde la
  couleur de la dernière note. Les 12 notes suivent le cercle des quintes (do rouge, sol orange,
  ré jaune…), pour que des notes harmoniquement proches aient des couleurs voisines.
- Les deux se combinent avec l'entrée audio : `live --link --midi`.

Au premier lancement avec `--link`, Windows demande d'autoriser l'accès réseau de
`dj-bridge.exe` : il faut accepter (réseau privé) pour voir les autres participants.

La compilation utilise le CMake des Build Tools (Ableton Link), le libclang de LLVM et le kit
ASIO de Steinberg (`C:\Dev\ASIOSDK`, double licence GPLv3 / accord Steinberg) ; leurs chemins
sur ce PC sont dans `.cargo/config.toml`.

Les identifiants du bridge et l'avance calibrée sont stockés dans
`%APPDATA%\dj-bridge\config.json`, hors du dépôt.

## Feuille de route

1. Sortie Hue + mesure de latence ✅
2. Entrée audio : Focusrite 2i2 (WASAPI ✅, ASIO ✅), banc de filtres, détection de kick ✅
3. Mode synthé live : Ableton Link + MIDI, flashs anticipés de la latence mesurée ✅ (à tester)
4. Mode DJ : détection de tempo sur l'audio + anticipation ; bonus : réseau de l'OPUS-QUAD
5. Modes d'effets, palettes, interface `egui`

## Licence

Ableton Link est distribué sous GPLv2 (ou licence commerciale d'Ableton). DJ-Bridge l'intègre :
l'usage personnel ne pose aucun problème, mais distribuer le programme imposerait de le publier
sous une licence compatible GPL, ou d'obtenir une licence auprès d'Ableton.
