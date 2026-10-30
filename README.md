# DJ-Bridge

Lumières Philips Hue pilotées par le son (platines, synthé, ce que joue le PC) et par le tempo
(détection automatique ou Ableton Link), avec la latence la plus faible possible. L'appli écoute
la musique et règle seule la mise en scène : elle flashe sur les vrais kicks, en avance pour
tomber pile malgré la latence des ampoules. Elle reconnaît les breaks, les montées et les drops,
et répartit la lumière entre les lampes selon leur place dans la pièce.

Écrit en Rust. La lumière part par l'Entertainment API de Hue : un flux UDP chiffré en DTLS,
60 paquets par seconde. Windows uniquement pour l'instant (capture ASIO/WASAPI).

- [Ce que fait DJ-Bridge](#ce-que-fait-dj-bridge)
- [Matériel](#matériel)
- [Installation](#installation)
- [Premier réglage : bridge, zone, latence](#premier-réglage--bridge-zone-latence)
- [Utilisation : la fenêtre](#utilisation--la-fenêtre)
- [Utilisation : le terminal](#utilisation--le-terminal)
- [Comment ça marche](#comment-ça-marche)
- [Analyse, captures et banc d'essai](#analyse-captures-et-banc-dessai)
- [Fichiers et état des lampes](#fichiers-et-état-des-lampes)
- [Dépannage](#dépannage)
- [Licence](#licence)

## Ce que fait DJ-Bridge

- **Flashs sur les vrais kicks.** Ils sont détectés dans les basses et anticipés de la latence
  des lampes, pour s'allumer en même temps que le son. Les breaks restent sombres.
- **Tempo et grille.** Le BPM est détecté sur les kicks (ou imposé, ou donné par Ableton Link),
  et une grille de temps tient à travers les breaks et les roulements.
- **Structure du morceau.** Les mesures et les phrases (16 temps) sont repérées aux changements de
  section. La tête de mesure et la tête de phrase sont alors accentuées.
- **Phases reconnues** : groove, break, montée (avec son avancement vers le drop prévu), drop. Le
  drop peut être allumé d'avance quand la structure le prévoit.
- **Régisseur automatique.** Il change la disposition et la teinte d'une section à l'autre, selon
  l'énergie du set.
- **Pièces et configurations.** On dessine la pièce (cotes, place du DJ), et chaque configuration
  y place les lampes et leur hauteur. DJ-Bridge en tire les mouvements possibles : vagues et
  balayages dans tous les sens, rotation autour de la pièce, onde depuis le DJ, groupes qui se
  répondent. Chaque lampe a aussi son retard propre.
- **Scènes** enregistrées, rappelables d'un clic, d'une note MIDI ou du téléphone.
- **Télécommande mobile** : une page pour téléphone sur le réseau local.
- **Synthé** : Ableton Link pour le tempo exact, et chaque note MIDI flashe à sa couleur.
- **Fiable en soirée.** Le bridge et l'entrée audio perdus se reconnectent seuls. L'état des
  lampes est rétabli à la fin, même après un plantage.

## Matériel

- Un bridge Philips Hue **carré (v2)** et au moins une lampe **couleur** compatible Entertainment
  (ampoules, Hue Go, rubans…).
- Une **zone de divertissement** créée dans l'app Hue (Paramètres > Zones de divertissement) qui
  contient les lampes à piloter.
- Une source audio, au choix :
  - une carte son (testé avec une Focusrite Scarlett 2i2) branchée sur la table de mixage ou le
    synthé, de préférence en ASIO ;
  - rien du tout : l'option **Sortie du PC** capte ce que joue le PC (Spotify, Ableton…).
- Le PC et le bridge sur le même réseau local. Le téléphone aussi, pour la télécommande.

## Installation

DJ-Bridge se compile depuis les sources. Comptez une demi-heure la première fois, surtout du
téléchargement.

### 1. Outils de compilation (Windows)

1. **Visual Studio Build Tools 2022** : <https://visualstudio.microsoft.com/fr/downloads/>
   (section « Outils pour Visual Studio »). Dans l'installeur, cochez **Développement Desktop en
   C++**. Il apporte le compilateur MSVC, le SDK Windows et **CMake**, qui compile Ableton Link.
2. **Rust** : <https://rustup.rs> puis `rustup-init.exe`, avec les choix par défaut
   (toolchain `stable-x86_64-pc-windows-msvc`). Ouvrez ensuite un **nouveau** terminal pour que
   `cargo` soit trouvé.
3. **LLVM** (pour `libclang`, qui génère les liaisons C d'Ableton Link et d'ASIO) :
   <https://github.com/llvm/llvm-project/releases>, le fichier `LLVM-…-win64.exe`. Il s'installe
   par défaut dans `C:\Program Files\LLVM`.
4. **Kit ASIO de Steinberg** : <https://www.steinberg.net/asiosdk>. Décompressez-le, par exemple
   dans `C:\Dev\ASIOSDK`. Le kit n'est pas inclus dans ce dépôt pour des raisons de licence.

### 2. Récupérer et configurer le projet

```bash
git clone https://github.com/HugoKirbach/music-light-bridge.git
```

Le fichier `.cargo/config.toml` du dépôt indique où trouver ces outils. Adaptez les trois
chemins s'ils diffèrent chez vous :

```toml
[env]
CMAKE = "C:\\Program Files (x86)\\Microsoft Visual Studio\\2022\\BuildTools\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\CMake\\bin\\cmake.exe"
LIBCLANG_PATH = "C:\\Program Files\\LLVM\\bin"
CPAL_ASIO_DIR = "C:\\Dev\\ASIOSDK"
```

Avec Visual Studio Community ou Professional au lieu des Build Tools, remplacez `BuildTools` par
`Community` (ou `Professional`) dans le chemin de CMake.

### 3. Compiler

Dans le dossier du projet :

```bash
cargo build --release
```

L'exécutable est `target\release\dj-bridge.exe`. Les exemples qui suivent l'appellent
`dj-bridge` : lancez-le depuis le dossier du projet avec `.\target\release\dj-bridge.exe …`, ou
copiez-le où vous voulez (il est autonome).

## Premier réglage : bridge, zone, latence

1. **Associer le bridge.** Lancez `dj-bridge pair` et appuyez sur le bouton du bridge quand c'est
   demandé. Si le bridge n'est pas trouvé, cherchez-le avec `dj-bridge discover` et passez son
   adresse : `dj-bridge pair --ip 192.168.x.y`.
2. **Choisir la zone.** `dj-bridge areas` liste les zones de divertissement et leurs lampes
   (→ marque celle utilisée). S'il n'y en a qu'une, elle est choisie d'office ; sinon
   `dj-bridge areas --select <id>` désigne la bonne.
3. **Tester le flux** (facultatif) : `dj-bridge test` joue un arc-en-ciel pendant 10 s et mesure
   la régularité d'envoi.
4. **Calibrer la latence** : `dj-bridge latency`.
   - Chaque seconde, un tic part dans les enceintes et un flash blanc vers les lampes au même
     instant. Comme les lampes ont du retard, on entend le tic avant de voir le flash.
   - Utilisez ↑ / ↓ pour faire partir le flash plus tôt ou plus tard (5 ms), et → / ← pour un pas
     de 1 ms. Réglez jusqu'à ce que le tic et le flash tombent ensemble, puis appuyez sur Entrée
     pour enregistrer (Échap pour quitter sans enregistrer).
   - Pour une mesure précise, filmez au ralenti (240 i/s) avec l'avance à 0 ms. L'écart entre le
     tic et le flash sur la vidéo est la latence (1 image = 4,2 ms).
   - Prenez une sortie audio filaire : le Bluetooth ajoute une latence que Windows ne rapporte
     pas.
5. **Ouvrir la fenêtre** : `dj-bridge gui`. Elle ne touche pas aux lampes tant que le live n'est
   pas lancé.

Les identifiants du bridge et tous les réglages sont enregistrés dans
`%APPDATA%\dj-bridge\config.json`, hors du dépôt.

## Utilisation : la fenêtre

`dj-bridge gui` ouvre l'écran **Préparer la session**. L'en-tête indique si le bridge répond, la
zone pilotée et le nombre de lampes.

**Au centre, l'essentiel :**

1. **Source audio.**
   - **Carte son** : platines ou synthé branchés sur la carte son.
   - **Sortie du PC** : capte ce que joue le PC, sans rien brancher. Si la carte de cette sortie
     a une entrée **Loopback** (Scarlett 4e génération, Clarett+…), DJ-Bridge écoute celle-ci :
     elle entend tout ce que joue la carte, y compris Ableton en ASIO. Sinon, il capte ce que
     Windows joue sur la sortie (WASAPI loopback), où un logiciel en ASIO ne passe pas. La ligne
     sous l'appareil dit laquelle sera écoutée.
2. **Lampes.**
   - Cochez les lampes de la zone à piloter. Les autres gardent leur état pendant le live.
   - **Mettre à jour** relit la zone si elle a changé dans l'app Hue.
   - **Pièce et plan des lampes**, replié, décrit la pièce, ses configurations et le plan (voir
     [Pièces et configurations](#pièces-et-configurations)).
3. **Tempo.**
   - **Détection auto** (plage 100–190 BPM).
   - **BPM imposé** : le tempo reste fixe, seule la phase se cale sur les kicks.
   - **Ableton Link** : tempo et phase viennent de la session.

Puis le bouton **Lancer le live**. Dessous, **Plus d'options** reste replié et résume ce qu'il
contient :
- **Pilote de la carte son** : Auto (ASIO s'il est libre, sinon WASAPI), ASIO ou WASAPI.
- **MIDI** : les notes du synthé flashent à leur couleur, et une note peut rappeler une scène.
- **Télécommande** : cochez **Piloter le live depuis un téléphone** pour ouvrir la télécommande
  mobile au lancement.
- **Déclenchement des flashs** : Kicks anticipés, Réactif, Chaque temps.
- **Seuil des kicks.**

**À droite**, sous un aperçu de la couleur principale, la lumière se règle sur quatre pages, une
à la fois :
- **Mouvement** : animation (Auto, Pulse, Respiration, Montée), disposition des lampes (parmi
  celles que la configuration rend possibles), case **Régisseur auto**, houle des breaks.
- **Couleurs** : Groove, Creux, Filtres, rotation des couleurs, dégradé entre les lampes.
- **Flashs** : intensité, redescente, lueur de fond, contraste.
- **Scènes** : rappeler, enregistrer, supprimer.

Les réglages sont repris à l'ouverture suivante.

**Pendant le live**, la fenêtre devient une scène :
- Le fond prend la couleur des lampes.
- Le tempo s'affiche en très grand, avec quatre points pour les temps de la mesure.
- Au-dessus du tempo, la phase reconnue : « GROOVE · MESURE 3/4 », ou « MONTÉE · 62 % · DROP
  DANS 12 T ».
- En haut à droite, la couleur de chaque lampe, l'entrée captée et le bouton rond d'arrêt.

Le dock du bas garde tout réglable en onglets. Il reste replié sur ses onglets pour laisser la
scène dégagée : un clic sur un onglet l'ouvre, un second (ou **Replier**) le referme.

| Onglet | Contenu |
|---|---|
| Mouvement | Animation, disposition (la carte Auto montre celle jouée en ce moment), régisseur, houle |
| Couleurs | Les trois couleurs et leur nuancier, rotation, dégradé |
| Flashs | Allure des flashs |
| Scènes | Rappeler, enregistrer (avec une note MIDI facultative), supprimer (clic droit) |
| Tempo | Imposer un BPM ou revenir à la détection |
| Lampes | **Test de synchro** et retard propre de chaque lampe |
| Source | Entrée, niveaux, seuil, déclenchement, capture des 40 dernières secondes, adresse de la télécommande |

Le bouton rond en haut à droite arrête le live et rétablit les lampes. Le bilan de la session
s'affiche ensuite sous le bouton de lancement.

### Pièces et configurations

**Pièce et plan des lampes**, sous la section **Lampes**, décrit où sont les lampes. DJ-Bridge en
tire les mouvements possibles.

- **Pièce** : un rectangle vu de dessus, avec sa largeur et sa profondeur en mètres. Le devant
  (en bas du plan) est le côté du DJ. **Nouvelle** ajoute une pièce, **Renommer** et
  **Supprimer** gèrent la pièce choisie.
- **Configuration** : une pièce peut en avoir plusieurs (« Soirée », « Mix calme »…). Chacune
  garde les lampes cochées, la place et la hauteur de chaque lampe. **Copier** crée une
  configuration à partir de celle affichée. Le live utilise la pièce et la configuration choisies
  ici.
- **Plan** : faites glisser chaque lampe et le **DJ** (par pas de 10 cm). Cliquez une lampe pour
  choisir sa hauteur : **Sol**, **Mi-hauteur** ou **Plafond** (les lampes du plafond sont
  cerclées).
- **Mouvements possibles** : sous le plan, les dispositions que la configuration permet sont en
  couleur. Les autres sont grisées, et leur survol dit pourquoi (par exemple « les lampes doivent
  entourer la pièce » pour la rotation). Seules les dispositions possibles sont proposées, dans la
  fenêtre comme sur le téléphone.

À la première ouverture, une pièce « Ma pièce » de 5 × 4 m reprend les lampes écartées et leurs
places de gauche à droite.

### Accorder plusieurs lampes

Les ampoules et la Hue Go ne réagissent pas toutes à la même vitesse. Dans l'onglet **Lampes** :

1. Activez **Test de synchro** : toutes les lampes flashent en blanc sur chaque temps (ou toutes
   les 0,5 s sans tempo).
2. Si une lampe s'allume après les autres, montez son curseur (0–150 ms) jusqu'à ce que toutes
   flashent ensemble.

DJ-Bridge envoie alors les flashs plus tôt à la lampe la plus lente et retarde d'autant les
autres. Le réglage est enregistré.

### Scènes

Une scène garde les couleurs, l'allure des flashs, l'animation, la disposition, le déclenchement
et le régisseur.
- **Enregistrer** : donnez un nom (et une note MIDI si vous voulez) puis cliquez sur **Enregistrer
  la scène**. Une scène du même nom est remplacée.
- **Rappeler** : un clic, une note MIDI liée (elle rappelle la scène au lieu de flasher), une
  touche 1 à 9 dans le terminal, ou un bouton sur le téléphone.

### Télécommande mobile

Avec l'option cochée, ou `live --remote` dans le terminal, DJ-Bridge ouvre un petit serveur web
sur le port 8484.
- **L'adresse** s'affiche au lancement (onglet Source, ou première ligne du terminal), par exemple
  `http://192.168.1.20:8484/3f9c…/`. Ouvrez-la sur un téléphone connecté au même Wi-Fi.
- **La page** montre le tempo, la phase, l'avancement d'une montée et la couleur des lampes. Elle
  pilote les scènes, l'animation, la disposition, le déclenchement et le régisseur, et un grand
  bouton **DROP** envoie un éclat blanc.
- **Sécurité** : l'adresse contient un jeton secret, gardé dans la configuration. Elle reste la
  même d'une soirée à l'autre, vous pouvez la mettre en favori. Ne la partagez qu'avec qui doit
  piloter.
- **Pare-feu** : au premier lancement, Windows demande d'autoriser l'accès réseau de
  `dj-bridge.exe`. Acceptez pour les réseaux privés.

## Utilisation : le terminal

`dj-bridge live` lance le mode live sans fenêtre. Il reprend les lampes, places, retards, couleurs
et scènes réglés dans la fenêtre.

| Option | Effet |
|---|---|
| `--input "nom partiel"` | Une autre entrée audio (voir `dj-bridge devices`) |
| `--audio auto\|asio\|wasapi\|output` | Pilote de capture ; `output` capte ce que joue le PC |
| `--bpm 155` | Démarrer avec un tempo imposé |
| `--bpm-min` / `--bpm-max` | Plage de la détection automatique (100–190) |
| `--flash kicks\|reactif\|temps` | Kicks anticipés (défaut), chaque kick sans avance, chaque temps |
| `--motion auto\|pulse\|breath\|build-up` | Animation |
| `--spread auto\|together\|wave\|alternate\|split\|depth\|diagonal\|rise\|rotate\|ripple\|ping-pong\|corners` | Disposition ; la pièce et la configuration sont celles choisies dans la fenêtre |
| `--no-director` | Sans régisseur : en disposition auto, le groove alterne toujours |
| `--rotation off\|8\|16` | Rotation des couleurs toutes les 8 ou 16 mesures (défaut : fixes) |
| `--link` | Rejoindre la session Ableton Link |
| `--midi ["nom partiel"]` | Écouter une entrée MIDI |
| `--remote` | Ouvrir la télécommande mobile |
| `--threshold 1.6` | Seuil de détection des kicks |

Touches en direct :

| Touche | Effet |
|---|---|
| `b`, chiffres, Entrée | Imposer un BPM |
| `+` / `-` | BPM ±0,5 |
| `a` | Revenir au BPM automatique |
| `m` | Déclenchement : kicks → réactif → temps |
| ↑ / ↓ | Seuil des kicks plus haut (moins de kicks) / plus bas |
| `1` à `9` | Rappeler une scène |
| `r` | Enregistrer les 40 dernières secondes (pour `analyze`) |
| Échap | Quitter (lampes rétablies) |

Ligne d'état, de gauche à droite :
- `B` / `M` / `A` : niveaux des basses, médiums et aigus ;
- `●` : kick détecté ; `♪` : note MIDI ;
- `♩` : tempo et sa source ; puis le mode de flash et la phase (avec le % d'une montée) ;
- `±n ms` : écart moyen entre les kicks et les temps de l'horloge ;
- `★` : dernière scène rappelée ;
- `⚠` : coupures audio, entrée perdue ou bridge injoignable ;
- `●rec` : captures automatiques.

Autres commandes : `devices` (avec `--test` pour vérifier qu'une entrée reçoit du son),
`restore`, `analyze`, `bench` (voir plus bas).

## Comment ça marche

### De la musique au kick

L'entrée audio est découpée en blocs par le pilote (ASIO avec une Focusrite réglée sur 64 ou
128 échantillons pour la latence la plus faible). Un banc de filtres suit les basses, les
médiums et les aigus, et un détecteur de kick cherche le bond d'énergie du grave.

Le détecteur sait lire un mix dense. Il prend en compte le punch autour de 160 Hz, net même quand
le sidechain efface la sous-basse au même instant. Il guette aussi le clic d'attaque vers 2,5 kHz,
qui précède le corps du kick d'une vingtaine de ms, et un kick qui remonte du creux creusé par le
sidechain. Près d'un kick attendu, le seuil relâché ne descend pas sous les deux tiers de l'avance
habituelle des kicks du morceau sur le fond : sous un rumble (sous-basse continue de hard
techno), sa remontée ne passe pas pour le kick suivant. Toutes les 10 ms, une force d'attaque par bande est publiée pour le suivi du tempo,
sans jamais bloquer le fil audio.

### Tempo et grille

Les kicks et la force d'attaque alimentent une horloge de tempo. Les écarts entre kicks donnent
le BPM, et un « peigne » range la force d'attaque sur la grille des 6 dernières secondes pour
caler la phase, même décalée d'un demi-temps après un break. Au drop d'une montée, il oublie la force
d'attaque d'avant : un roulement de caisse claire ne retarde pas le recalage sur les kicks du drop. Une fois sûr, le tempo est tenu une
minute : un break ou un roulement ne le déplace pas.

Quand le tempo est sûr, la détection cherche le kick là où le motif l'attend (voir plus bas) :
- elle relâche ses seuils près d'un kick attendu ;
- en plein groove, elle les durcit ailleurs, pour qu'une note de basse ne passe pas pour un kick.

### Motif rythmique

La plupart des morceaux ont un kick sur chaque temps. D'autres le placent ailleurs : hard groove,
breakbeat, demi-tempo. DJ-Bridge apprend, mesure après mesure, sur quelles doubles croches de la
mesure (seize) tombent les kicks détectés. Le motif ne remplace le « un kick par temps » que s'il
est net :
- un temps reste vide depuis plusieurs mesures ;
- les doubles croches du motif portent un kick dans la plupart des mesures ;
- les kicks hors du motif pèsent moins que ceux du motif.

Un morceau en 4/4 garde donc exactement le comportement habituel. Limite connue : quand deux
familles de kicks d'égale force tombent à un quart de temps l'une de l'autre, la grille hésite
entre elles et le motif ne s'installe pas.

### Flashs anticipés

Une fois le groove établi (4 kicks attendus confirmés de suite), chaque kick du motif flashe **en
avance de la latence calibrée** : la lampe s'allume au moment où le kick sonne. Un kick manqué
garde son flash. Deux kicks manqués de suite signent un break : un flash de trop, puis le suivant
est annulé. Les kicks hors du motif (roulements, retour après un break) flashent aussitôt.

En plein groove (hors montée), un coup hors du motif ne flashe pas : le donk à contretemps de
la hard house, une note de basse ou un clap laissent la lumière tranquille.

**Trous de kick** : en hard trance ou en hard techno, le kick se tait souvent deux temps ou plus
avant la phrase suivante. Quand un trou de 2 temps à 2 mesures revient un nombre entier de
mesures après le précédent, il est appris : au suivant, la lumière se tait dans le trou, le groove
le traverse, et le kick qui revient est anticipé. Si un kick tombe malgré tout dans le trou prévu,
il flashe aussitôt et le trou est oublié.

### Breaks qui flottent

Hors du groove, un coup détecté ne compte comme kick que s'il porte du grave : au moins le quart
du niveau habituel du groove. Une mélodie seule, des charlestons ou un clap ne flashent donc pas
pendant un break : la lumière respire avec la musique, sans taper. Un vrai kick (un impact, le
retour du groove) passe toujours.

### Structure : mesures et phrases

Dans une boucle de techno, rien ne distingue le premier temps de la mesure. La structure se lit
aux changements de section (break, drop, entrée d'un élément), qui tombent en tête de phrase
(quatre mesures, seize temps). Chaque temps est décrit par quelques indices, et les deux mesures
qui le précèdent sont comparées aux deux qui le suivent : un pic de nouveauté vote pour sa place
dans la phrase. Une fois reconnue, la structure tient d'une section à l'autre. Tant qu'aucune
position ne ressort nettement, elle reste inconnue et rien ne change.

### Phases : groove, break, montée, drop

- **Groove** : la grille confirme des kicks réguliers.
- **Break** : la grille a perdu les kicks depuis 1 s, et soit ils sont rares, soit les basses
  sont tombées sous 40 % de leur niveau habituel. Si les basses ont disparu (sous 25 %) pendant
  2 s, c'est aussi un break, même quand des percussions tombent sur les temps.
  - Le niveau habituel monte vite avec le groove mais ne baisse qu'en une minute : un break qui
    s'installe ne l'entraîne pas.
  - Un roulement, un impact ou des percussions sur les temps ne relancent pas le groove. Il faut
    que la grille retrouve des kicks réguliers, avec des basses revenues aux trois quarts (le
    drop), ou au-dessus de 40 % pendant 8 s (un morceau aux basses plus légères).
- **Montée** : sans basse, les médiums et aigus ne cessent de monter (au moins 0,75 dB par seconde,
  trois secondes de suite, sans ralentir, jusqu'à 4 dB au-dessus des secondes précédentes). Ou bien
  des kicks qui portent du grave roulent : plus de 4,5 par seconde, et une fois et demie plus vite
  que d'habitude. Une mélodie ou des charlestons qui entrent d'un coup ne font pas une montée, et un
  groove qui tape en croches depuis longtemps (kick et donk de hard house) ne roule pas.
  - « Sans basse » : sous la moitié de leur plein, ou sous 40 % de celles du groove. Le gain
    automatique remonte vite le peu de grave d'un roulement de caisse claire, d'où la comparaison
    au groove.
  - Quand la structure est connue, le drop est prévu sur la première tête de phrase au moins
    8 temps après le début de la montée (puis la suivante s'il ne vient pas). L'intensité suit
    l'avancement vers ce drop.
  - Sans structure, l'intensité monte en 8 s.
- **Drop** : un kick aux basses pleines conclut la montée par un éclat blanc sur toutes les
  lampes. Il peut suivre de près un roulement de caisse claire : un kick qui ramène les
  basses au niveau du groove suffit.
  - Quand le drop est prévu, l'éclat part avec l'avance des lampes pour tomber pile.
  - Un kick aux basses nettement plus fortes que pendant la montée le confirme.
  - Sinon l'éclat s'éteint après 0,25 s, et la montée n'anticipe plus.

### Rendu : animation, disposition, couleurs

- **Animation** :
  - **Pulse** : un flash par kick ;
  - **Respiration** : la lumière suit l'énergie, un kick n'ajoute qu'une houle ;
  - **Montée** : la lumière s'intensifie et blanchit ;
  - **Auto** : pulse en groove, respiration en break, montée en montée, éclat au drop.
- **Disposition**, calculée depuis le plan de la configuration. Chaque lampe qui flashe s'allume
  sur le kick même, jamais après. Les mouvements dirigés (vague, avant → fond, diagonale,
  bas → haut, onde du DJ) avancent d'un kick à l'autre. Les lampes y sont groupées selon leur
  place, quatre groupes au plus, et chaque kick allume le groupe suivant :
  - **Ensemble** : toutes les lampes à chaque kick ;
  - **Vague** : de gauche à droite, puis retour ;
  - **Avant → fond** : du côté du DJ vers le fond ;
  - **Diagonale** : d'un coin à l'autre, puis retour ;
  - **Bas → haut** : du sol vers le plafond (il faut au moins deux hauteurs) ;
  - **Onde du DJ** : depuis le DJ vers les lampes les plus loin ;
  - **Rotation** : une lampe à chaque kick, tout autour de la pièce (il faut au moins trois
    lampes qui entourent la pièce) ;
  - **Alternance**, **Devant / fond**, **Coins opposés** : deux groupes à tour de rôle (gauche et
    droite, devant et fond, une diagonale et l'autre), tous ensemble en tête de mesure ;
  - **Graves / aigus** : une lampe sur deux suit les kicks, les autres les médiums et aigus ;
  - **Auto** : en montée, bas → haut si des lampes sont en hauteur (sinon vague) ; en break, une
    ondulation lente qui tourne autour de la pièce si les lampes l'entourent (sinon la traverse) ;
    en groove, c'est le régisseur qui choisit.

  Un mouvement demande des lampes assez étalées dans son sens (au moins 80 cm). Pendant un break,
  la respiration ondule le long du mouvement choisi.
- **Accents** : en tête de phrase, toutes les lampes flashent en blanc plus franc.
- **Régisseur** (disposition Auto) : une nouvelle section s'ouvre au drop, au retour du groove et
  toutes les deux phrases (64 kicks sans structure). Il choisit alors, parmi les mouvements que la
  configuration permet, selon l'énergie des 4 dernières secondes comparée à celle du set :
  - quand ça s'emballe, ensemble, alternance, coins opposés, devant / fond ou rotation ;
  - quand ça retombe, graves / aigus, vague, onde du DJ ou avant → fond.

  Il décale aussi la teinte principale de ±20° pour que deux sections se distinguent.
- **Couleurs** :
  - **Groove** : la couleur principale ;
  - **Creux** : en break ;
  - **Filtres** : passe-haut ou passe-bas, quand l'équilibre grave/aigu s'écarte d'environ
    14 dB de celui du morceau.

  On passe de l'une à l'autre en fondu par la teinte (espace OKLCh) : de rouge à vert, la lampe
  traverse l'orange et le jaune au lieu d'un kaki terne. Du noir en Creux éteint les lampes
  pendant les breaks. Un break qui retire toute la basse d'un coup garde la couleur Creux : seul
  un équilibre qui glisse en quelques dixièmes de seconde (un filtre qu'on tourne) prend la
  couleur Filtres.
- **Rotation** (page Couleurs) : la teinte avance d'un huitième de tour toutes les 8 ou 16
  mesures, en fondu, et à chaque drop ; **Fixes** (par défaut) garde les couleurs choisies. Les
  mesures se comptent sur les têtes de phrase : la rotation attend que la structure soit
  reconnue, et se fige pendant les breaks. Elle s'ajoute au décalage du régisseur ; les couleurs
  des notes MIDI ne tournent pas.
- **Dégradé entre les lampes** (±20° par défaut, page Couleurs) : la teinte de chaque lampe
  s'écarte un peu de celle de la scène, d'un côté puis de l'autre de la pièce, et le dégradé
  traverse lentement la pièce en 8 mesures (16 s sans tempo). Avec une seule lampe, sa teinte
  ondule autour de la couleur choisie. À 0, toutes les lampes ont la couleur de la scène.
- **Houle des breaks** (35 % par défaut, page Mouvement) : en respiration, la lumière se creuse
  entre les temps et remonte sur chaque demi-mesure, calée sur la grille qui continue de battre.
  L'ondulation du break dure deux mesures. Ces mouvements se recalent en douceur quand la grille
  bouge ; sans tempo, la houle s'efface et l'ondulation garde une période de 4 s.
- **Allure des flashs** (page Flashs) : **intensité** (jusqu'où monte un flash au-dessus de la
  lueur), **redescente** (de 40 ms, sec, à 1,5 s, très doux), **lueur de fond** (luminosité
  gardée entre les kicks) et **contraste** (courbe de luminosité, 1,4 par défaut : plus haut, les
  fondus s'attardent dans le sombre et les flashs ressortent davantage ; à 1, aucune courbe).
- **Flashs tenus** : le bridge ne relaie le flux qu'à ~25 Hz en Zigbee. Le sommet de chaque
  flash est tenu 40 ms, une période entière, pour que la lampe le reçoive toujours à pleine
  force. La lueur qui suit les basses est lissée (montée en 30 ms, descente en 150 ms) pour ne
  pas trembler à chaque note de basse.
- **MIDI** : chaque note flashe à sa couleur, sur le cercle des quintes (do rouge, sol orange,
  ré jaune…), et plus fort quand la vélocité est élevée.

### Fiabilité

- Si le bridge ne reçoit plus le flux (redémarrage, Wi-Fi), DJ-Bridge le rouvre seul, avec une
  attente de 0,5 à 5 s entre deux tentatives. La scène affiche « Bridge injoignable :
  reconnexion… ».
- Si l'entrée audio disparaît (câble, pilote, ou plus aucun bloc depuis 1 s), elle est rouverte
  toutes les 2 s.
- Le bilan de fin de session compte ces reprises.

## Analyse, captures et banc d'essai

- `dj-bridge analyze <capture.wav>` rejoue un enregistrement dans la même chaîne que le live et
  résume chaque tranche de 4 s. Le résumé donne le tempo, la part de groove, les temps marqués
  par un kick, les kicks hors temps, les flashs (anticipés, réactifs, annulés), la place dans la
  phrase et la phase.
  - Options : `--bpm`, `--threshold`, `--slice`.
- **Captures automatiques** : quand le groove décroche alors que les basses restent fortes (la
  détection a perdu les kicks, ce n'est pas un break), les 40 dernières secondes sont
  enregistrées d'elles-mêmes, au plus une toutes les 2 minutes. Elles vont dans
  `%APPDATA%\dj-bridge\captures\`, chacune avec une fiche JSON.
- `dj-bridge bench` rejoue les captures annotées dans `captures\bench.json` et note la lumière
  temps par temps : allumée à l'heure (à ±40 ms), en retard ou manquée, lumières en trop, tempo,
  breaks reconnus, mesures et phrases.
  - Chaque note est comparée au passage précédent.
  - `--detail` détaille chaque capture, `--lumieres` liste chaque flash.

  Exemple d'annotation :

  ```json
  { "captures": [ {
    "file": "capture-1792761389.wav", "note": "reprise après break",
    "groove": [ { "from": 18.25, "to": 40.0, "bpm": 154.0, "kick": 18.30 } ],
    "breaks": [ [12.5, 16.3] ], "phrases": [ 17.91 ]
  } ] }
  ```

- **Catalogue de cas** (pour le développement) : `cargo test catalogue -- --nocapture` joue des
  morceaux de synthèse décrits comme un DJ les décrirait, dont on connaît la vérité à la
  milliseconde. Chaque morceau a son motif de kick sur seize doubles croches, son charleston, sa
  basse, sa mélodie, son filtre et ses sections. Cas couverts :
  - techno 4/4 ;
  - breakbeat ;
  - demi-tempo ;
  - break avec une mélodie seule, ou avec mélodie et charlestons ;
  - roulement de montée puis drop ;
  - intro filtrée ;
  - hard groove syncopé (limite connue, test ignoré).

  Chaque cas vérifie que la lumière tape sur les kicks en groove et flotte en break.

## Fichiers et état des lampes

Avant de prendre la main sur la zone, DJ-Bridge photographie l'état de chaque lampe : marche,
luminosité, couleur et scènes actives. À la fermeture, il le rétablit en fondu et relance ces
scènes. La photo est aussi écrite dans `%APPDATA%\dj-bridge\lampes-avant-flux.json` : après un
plantage, elle est reprise au lancement suivant (si elle a moins de 6 h), ou tout de suite avec
`dj-bridge restore`.

| Fichier | Contenu |
|---|---|
| `%APPDATA%\dj-bridge\config.json` | Identifiants du bridge, zone, latence, réglages, pièces et configurations des lampes, retards des lampes, scènes, jeton de la télécommande |
| `%APPDATA%\dj-bridge\captures\` | Captures audio, leurs fiches, `bench.json` |
| `%APPDATA%\dj-bridge\lampes-avant-flux.json` | État des lampes à rétablir |

Ne publiez pas `config.json` : il contient les clés d'accès au bridge.

## Dépannage

- **`cargo` introuvable** : le terminal a été ouvert avant l'installation de Rust. Ouvrez-en un
  nouveau.
- **Erreur de compilation sur CMake, libclang ou ASIO** : vérifiez les trois chemins de
  `.cargo/config.toml`.
- **Barres de niveau vides** : vérifiez le gain d'entrée de la carte son. Dans Windows, allez dans
  Paramètres > Confidentialité > Microphone et activez « Autoriser les applications de bureau ».
- **ASIO indisponible** : un pilote ASIO n'accepte souvent qu'un logiciel à la fois (Ableton
  l'occupe). DJ-Bridge se replie alors sur WASAPI et le dit.
- **Ableton n'est pas entendu** (« Rien n'arrive de la sortie du PC ») : en ASIO, Ableton joue
  directement sur la carte, sans passer par Windows. Choisissez **Sortie du PC** avec la sortie
  de la carte : si elle a une entrée Loopback, DJ-Bridge l'écoute et entend Ableton. Vérifiez
  dans Focusrite Control que le Loopback reprend bien la lecture de l'ordinateur. Sans entrée
  Loopback, deux solutions :
  - dans Ableton, Préférences > Audio, passez le type de pilote sur **MME/DirectX** (plus de
    latence pour jouer) ;
  - ou gardez l'ASIO et prenez le tempo **Ableton Link** avec les flashs **Chaque temps** : la
    lumière suit alors la session sans écouter le son.
- **« Aucun son capté depuis 10 s »** : la source n'entend rien. Vérifiez l'appareil choisi et
  son gain.
- **Les flashs arrivent après le son** : refaites `dj-bridge latency`. Avec plusieurs lampes,
  réglez leurs retards avec le test de synchro.
- **Lampe absente** : elle doit faire partie de la zone de divertissement (app Hue), puis cliquez
  sur **Mettre à jour**.
- **Le téléphone n'ouvre pas la télécommande** : il doit être sur le même Wi-Fi que le PC, et
  `dj-bridge.exe` autorisé dans le pare-feu Windows (réseau privé).
- **Lampes restées dans l'état du live** : lancez `dj-bridge restore`.

## Licence

Copyright © 2026 Hugo KIRBACH.

DJ-Bridge est un logiciel libre : vous pouvez le redistribuer et le modifier selon les termes de
la licence publique générale GNU publiée par la Free Software Foundation, version 3 ou (à votre
choix) toute version ultérieure (GPL-3.0-or-later). Il est distribué sans aucune garantie ; le
texte complet de la licence est dans [LICENSE](LICENSE).

DJ-Bridge intègre Ableton Link (GPLv2 ou ultérieure) et se compile avec le kit ASIO de
Steinberg, utilisé sous GPLv3. Le kit n'est pas inclus dans ce dépôt : il se télécharge chez
Steinberg (<https://www.steinberg.net/asiosdk>).
