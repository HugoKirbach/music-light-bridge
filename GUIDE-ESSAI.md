# Guide d'essai des évolutions

Les évolutions arrivent sur la branche `evolutions`, une par commit, le numéro entre crochets
(`[26] …`). Depuis la PR #3, tout ce qui précède [26] est aussi dans `master`. `master` reste la
version pour jouer : une nouvelle évolution n'y passe qu'une fois essayée et gardée.

**Comment répondre :** pour chaque numéro, dis « ok », « pas ok » (avec ce que tu as vu) ou « à
revoir ». Je fais passer dans `master` ce que tu gardes, puis je vérifie compilation, tests et banc
d'essai. Tu n'as rien à faire dans git.

**Avant de commencer :**

```bash
git switch evolutions
```

```bash
cargo build --release
```

Puis lance `.\target\release\dj-bridge.exe gui`. Pour les numéros 2, 3 et 7, un mix avec de vraies
montées et de vrais drops (techno, house) aide beaucoup. Si un passage se passe mal, appuie sur
« Capturer les 40 dernières secondes » (onglet Source) et note l'heure : je pourrai le rejouer.

---

## [19] Découpage du code

Rien de visible : le moteur et le rendu sont rangés en modules plus petits.
**À vérifier :** que tout marche comme avant (c'est la base des autres).

## [1] Breaks fiables

Les breaks ne repassent plus en groove au moindre coup de kick ou roulement.
**À essayer :** un morceau avec un break, puis la reprise.
**Ce qui est attendu :**
- la pastille au-dessus du BPM passe sur BREAK pendant le break et y reste malgré les impacts
  isolés ;
- elle repasse sur GROOVE à la reprise (au plus quelques temps après) ;
- dans un passage fort, elle ne bascule pas en break juste parce qu'un kick manque.

## [11] Placement des lampes

Dans la fenêtre, section Lampes : une bande « pièce » où faire glisser chaque lampe cochée de
gauche à droite. La place est enregistrée.
**À essayer :** place la Hue Go à gauche et le Bureau à droite, lance le live en disposition
**Vague** puis **Alternance**.
**Ce qui est attendu :** la vague traverse dans le bon sens, et l'alternance va de gauche à droite.

## [15] Retard par lampe et test de synchro

Nouvel onglet **Lampes** dans le dock du live.
**À essayer :**
1. Coche les 4 lampes, lance le live (même sans musique) et active **Test de synchro** : toutes
   flashent en blanc à chaque temps (ou toutes les 0,5 s).
2. Repère celle qui s'allume en retard (souvent la Hue Go ou une ampoule ancienne). Monte son
   curseur jusqu'à ce qu'elles flashent ensemble.
3. Relance : le réglage doit être repris.

**Ce qui est attendu :** les lampes alignées entre elles, et les flashs toujours à l'heure avec le
son (sinon refais `dj-bridge latency`).

## [2] Montées comprises

Pendant une montée, la pastille affiche « MONTÉE · 62 % · DROP DANS 12 T » quand la structure est
reconnue (sinon seulement le %).
**À essayer :** un morceau avec une montée d'au moins 8 mesures, en animation **Auto**. Il faut
que la structure ait été reconnue avant : la pastille du groove doit afficher « MESURE n/4 ».
**Ce qui est attendu :**
- le % monte régulièrement jusqu'au drop ;
- le décompte « DROP DANS … T » tombe à 0 sur le vrai drop, ou se décale d'une phrase si le drop
  vient plus tard ;
- la lumière s'intensifie avec le %.

## [3] Drops anticipés

Quand le drop est prévu (voir [2]), l'éclat blanc part en avance pour tomber pile sur le drop.
**À essayer :** même situation que [2]. Regarde le moment du drop.
**Ce qui est attendu :**
- l'éclat blanc tombe en même temps que le drop, sans retard visible ;
- s'il n'y a pas de drop à l'endroit prévu (fausse montée, break qui continue), un flash blanc
  bref peut apparaître puis s'éteint, une seule fois par montée.

Dis-moi si ces faux éclats te gênent.

## [7] Régisseur automatique

En disposition **Auto**, la disposition du groove change d'une section à l'autre (au drop, au
retour du groove, toutes les deux phrases), selon l'énergie. La teinte principale varie aussi un
peu (±20°).
**À essayer :** un set de 10–15 min en disposition Auto, case **Régisseur auto** cochée. Compare
ensuite en la décochant (retour à l'alternance fixe et à ta couleur exacte).
**Ce qui est attendu :**
- des changements qui tombent aux bons moments ;
- ensemble ou alternance quand ça monte en énergie, graves / aigus ou vague quand ça retombe ;
- une teinte qui reste dans l'esprit de ta couleur.

La carte Auto du dock affiche « En ce moment : … ».

## [12] Reprise automatique (bridge et audio)

**À essayer, pendant le live :**
- **Bridge** : débranche le câble réseau du bridge (ou son alimentation) 10–20 s, puis
  rebranche. La scène affiche « Bridge injoignable : reconnexion en cours… » et les lumières
  reprennent seules, sans arrêter le live. Après un redémarrage complet du bridge, compte jusqu'à
  une minute.
- **Audio** : débranche l'USB de la Focusrite quelques secondes, puis rebranche. Le message
  « Entrée audio perdue… reconnexion » apparaît, puis « Entrée audio retrouvée ».

**Ce qui est attendu :** pas besoin de relancer. Le bilan de fin dit « Flux du bridge rouvert n
fois » et « Entrée audio rouverte n fois ».

## [22] Scènes

Section **Scènes** dans la fenêtre (en bas à droite) et onglet **Scènes** dans le dock du live.
**À essayer :**
1. Règle couleurs, animation et disposition, donne un nom (« Drop rouge »), puis clique sur
   **Enregistrer la scène**. Fais-en deux ou trois différentes.
2. Pendant le live, clique sur les scènes : tout change d'un coup, et la fenêtre suit.
3. Avec le synthé, enregistre une scène avec une note MIDI (par exemple 36). Joue cette note :
   la scène est rappelée, sans flash de note.
4. Dans le terminal (`dj-bridge live`), les touches 1, 2, 3… rappellent les scènes.
5. Clic droit sur une scène pour la supprimer.

## [21] Télécommande mobile

**À essayer :**
1. Coche **Piloter le live depuis un téléphone** (section 5 · Télécommande) et lance le live.
2. Windows peut demander l'accès réseau : accepte (réseau privé).
3. L'adresse s'affiche (message et onglet Source), par exemple `http://192.168.1.20:8484/…/`.
   Ouvre-la sur le téléphone, connecté au même Wi-Fi.

**Ce qui est attendu :**
- la page montre le BPM, la phase et la couleur des lampes en direct ;
- les boutons scènes, animation, disposition, déclenchement et régisseur agissent, et la fenêtre
  du PC suit ;
- **DROP** envoie un éclat blanc ;
- l'adresse reste la même au lancement suivant (tu peux la mettre en favori).

## [23] Pièces, configurations et mouvements calculés

La section Lampes de la fenêtre décrit maintenant la pièce. La bande gauche → droite du [11]
devient un plan vu de dessus. Tes réglages actuels sont repris dans « Ma pièce / Par défaut ».
**À essayer :**
1. Règle largeur et profondeur, puis place le DJ et tes 4 lampes sur le plan. Clique chaque lampe
   pour régler sa hauteur (sol, mi-hauteur, plafond).
2. Sous le plan, regarde les mouvements en couleur. Survole ceux en gris pour la raison.
3. **Copier** une configuration, la renommer, en cocher d'autres lampes ou les déplacer ;
   passer de l'une à l'autre. Crée aussi une deuxième pièce.
4. Lance le live et essaie chaque nouvelle disposition dans l'onglet Animation : Avant → fond,
   Diagonale, Bas → haut, Rotation, Onde du DJ, Devant / fond, Coins opposés.

**Ce qui est attendu :**
- chaque mouvement suit le plan, avec les bonnes lampes dans le bon ordre ;
- en Auto, la montée grimpe vers le plafond et le break tourne autour de la pièce si les lampes
  l'entourent ;
- seuls les mouvements possibles sont proposés, dans la fenêtre et sur le téléphone.

## [24] Motifs rythmiques et breaks qui flottent

L'appli apprend où tombent les kicks dans la mesure, quand ils ne sont pas sur chaque temps
(breakbeat, demi-tempo). Hors groove, un coup sans grave ne flashe plus.
**À essayer :**
1. Un morceau en 4/4 classique : rien ne doit changer, c'est le garde-fou.
2. Un breakbeat ou un morceau en demi-tempo : après quelques mesures, les flashs tombent pile sur
   les kicks, même ceux entre les temps.
3. Un break avec une mélodie seule, ou mélodie et charlestons : la lumière respire sans taper, et
   la pastille reste sur BREAK, pas MONTÉE.
4. Un vrai riser (bruit blanc qui monte, roulement de caisse claire) : la pastille passe sur
   MONTÉE après environ 3 s.

**Limite connue :** ton hard groove (kick décalé d'une double croche après le 2e temps) n'est pas
encore anticipé. La grille hésite entre les deux familles de kicks, et les flashs restent réactifs,
avec environ 60 ms de retard. Si tu captures un autre passage de ce style, il m'aidera.

## [25] Musiques dures : hard trance, hard house, schranz, hard techno

Le moteur a été affiné sur des cas de synthèse fidèles à ces styles, puis sur tes trois captures
hard trance du 28 octobre (158 à 163 BPM).
**À essayer :**
1. Hard techno ou schranz (kick saturé, rumble, coupures du grave à la table) : les flashs tombent
   sur les kicks, pas un peu avant, et le groove tient pendant les coupures.
2. Hard house : le donk à contretemps ne flashe pas, et la pastille ne passe pas sur MONTÉE.
3. Hard trance, kick qui se tait avant chaque phrase : dès le troisième trou, la lumière se tait
   dans le trou et reprend pile avec le kick.
4. Hard trance, montée en roulement de caisse claire sans basse : MONTÉE quand le roulement passe
   en doubles croches, puis GROOVE dès le premier kick du drop, et les kicks du drop sont
   anticipés tout de suite.

**Limites connues :**
- Un kick noyé dans une basse continue (capture du 28 octobre à 19 h 48) : le tempo peut se caler
  à côté. Dis-moi ce qui se passe dans la capture manuelle de 19 h 55, je l'ajouterai au banc.
- Une caisse claire qui continue dans le trou du kick fait flasher son temps.

## [26] Sonde OPUS-QUAD

Avant de brancher l'OPUS sur la lumière, il faut savoir ce qu'il envoie au PC, par quel chemin
(câble USB ou Wi-Fi), et si la liaison tient. La sonde se présente à l'OPUS comme rekordbox (mode Lighting), écoute,
et fait le bilan. Elle ne touche pas aux lampes.

**À faire :**
1. Ferme rekordbox : il prend les mêmes ports réseau.
2. Branche l'OPUS au PC en USB-C, avec ta clé USB dans l'OPUS, comme pour mixer.
3. Lance la sonde (3 minutes, Échap pour arrêter avant) :

   ```bash
   .\target\release\dj-bridge.exe sonde-opus
   ```

4. Si Windows demande d'autoriser dj-bridge sur le réseau, accepte. Si la carte réseau de l'OPUS
   est rangée dans les réseaux publics, il faut aussi cocher « publics ».
5. Pendant l'écoute :
   - charge un morceau sur la platine 1, lance-le, bouge le pitch ;
   - charge un morceau sur la platine 2, lance-le ;
   - passe le rôle de maître de l'une à l'autre ;
   - baisse puis remonte le fader d'une platine.
6. Copie-moi le bilan affiché à la fin. Le journal complet est dans
   `%APPDATA%\dj-bridge\captures\opus-….txt`, je le relirai.

**Ce que la sonde dit :**
- La liste des cartes réseau au démarrage. Si aucune carte de l'OPUS n'y apparaît, la liaison
  réseau par USB n'existe pas quand l'OPUS joue en autonome. C'est la première chose à savoir.
- Si l'OPUS répond quand on se présente comme rekordbox.
- L'état de chaque platine : morceau, tempo, pitch, lecture, maître, à l'antenne.
- La structure des morceaux chargés (Intro, Up, Down, Chorus, Outro), si l'OPUS l'envoie.
- Les coupures de la liaison, et le tempo mesuré par le compteur de temps de l'OPUS comparé au
  tempo annoncé.

**Si rien n'arrive :** relance une fois après une minute (la carte réseau USB peut mettre ce temps
à recevoir une adresse). Essaie aussi `sonde-opus --passif`, qui écoute sans rien envoyer.

**Premier essai (4 novembre) :** tout est passé par le Wi-Fi de la maison (l'OPUS à
192.168.1.88), sans aucune coupure, avec un état par platine toutes les 160 ms. Windows a bien vu
la carte réseau USB de l'OPUS (« AlphaTheta Network Adapter »), mais sans adresse pendant la sonde.
L'OPUS a envoyé de lui-même la grille, la structure, les waveforms et la pochette de chaque morceau
chargé. Le compteur de temps suit le tempo joué ; il recule en lecture à l'envers et tourne en
rond dans une boucle.

**Deuxième essai, pour l'USB seul :**
1. Coupe le Wi-Fi de l'OPUS dans ses réglages, puis branche l'USB.
2. Attends une minute, puis lance la sonde.
3. La liste du début doit montrer « AlphaTheta Network Adapter » avec une adresse (169.254.… ou
   autre). Si l'adresse arrive pendant l'écoute, la sonde affiche « Nouvelle adresse ».
4. Refais les mêmes gestes, et copie-moi le bilan. La ligne « Chemin » dit par où l'OPUS est
   passé.

## [27] La lumière menée par l'OPUS-QUAD

La lumière suit la platine maître de l'OPUS par le Wi-Fi, comme au premier essai de la sonde, sans
avoir besoin du son.
- **Flashs :** sur les temps où la waveform du morceau montre un kick, en avance de la latence des
  lampes.
- **Phases :** une phrase rekordbox « Up » est une montée, dont le drop tombe au début de la phrase
  suivante. Ailleurs, c'est un groove s'il y a des kicks, et un break qui flotte sinon.
- **Régisseur :** il change de mise en scène aux têtes de phrase.
- **Couleurs de la trame :** la couleur de la waveform du temps en cours teinte les lampes (rouge pour
  les graves, vert pour les médiums, bleu pour les aigus).

**À essayer :**
1. Ferme rekordbox et la sonde. L'OPUS reste sur le Wi-Fi de la maison, comme au premier essai.
2. Lance la fenêtre :

   ```bash
   .\target\release\dj-bridge.exe gui
   ```

3. Tempo : choisis « OPUS-QUAD », puis lance le live.
4. Sous le BPM, une ligne du genre « BPM · OPUS · PLATINE 1 · CHORUS → UP DANS 12 T ». Juste après
   un chargement ou un saut, elle affiche « CALAGE » une ou deux secondes : la lumière attend alors
   avant de flasher.
5. Mixe :
   - les flashs tombent sur les kicks, et rien ne flashe dans le vide ;
   - les breaks flottent ;
   - une phrase Up monte, et son drop tombe pile au début de la suivante ;
   - en passant le rôle de maître à l'autre platine, la lumière la suit.
6. Si les flashs tombent toujours un peu après les kicks (ou avant), règle « Retard de l'OPUS » dans
   l'onglet Tempo, par crans de 5 ms. La valeur est gardée.
7. Couleurs : décoche « Couleurs de la trame » (onglet Couleurs) pour revenir à ta palette, et
   compare.

**Limites connues :**
- La waveform est celle du morceau, pas du mix : faders, EQ et filtres de l'OPUS n'y changent rien.
- Pendant une transition, c'est la platine maître qui mène.
- Morceau déjà chargé avant le lancement : l'OPUS n'envoie ses données qu'au chargement. DJ-Bridge
  les redemande, mais seule la demande de structure est vérifiée. Si la lumière reste en break,
  recharge le morceau.
- Fin de boucle : un ou deux flashs peuvent tomber jusqu'à 80 ms à côté.

---

## Ce qui a été vérifié de mon côté

- Compilation, `clippy` sans avertissement, 114 tests unitaires, dont un catalogue de cas musicaux
  de synthèse (motifs, breaks, montée, filtre).
- Banc d'essai sur les 5 captures annotées : note d'ensemble 80, inchangée par les numéros 15 à
  21.
- [25] : 120 tests, dont 12 cas musicaux de synthèse ; banc d'essai inchangé sur les 7 captures
  annotées.
- [26] : tests sur les paquets de l'OPUS, dont 4 sur de vrais paquets de ton premier essai
  (état, structure masquée, grille, waveform) ; la sonde corrigée a relu tout le journal de ce
  premier essai ; banc d'essai inchangé.
- [27] : 149 tests, dont 9 sur la liaison OPUS (kicks de la waveform, phases, prochain kick,
  calage de la phase, boucles) ; ton journal rejoué à 60 images par seconde : les flashs visent le
  vrai temps à ±15 ms en lecture normale (écart-type), aucun flash hors kick ; banc d'essai
  inchangé ; la fenêtre s'ouvre. Jamais essayé avec les lampes ni avec l'OPUS en direct.
- La fenêtre (capture d'écran) et la page mobile (aperçu dans un navigateur, à la taille d'un
  téléphone).

Rien de tout ça n'a été essayé avec les vraies lampes ni le vrai son, d'où ce guide. Les points
les plus incertains :
- **[3]** : l'anticipation ne joue que si la structure est reconnue avant la montée.
- **[7]** : les choix du régisseur sont une question de goût.
- **[12]** : le comportement exact du bridge quand on le coupe.
