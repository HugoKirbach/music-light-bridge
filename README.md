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
   - `--audio output` : capte ce que le PC joue (WASAPI loopback), sur la sortie par défaut ou
     celle choisie avec `--input` (haut-parleurs, sorties de la carte son). Dans la fenêtre :
     « Sortie du PC ». La latence de sortie n'étant pas connue, les flashs peuvent précéder le
     son de quelques ms (davantage en Bluetooth).
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

## Fenêtre et couleurs

`.\target\release\dj-bridge.exe gui` ouvre une fenêtre sans toucher aux lampes : l'écran
**Préparer la session**. L'en-tête dit si le bridge répond et quelle zone sera pilotée. Au
centre : la source audio (**Carte son**, avec l'appareil et le pilote, ou **Sortie du PC**), le
tempo (**Détection auto**, **BPM imposé** ou **Ableton Link**), le MIDI, puis le bouton **Lancer
le live**. La section **Lampes** liste les lampes de la zone de divertissement : celles qu'on
décoche sont laissées hors du flux et gardent leur état pendant le live (le choix vaut aussi pour
`live` dans le terminal). Seules les lampes ajoutées à la zone dans l'app Hue (Paramètres > Zones
de divertissement) apparaissent ; si la zone change dans l'app Hue, le bouton **Mettre à jour**
de la section la relit sans fermer la fenêtre. À droite : un aperçu de la couleur principale, l'animation, les couleurs, l'allure des
flashs, leur déclenchement (kicks anticipés, réactif, chaque temps) et le seuil des kicks. Les
réglages sont enregistrés et repris à l'ouverture suivante ; après l'arrêt, le bilan de la
session s'affiche sous le bouton.

Pendant le live, la fenêtre devient une scène : le fond prend la couleur envoyée à la lampe, la
phase reconnue s'affiche en haut, le tempo en très grand au centre (mis à jour chaque seconde,
avec sa source) et quatre points marquent les temps de la mesure (le premier temps n'étant pas
repéré, la mesure peut commencer sur n'importe lequel). En haut à droite, l'entrée captée et le
bouton rond qui arrête le live (lampes rétablies). Le dock du bas garde tout réglable en onglets :
**Animation**, **Couleurs**, **Flashs** (allure et déclenchement), **Tempo** (imposer un BPM ou
revenir à la détection) et **Source** (entrée, niveaux, seuil, capture des 40 dernières secondes).

Les trois couleurs : **Groove** (couleur principale), **Creux** (plus de kick depuis 1,5 s :
break, passage calme) et **Filtres** (passe-haut ou passe-bas : l'équilibre grave/aigu s'écarte
d'au moins ~14 dB de l'habitude du morceau, mémorisée sur 30 s). Un clic sur une pastille ouvre
son nuancier. La lampe passe de l'une à l'autre en fondu ; la luminosité de chaque couleur
plafonne celle de la scène (du noir en temps creux éteint la lampe pendant les breaks).

La section **Animation** choisit le mouvement de la lumière (`--motion` dans le terminal) :

- **Pulse** : un flash par kick ;
- **Respiration** : la luminosité suit l'énergie du son, les kicks n'ajoutent qu'une houle ;
- **Montée** : la lumière s'intensifie et blanchit, les flashs gagnent en force ;
- **Auto** (par défaut) : suit les phases du morceau, reconnues à l'écoute : **groove** (kicks
  réguliers) → pulse, **break** (plus de kick depuis 1,5 s) → respiration, **montée** (médiums et
  aigus qui enflent de plus de 4 dB sans basse, ou kicks qui roulent à plus de 4,5 par seconde)
  → montée, puis **drop** (premier gros kick après la montée) → éclat blanc et retour au pulse.
  Le passage d'une animation à l'autre se fait en fondu ; la phase reconnue s'affiche au-dessus
  du BPM, et la carte Auto du dock indique l'animation jouée en ce moment.

La section **Flashs** règle leur allure, avant comme pendant le live : **intensité** (jusqu'où
monte un flash au-dessus de la lueur), **redescente** (de 40 ms, sec, à 1,5 s, très doux) et
**lueur de fond** (luminosité minimale gardée entre les kicks).

Les couleurs et l'allure des flashs sont enregistrées à la fermeture de la fenêtre et reprises par `live`. Limite
connue : un break qui retire toute la basse ressemble à un passe-haut et prend la couleur des
filtres.

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

## État des lampes avant et après

Avant de prendre la main sur la zone de divertissement (`live`, `gui`, `test`, `latency`), DJ-Bridge
photographie l'état de chacune de ses lampes : marche/arrêt, luminosité, couleur ou blanc, et les
scènes actives qui les concernent. À la fermeture, il le rétablit en fondu (400 ms) et relance
ces scènes, une scène dynamique reprenant son animation.

La photo est aussi écrite dans `%APPDATA%\dj-bridge\lampes-avant-flux.json`. Si l'appli est
coupée brutalement (plantage, console fermée), elle est reprise au lancement suivant (si elle a
moins de 6 h) et rétablie à sa fermeture ; ou tout de suite avec :
`.\target\release\dj-bridge.exe restore`.

Les identifiants du bridge et l'avance calibrée sont stockés dans
`%APPDATA%\dj-bridge\config.json`, hors du dépôt.

## Feuille de route

1. Sortie Hue + mesure de latence ✅
2. Entrée audio : Focusrite 2i2 (WASAPI ✅, ASIO ✅), banc de filtres, détection de kick ✅
3. Mode synthé live : Ableton Link + MIDI, flashs anticipés de la latence mesurée ✅ (à tester)
4. Mode DJ : détection de tempo sur l'audio + anticipation ; bonus : réseau de l'OPUS-QUAD
5. Palette à trois couleurs, interface `egui` ✅ (à tester)

## Licence

Copyright © 2026 Hugo KIRBACH.

DJ-Bridge est un logiciel libre : vous pouvez le redistribuer et le modifier selon les termes de
la licence publique générale GNU publiée par la Free Software Foundation, version 3 ou (à votre
choix) toute version ultérieure (GPL-3.0-or-later). Il est distribué sans aucune garantie ; le
texte complet de la licence est dans [LICENSE](LICENSE).

DJ-Bridge intègre Ableton Link (GPLv2 ou ultérieure) et se compile avec le kit ASIO de
Steinberg, utilisé sous GPLv3. Le kit n'est pas inclus dans ce dépôt : il se télécharge chez
Steinberg (<https://www.steinberg.net/asiosdk>).
