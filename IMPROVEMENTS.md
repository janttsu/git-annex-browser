# Parannusmahdollisuudet — git-annex-browser

Katselmoitu 2026-09-27 commitista `8d2e865`. Koko lähdekoodi (n. 6 200 riviä Rustia,
7 moduulia) luettiin läpi, `cargo clippy -D warnings` on puhdas ja kaikki 29 testiä
menevät läpi. Havainnot perustuvat koodin lukemiseen, ei profilointiin, ellei toisin
mainita. Mittapisteenä käytettiin keskikokoista testikokoelmaa: kymmenkunta repoa,
kymmeniä tuhansia tiedostoja ja satoja tuhansia sijaintimerkintöjä.

Rivinumerot viittaavat yllä mainittuun commitiin.

## Toteutustila (päivitetty 2026-09-27)

Kaikki alla luetellut kohdat on toteutettu haarassa `worktree-improvements-doc`.
Testejä on nyt 63, kun alussa niitä oli 29. Clippy on puhdas. Mittaukset on tehty
samalla testikokoelmalla väliaikaista välimuistia vasten.

| Kohta | Tila | Huomio |
|------|------|--------|
| 1.1 käyttöpuu joka näppäimellä | tehty | Puu rakennetaan kerran per avattu repo. |
| 1.2 summaryjen kloonaus | tehty | Solmut jakavat `Rc<[RepoSummary]>`-listan; lapsilistat välimuistissa. |
| 1.3 työntekijä estyy | tehty | Discovery, välimuistin luku, lataukset ja kirjoitus omissa säikeissään. `whereis`-fallback poistettu. |
| 1.4 turha piirto | tehty | Piirretään vain viestin, näppäimen, hiiren tai koon muutoksen jälkeen. |
| 1.5 `git rev-parse` jokaiselle repolle | tehty | Refit luetaan levyltä; `--max-depth` ja `--one-file-system` lisätty. |
| 1.6 ~14 git-prosessia per repo | tehty | Nyt noin 4. Lämpimällä levyvälimuistilla aika ei muuttunut (git annex find hallitsee); hyöty näkyy hitailla levyillä. |
| 2.1 koko välimuisti joka tallennuksella | tehty, poikkeama | Yksi JSON-tiedosto per repo + indeksi. Binääriformaattia (postcard/zstd) ei otettu: jako per repo poisti kustannuksen ja JSON pysyy luettavana. Koko 30 MB → 22 MB. |
| 2.2 `Rc` → `Arc` | tehty | |
| 2.3 turha uudelleenhydratointi | tehty | Sormenjälki: git-annex-haara, HEAD ja configin mtime. Toistuva `--scan`: 54 s → 1 s. |
| 2.4 poistuneet repot | tehty | Näytetään "not found since …"; `--prune` poistaa. |
| 3.1 tyhjä metadata | tehty | |
| 3.2 UTF-8-paniikki | tehty | |
| 3.3 `q`/`Esc` ohjeessa | tehty | Esc ei enää koskaan lopeta. |
| 3.4 taustapäivitys hylätään | tehty, tarkennus | Tarkemmin katsottuna Nav-vastaus sisältää aina uusimman tilan, joten tietoa ei hävinnyt. Status ja skannaustila kopioidaan silti heti. |
| 3.5 statusrivi | tehty | Vihjeet pudotetaan leveyden mukaan; "hex-ish" korjattu. |
| 3.6 suodatin kahdessa paikassa | tehty | |
| 4.1 uuid-kopiot | tehty, poikkeama | `Arc<str>`-internointi deserialisoinnin aikana indeksitaulukon sijaan. `--dump` huippumuisti 55 → 36 MiB. |
| 5.1 toisto | tehty | Kolme hakemistopuuta yhdeksi, yhteinen lokiparseri, `Remote::new`, `RepoSummary::placeholder`, yhteinen `scan`-moduuli. |
| 5.2 `NodeKind` | tehty | |
| 5.3 `UiState` | tehty | Näppäin- ja hiirikäsittely testataan ilman terminaalia. |
| 5.4 kuollut koodi | tehty | Vapaan tilan laskenta poistettiin (ei näytetty). |
| 6 käytettävyys | tehty | Esc, lajittelu `s`, J/K ja Ctrl+d/u, leveystietoinen katkaisu, hiiri, `↓`-merkki, "at risk" ja "missing here" -näkymät. |
| 7 testit ja CI | tehty, osin | git-annex CI:ssä, MSRV- ja audit-jobit, release-workflow. `cargo publish` jätetty tekemättä: vaatii crates.io-tilin ja päätöksen. MSRV 1.89 tarkistuu vasta CI:ssä, koska koneella ei ole rustupia. |
| 8 CLI | tehty | `--json`, `--cache`, `--offline`, `--jobs`, `--max-depth`, `--one-file-system`, `--force-rescan`, `--prune`. |
| 9 riippuvuudet | tehty | chrono ja suora libc-riippuvuus poistettu; `rust-version = "1.89"`. |

Käyttäytymismuutokset, jotka kannattaa tietää:

- Välimuisti on uudessa paikassa `~/.cache/git-annex-browser/v2/`. Vanha `cache.json` tuodaan kerran eikä sitä poisteta.
- `--scan` palauttaa virhekoodin, jos jonkin repon lataus tai tallennus epäonnistuu.
- `--dump` ei enää lataa muuttumattomia repoja uudelleen; `--force-rescan` pakottaa.
- Esc ei lopeta ohjelmaa; vain `q` ja Ctrl+C.
- Ajat näytetään muodossa `YYYY-MM-DD HH:MM UTC`.
- `git annex numcopies` -asetus ohittaa vanhan `annex.numcopies`-git-configin, kuten git-annexissa.

## Priorisointi

| # | Aihe | Vaikutus | Työmäärä |
|---|------|----------|----------|
| 1 | `build_usage_tree` ajetaan joka näppäinpainalluksella repo-valikossa | suuri | pieni |
| 2 | Koko välimuisti luetaan, parsitaan ja kirjoitetaan uudelleen 4 s välein skannauksen aikana | suuri | keskisuuri |
| 3 | Työntekijäsäie estyy: discovery, on-demand-lataus ja `whereis`-fallback ajetaan synkronisesti | suuri | keskisuuri |
| 4 | Tyhjä metadata ylikirjoittaa toimivan välimuistimerkinnän, jos `git` epäonnistuu | suuri (datan laatu) | pieni |
| 5 | Uudelleenhydratointi vaikka git-annex-haara ei ole muuttunut | keskisuuri | pieni |
| 6 | `q`/`Esc` ohjenäkymässä lopettaa ohjelman | pieni (UX) | pieni |
| 7 | UTF-8-paniikki `run_scan`-edistymisrivillä | pieni | pieni |
| 8 | Muistinkulutus: uuid-merkkijonot monistetaan jokaiseen avaimeen | keskisuuri | keskisuuri |
| 9 | Koodin toisto: placeholder-summaryt, `Remote`-rakentaminen, log-parserit, hakemistopuut | keskisuuri (ylläpito) | keskisuuri |
| 10 | `kind()`-merkkijonot enumiksi, UI-tila omaksi tyypikseen | keskisuuri (ylläpito) | keskisuuri |
| 11 | Testikattavuus: app/worker/tui testaamatta, git-annex-testi ei aja CI:ssä | keskisuuri | keskisuuri |
| 12 | CLI-liput ja `--json`-dump | pieni–keskisuuri | pieni |

---

## 1. Suorituskyky

### 1.1 Käyttöpuu rakennetaan uudelleen jokaisella näppäinpainalluksella

**Havainto.** `RepoNode::children()` (`src/node.rs:296-301`) luo joka kutsulla
`UsageDirNode::root(meta)`, joka kutsuu `build_usage_tree(&meta.files)`
(`src/usage.rs:172-180`). `App::execute` kutsuu `children()` jokaisella
Up/Down/PageUp/PageDown/Bottom/Descend-komennolla ja `App::snapshot` kutsuu sen
uudestaan, eli puu rakennetaan kahdesti per näppäinpainallus aina kun käyttäjä on
repo-valikossa. Puun rakentaminen on O(tiedostot × polun syvyys) ja allokoi
HashMap-rakenteita jokaiselle hakemistolle.

**Toteutus.**
- Lisää `RepoNode`-rakenteeseen `usage_tree: OnceCell<Rc<UsageTree>>` (tai rakenna
  puu kerran `RepoNode::new`-kutsussa) ja anna se `UsageDirNode::root`-funktiolle
  parametrina.
- Vielä parempi: laske puu kerran `App::ingest_meta`-vaiheessa ja tallenna
  `Rc<UsageTree>` `AnnexMetadata`-olion rinnalle (esim. `struct LoadedRepo { meta:
  Rc<AnnexMetadata>, usage: Rc<UsageTree> }` `preloaded`-mappiin). Silloin puu
  rakennetaan täsmälleen kerran per lataus.
- Sama välimuistitus `RepoNode::children()`-listalle kokonaisuudessaan:
  `cached_children: RefCell<Option<Vec<Rc<dyn Node>>>>` kuten `FilesOnDriveNode`
  jo tekee. Nyt myös `AllFilesNode` ja `FilesOnDriveNode` luodaan uudestaan joka
  kutsulla, joten niiden oma `cached_children` auttaa vasta kun solmu on työnnetty
  pinoon.

### 1.2 Juurilista kloonaa kaikki summaryt joka näppäinpainalluksella

**Havainto.** `RootNode::children()` (`src/node.rs:87-97`) kloonaa jokaisen
`RepoSummary`-olion (mukaan lukien `remote_usage`-vektorin) uuteen
`RepoSummaryNode`-olioon, ja `GlobalReportNode` saa kopion koko listasta. Kutsuja
tulee kolme per näppäinpainallus (`execute`, `snapshot` ja `snapshot`-funktion
`total_repos`-laskenta `src/app.rs:135-143`).

**Toteutus.**
- Vaihda `RootNode.summaries: Vec<RepoSummary>` muotoon `Rc<[RepoSummary]>` tai
  `Vec<Rc<RepoSummary>>`, jolloin solmut jakavat datan.
- Muuta `Node::children` palauttamaan `Rc<[Rc<dyn Node>]>` tai lisää oletusmetodi
  `child_count()`, jota `execute` käyttää pelkän pituuden tarvitessaan.
- `total_repos` on `self.summaries.len()`; ei tarvitse rakentaa lapsilistaa.

### 1.3 Työntekijäsäie estyy synkronisissa operaatioissa

**Havainto.** Kaikki navigointikomennot käsitellään samassa säikeessä
(`src/worker.rs`), joka myös:
- ajaa `find_annex_repos` synkronisesti ennen komentosilmukkaa (`worker.rs:71`) ja
  uudelleen `Refresh`-komennossa (`worker.rs:201`). Välimuistista ladattu näkymä
  näkyy, mutta siinä ei voi navigoida ennen kuin koko hakemistopuu on kävelty.
  Hitailla verkkolevyillä tai suurella `DIR`-puulla tämä kestää kauan.
- ajaa `load_metadata` synkronisesti, jos käyttäjä laskeutuu repoon jota ei ole
  vielä hydratoitu (`worker.rs:222-235`).
- ajaa `git annex whereis` synkronisesti jokaisella valinnalla, joka osuu
  tiedostoon ilman sijaintitietoa (`src/node.rs:869-877`, kutsutaan `snapshot`-
  funktiosta `details()`-metodin kautta). Jos "all annexed files" -listassa on
  monta tiedostoa, joiden sisältö on pudotettu kaikkialta, jokainen j/k-painallus
  käynnistää git-annex-prosessin (satoja millisekunteja).
- serialisoi ja kirjoittaa koko välimuistin (`persist_preloaded`, ks. 2.1).

**Toteutus.**
- Discovery omaan säikeeseen: lähetä löydetyt polut `meta_tx`-tyyppisen kanavan
  kautta (`enum BgMsg { Discovered(Vec<PathBuf>), Loaded(PathBuf,
  Result<AnnexMetadata>) }`). Komentosilmukka käynnistyy heti välimuistin
  lataamisen jälkeen.
- On-demand-lataus: älä lataa synkronisesti. Siirrä polku `to_hydrate`-jonon
  kärkeen (`Vec::push` + pop-lopusta jo suosii viimeksi lisättyä) ja jätä
  `RepoLoadingNode` pinoon; `install_loaded_repo` korvaa sen kun tulos saapuu.
- `whereis`-fallback: poista se `details()`-metodista kokonaan. Sijaintilokit
  luetaan jo suoraan git-annex-haarasta, joten fallback ei tuo uutta tietoa.
  Jos se halutaan säilyttää, tee siitä eksplisiittinen toiminto (esim. näppäin
  `w`) ja tallenna negatiivinen tulos, ettei kyselyä toisteta.
- Siirry `Rc` → `Arc` `AnnexMetadata`-jakamisessa (ks. 2.2), jotta serialisointi
  voidaan tehdä toisessa säikeessä.

### 1.4 UI piirretään 10 kertaa sekunnissa vaikka mikään ei muutu

**Havainto.** `main.rs:280-292` piirtää joka tickillä (`--tick-ms`, oletus 100 ms).
`render_list` rakentaa kaikki `ListItem`-rivit ja lowercase-vertailut uudestaan
jokaisella piirrolla.

**Toteutus.** Pidä `dirty: bool`, joka asetetaan kun saapuu `WorkerOut`, näppäin
tai `Event::Resize`. Piirrä vain kun `dirty`. `event::poll`-timeout voi silloin
olla pidempi (esim. 250 ms) ilman että reagointi kärsii, koska näppäimet
herättävät pollin heti.

### 1.5 `find_annex_repos` käynnistää `git rev-parse` jokaiselle tavalliselle git-repolle

**Havainto.** `is_annex_repo` (`src/annex.rs:420-439`) käynnistää prosessin aina
kun `.git/annex` puuttuu. Kotihakemistossa, jossa on satoja tavallisia git-repoja,
tämä on hidasta.

**Toteutus.** Tarkista tiedostojärjestelmästä: `git_dir/refs/heads/git-annex` on
olemassa tai `git_dir/packed-refs` sisältää rivin, joka päättyy
`refs/heads/git-annex`. Vasta jos kumpikaan ei kerro mitään, kysy gitiltä.
Lisää myös `WalkDir::same_file_system(true)` oletukseksi (tai lippu
`--one-file-system`) ja `--max-depth N`.

### 1.6 `load_metadata` käynnistää noin 14 git-prosessia per repo

**Havainto.** `src/annex.rs:1241-1267` ajaa `git config --get` kolmesti, `git
show git-annex:<log>` seitsemästi, `git config --get-regexp` kerran ja lisäksi
`git annex find` ja `git ls-tree` + `git cat-file --batch`. Prosessin käynnistys on
noin 5–10 ms, mutta hitaalla levyllä repositoryn avaaminen joka kerta maksaa
enemmän.

**Toteutus.**
- Yksi `git config --list --local` (tai `--get-regexp '^annex\.'`) ja poimi
  `annex.uuid`, `annex.describe`, `annex.numcopies` tuloksesta.
- Lue kaikki haaran tason lokit (`uuid.log`, `remote.log`, `trust.log`, …) samalla
  `git cat-file --batch`-kutsulla, jota `cat_file_location_logs` jo käyttää: syötä
  `git-annex:uuid.log`-muotoiset viittaukset (cat-file hyväksyy ne). Silloin koko
  haaran luku on yksi `ls-tree` + yksi `cat-file`.

## 2. Välimuisti

### 2.1 Koko välimuisti luetaan, parsitaan ja kirjoitetaan uudelleen jokaisella tallennuksella

**Havainto.** `upsert_cache_repos` ja `merge_scan_into_cache` (`src/annex.rs:1683-
1708`) tekevät `load_cache()` → koko JSON parsitaan → mergetään → koko JSON
serialisoidaan → kirjoitetaan. Skannauksen aikana `persist_preloaded` kutsutaan 4
sekunnin välein (`worker.rs:143-147`) ja jokaisella kerralla kaikki `preloaded`-
metadatat kloonataan (`v.as_ref().clone()`, `worker.rs:255`) ja `redact_meta`
kloonaa ne vielä kerran. Testikokoelmalla tiedosto on kymmeniä megatavuja ja skaalautuu
lineaarisesti avainten määrän mukaan; miljoonan avaimen annexeilla puhutaan
sadoista megatavuista, jotka parsitaan ja kirjoitetaan työntekijäsäikeessä
useita kertoja skannauksen aikana. UI näyttää sen ajan `[busy]`.

**Toteutus.**
- **Yksi tiedosto per repo.** `$XDG_CACHE_HOME/git-annex-browser/repos/<hash>.json`
  (hash kanonisesta polusta) + kevyt `index.json` (polku, uuid, nimi, summary,
  `scanned_at`, `annex_branch_sha`). Tallennus koskee vain muuttunutta repoa.
  Juurilista ladataan `index.json`-tiedostosta millisekunneissa; täysi metadata
  ladataan laiskasti kun repoon laskeudutaan tai taustalla.
- **Älä kloonaa serialisoidessa.** `serde` osaa serialisoida `&AnnexMetadata`-
  viittauksen; rakenna `HashMap<&str, &AnnexMetadata>` tai serialisoi `Rc`/`Arc`
  suoraan (`serde`-featuret `rc`). Redaktointi tehdään jo `parse_remote_log`-
  vaiheessa (`annex.rs:563`), joten `redact_meta` voidaan poistaa tai jättää
  halvaksi tarkistukseksi.
- **Formaatti.** JSON on luettavaa mutta hidasta ja isoa. Vaihtoehdot: `postcard`
  tai `bincode` + `zstd` (10× pienempi, paljon nopeampi), tai pidä JSON mutta
  pakkaa `zstd`-virralla. Säilytä `CACHE_VERSION`-tarkistus ja lisää migraatio
  vanhasta muodosta tai vähintään lokiviesti "cache format changed, rescanning".
- **Tiedoston oikeudet.** `write_cache_unlocked` (`annex.rs:1659-1674`) kirjoittaa
  `.tmp`-tiedoston umaskin oikeuksilla ja asettaa 0600 vasta renamen jälkeen.
  Luo tiedosto `OpenOptions::new().mode(0o600)` -asetuksella. Lisää `sync_all()`
  ennen renamea, jos halutaan kestävyys sähkökatkossa.

### 2.2 `Rc` → `Arc`, jotta tallennus voi tapahtua toisessa säikeessä

**Havainto.** `App` on suunniteltu ei-`Send`-tyypiksi (`Rc<dyn Node>`), mikä on
perusteltua, mutta `Rc<AnnexMetadata>` estää metadatan jakamisen tallennus-
säikeelle.

**Toteutus.** Vaihda `preloaded: HashMap<PathBuf, Arc<AnnexMetadata>>` ja
`RepoNode.meta: Arc<AnnexMetadata>` (solmut itse voivat pysyä `Rc`-tyyppisinä).
Tallennus: `thread::spawn(move || upsert(vec_of_arcs))`. Kustannus on
atomiviitelaskenta, joka on merkityksetön.

### 2.3 Uudelleenhydratointi vaikka mikään ei ole muuttunut

**Havainto.** TUI hydratoi *kaikki* löydetyt repot taustalla jokaisella
käynnistyksellä (`queue_hydrate`, `worker.rs:272-286`), vaikka git-annex-haara
olisi sama kuin välimuistia kirjoitettaessa. `annex_branch_mtime` on jo olemassa
mutta sitä käytetään vain järjestämiseen.

**Toteutus.** Tallenna välimuistiin per repo `annex_branch_sha: String` (`git
rev-parse git-annex`, tai lue `refs/heads/git-annex` / `packed-refs` suoraan
tiedostosta ilman prosessia) ja `head_sha` (työpuun tiedostolista riippuu
HEADista). Hydratoi vain jos jompikumpi eroaa. Lisää lippu `--force-rescan`, joka
ohittaa tarkistuksen. Tämä tekee normaalista käynnistyksestä käytännössä
välittömän.

### 2.4 Poistuneiden repojen käsittely

**Havainto.** `merge_scan_repos` (`annex.rs:1635-1648`) poistaa välimuistista
kaikki `scan_root`-polun alla olevat repot, joita ei tällä kertaa löytynyt. Jos
`DIR` sisältää liitospisteitä (esim. `/mnt`) ja levy on irti, sen repot katoavat
välimuistista — vastoin työkalun tarkoitusta ("often offline drives").

**Toteutus.** Merkitse `missing_since: Option<i64>` sen sijaan että poistat.
Näytä juurilistassa harmaana `(not mounted since 2026-09-20)`. Poista vasta
eksplisiittisellä `--prune`-lipulla tai kun `missing_since` on yli N päivää.

## 3. Oikeellisuus

### 3.1 Tyhjä metadata ylikirjoittaa toimivan välimuistimerkinnän

**Havainto.** `load_metadata` (`annex.rs:1241-1267`) nielee kaikkien
`run_git`-kutsujen virheet `unwrap_or_default`-kutsuilla. Jos `git` epäonnistuu
(levy irrotetaan kesken skannauksen, korruptoitunut repo, `git` puuttuu PATHista
cronissa), funktio palauttaa `Ok(AnnexMetadata { uuid: "", files: [], … })`, joka
mergetään välimuistiin hyvän merkinnän tilalle ja näkyy UI:ssa nollarivinä.

**Toteutus.** Palauta virhe, kun `annex.uuid` puuttuu:

```rust
let uuid = run_git(&root, &["config", "--get", "annex.uuid"])
    .context("reading annex.uuid")?;
let uuid = uuid.trim().to_string();
if uuid.is_empty() {
    anyhow::bail!("{}: annex.uuid is empty — not an initialised annex?", root.display());
}
```

Tee sama, jos `git ls-tree git-annex` epäonnistuu (`load_locations_from_branch`
palauttaa nyt hiljaa tyhjän mapin). Virhe kulkee jo `meta_rx`-kanavan kautta
statusriville, eikä vanha välimuistimerkintä katoa.

### 3.2 UTF-8-paniikki edistymisrivillä

**Havainto.** `main.rs:81-83`: `&name[name.len() - 37..]` viipaloi tavuindeksillä.
Jos hakemiston nimi sisältää ei-ASCII-merkkejä (ä, ö, emoji) ja leikkauskohta
osuu monitavuisen merkin keskelle, ohjelma paniikkii `--scan`-ajossa.

**Toteutus.** Käytä `name.chars().rev().take(37).collect::<String>()` käänteisenä
tai `unicode-segmentation`. Sama tarkistus `short_uuid` (`util.rs:31`) ja
`short_name` (`annex.rs:1460`) — ne toimivat, koska uuid on ASCII, mutta
`chars().take(8)` on turvallisempi.

### 3.3 `q`/`Esc` ohjenäkymässä lopettaa ohjelman

**Havainto.** `main.rs:360-372`: `Command::Quit`-haara osuu ennen `_ if
show_help`-haaraa, joten ohjenäkymä ei sulkeudu vaan ohjelma sammuu. Sama koskee
zoom-tilaa vain osittain (siellä `Quit if zoom` on käsitelty).

**Toteutus.** Käsittele `show_help` ensin:

```rust
if show_help {
    if !matches!(cmd, Command::None) { show_help = false; }
    continue;
}
```

Harkitse samalla `Esc`-näppäimen semantiikkaa: useimmissa TUI-työkaluissa `Esc`
sulkee päällimmäisen tilan (suodatin → zoom → ohje → taso ylös) ja vain `q`
lopettaa. Nyt `Esc` juurella lopettaa ilman varmistusta.

### 3.4 Taustapäivitys hylätään, jos navigointi on kesken

**Havainto.** `apply_background_snapshot` (`main.rs:436-466`): jos `pending > 0`
ja murupolku eroaa, saapunut snapshot pudotetaan kokonaan. Seuraava taustapäivitys
korjaa tilanteen, mutta jos se oli viimeinen (`scanning=false`, "cache updated"),
statusrivi jää näyttämään vanhaa tilaa.

**Toteutus.** Tallenna hylätty snapshot muuttujaan `pending_bg:
Option<ViewSnapshot>` ja sovella se, kun `pending` putoaa nollaan. Vähintään
kopioi `status` ja `scanning` hylätystä snapshotista nykyiseen, jotta statusrivi
pysyy ajan tasalla.

### 3.5 Statusrivi ylivuotaa kapeissa terminaaleissa

**Havainto.** `render_status` (`tui.rs`) muodostaa yhden pitkän rivin, joka
katkeaa keskeltä 80-sarakkeisessa terminaalissa, ja tärkein osa (status) jää
näkymättömiin, jos filter-teksti on pitkä.

**Toteutus.** Rakenna rivi prioriteettijärjestyksessä ja pudota vihjeet
(`/ filter  z zoom …`) pois, kun leveys ei riitä. `HELP_TEXT`-tekstissä mainittu
"hex-ish view" ei vastaa toteutusta — korjaa sanamuoto.

### 3.6 Suodatinlogiikka kahdessa paikassa

**Havainto.** `visible_indices` (`main.rs:482-494`) ja `render_list`-funktion
suodatin (`tui.rs`) toteuttavat saman vertailun erikseen. Jos toinen muuttuu
(esim. fuzzy-haku), ne eroavat ja valinta osoittaa väärään riviin.

**Toteutus.** Yksi `pub fn matches_filter(item: &ListItem, filter: &str) -> bool`
`app.rs`-moduulissa, jota molemmat käyttävät. Laske lowercase-muoto suodattimesta
kerran.

## 4. Muisti

### 4.1 UUID-merkkijonot monistetaan jokaiseen avaimeen

**Havainto.** `AnnexMetadata.locations: HashMap<String, HashSet<String>>`
(`annex.rs:142`). Testikokoelmalla 300 000 uuid-viittausta × 36 tavua + `String`- ja
`HashSet`-ylimäärä ≈ 30–40 MB pelkkiä uuid-kopioita. Sama tieto on
`Remote`-mapissa kerran.

**Toteutus.**
- Interoi uuidit: `uuids: Vec<String>` ja `locations: HashMap<String, SmallVec<[u16; 4]>>`
  (tai `u32` bittimaski, kun remoteja on alle 32/64 — käytännössä aina).
  Serialisoi välimuistiin samassa muodossa (`uuids`-taulukko + indeksit).
- Avaimet (`SHA256E-s…--…`) ovat 80–100 tavua ja esiintyvät sekä `files`-vektorissa
  että `locations`-mapissa. `Arc<str>` tai indeksi `keys: Vec<String>`-taulukkoon
  puolittaa niiden muistin.
- `ViewSnapshot` kopioi `repo_visuals` kaikille repoille jokaisella
  päivityksellä; sekin voi olla `Arc<[VisualRepoDetail]>`.

## 5. Koodin rakenne

### 5.1 Toistuva koodi

| Mitä | Missä | Korjaus |
|------|-------|---------|
| Placeholder-`RepoSummary` 16 kentällä | `app.rs:283-303`, `worker.rs:89-108` | `impl RepoSummary { fn placeholder(root: PathBuf) -> Self }` + `#[derive(Default)]` |
| `Remote { … 11 kenttää }` kolmesti | `annex.rs:1291-1360`, testit | `Remote::new(uuid, description, config, trust, last_fsck)` + `Default` |
| "Last write wins per uuid" -silmukka viidesti | `parse_uuid_log`, `parse_remote_log`, `parse_trust_log`, `parse_group_log`, `parse_content_log` | `fn latest_per_uuid<T>(text, parse_body: impl Fn(&str) -> Option<T>) -> HashMap<String, T>`; `keep_latest` on jo olemassa mutta käytössä vain `parse_count_log`-funktiossa |
| Hakemistopuun rakennus (subdirs + direct_files) kolmesti | `FilesOnDriveNode`, `DirectoryNode`, `AllFilesNode` (`node.rs`) | Yksi `fn split_dir_level(files, prefix, filter) -> (BTreeSet<String>, Vec<AnnexedFile>)`; tai rakenna `AllFilesNode`/`FilesOnDriveNode` suoraan `DirectoryNode { dir_path: "" }`-solmuina — ne ovat käytännössä sama solmu eri suodattimella |
| `AnnexFileNode`-tilapäisolion luonti `details()`/`raw_text()`-kutsua varten | `usage.rs:316-337` | Siirrä sijaintilistan muotoilu vapaaksi funktioksi `fn file_details(meta, file, highlight) -> Vec<String>` |
| Discovery + lataus + merge kolmessa polussa | `run_scan`, `--dump`-haara, `worker::spawn` | Yksi `scan::run(root, jobs, on_progress) -> Vec<(PathBuf, Result<AnnexMetadata>)>` rinnakkaisella lataajalla (`std::thread::scope`, N työntekijää), jota kaikki kolme käyttävät. Nyt `--scan` ja `--dump` lataavat repot peräkkäin, TUI kahdella säikeellä. |

### 5.2 `kind()`-merkkijonot enumiksi

**Havainto.** Solmutyyppi on `&'static str` (`"repo"`, `"drive"`, `"here"`,
`"report"`, `"viz"`, `"usage"`, `"dir"`, `"parent"`, `"file"`, `"files"`,
`"info"`, `"drives"`, `"root"`) ja sitä verrataan literaaleihin `app.rs`,
`main.rs` ja `tui.rs` -tiedostoissa. Kirjoitusvirhe ei näy käännöksessä eikä
`match` ole tyhjentävä.

**Toteutus.**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind { Root, Report, Repo, Here, Drive, Drives, Info, Files, Dir, File, Usage, Viz, Parent }
impl NodeKind { pub fn label(self) -> &'static str { … } }
```

`ListItem.kind: NodeKind`; `tui.rs`-värit ja `visual_for` matchaavat enumiin.
Suodatin käyttää `kind.label()`.

### 5.3 UI-tila omaksi tyypikseen

**Havainto.** `main()` pitää kahdeksaa irrallista muuttujaa (`show_help`,
`show_raw`, `zoom`, `filter`, `filter_editing`, `detail_scroll`, `pending`,
`snapshot`) ja näppäinkäsittely on 130 rivin `match`, jossa `x`, `z` ja `/`
käsitellään ennen `map_key`-kutsua. `Command::ToggleRaw` on määritelty
(`keyboard.rs:17`) mutta ei koskaan saavuta `execute`-funktiota.

**Toteutus.**

```rust
struct UiState { snapshot: Option<ViewSnapshot>, pending: usize, help: bool, raw: bool,
                 zoom: bool, filter: Filter, detail_scroll: usize }
enum UiAction { Redraw, Send(WorkerMsg), Quit, Nothing }
impl UiState { fn handle_key(&mut self, key: KeyEvent, page: usize) -> UiAction }
```

Silloin näppäinkäsittely on puhdas funktio, jota voi yksikkötestata ilman
terminaalia (esim. "q ohjenäkymässä sulkee ohjeen", "Esc suodattimessa tyhjentää
suodattimen"). `keyboard::map_key` voi palauttaa `UiCommand`-enumin, jossa on
myös `ToggleRaw`, `Zoom`, `StartFilter`, `ScrollDetails(±)`.

### 5.4 Kuollut koodi ja vanhentuneet kommentit

- `available_space` / `here_available_space` / `fill_drive_spaces` / `get_fs_space`
  (`annex.rs:1463-1510`): lasketaan `statvfs`-kutsulla jokaisella latauksella,
  mutta kommentin mukaan "no longer displayed". Joko näytä vapaa tila liitettyjen
  `directory`-remotejen kohdalla (hyödyllinen tieto, data on jo olemassa) tai
  poista koodi ja `libc`-riippuvuus `statvfs`-osalta.
- `save_cache` on `#[allow(dead_code)]` (`annex.rs:1676`).
- `AnnexMetadata.version` on aina `None`.
- `AnnexFileNode::label` laskee `_locs`-muuttujan, jota ei käytetä (`node.rs:824`).
- `UsageChild.rel_path` ja `file_idx` — `file_idx` menee rikki, jos `files`-
  vektoria joskus suodatetaan; harkitse `Rc<AnnexedFile>`-viittausta.
- `app.rs:581-583`: kommentti downcastingista ei vastaa koodia.
- `App::snapshot(&self, _page)` ja `WorkerMsg::Nav(cmd, page)`: `page` kulkee
  työntekijälle mutta sitä ei käytetä (`app.rs:80`).

## 6. Käytettävyys

- **`Esc`-semantiikka** (ks. 3.3): sulje päällimmäinen tila, älä lopeta.
- **Lajittelu juurilistassa.** Nyt aina polun mukaan. Lisää `s`-näppäin, joka
  kiertää `name → size → health → files`. `VisualReport` lajittelee jo koon
  mukaan, joten lista ja visuaali ovat eri järjestyksessä.
- **Vaihtoehto Shift+PgUp/PgDn-näppäimille.** Monet terminaalit (tmux, jotkin
  emulaattorit) eivät välitä Shift-modifieria PgUp/PgDn-näppäimille. Lisää
  `J`/`K` tai `Ctrl+d`/`Ctrl+u` yksityiskohtapaneelin vieritykseen.
- **Leveysturvallinen katkaisu.** `trunc_name` (`tui.rs`) laskee `chars().count()`,
  ei näyttöleveyttä; CJK- ja emoji-nimet rikkovat sarakkeet. `ratatui` tuo jo
  `unicode-width`-crate:n mukanaan: käytä `UnicodeWidthStr::width`.
- **Hiirituki** (`crossterm::event::EnableMouseCapture`): klikkaus valitsee rivin,
  rulla vierittää. Pieni lisä, iso mukavuus.
- **Copy-health-selite** näkyy vain visuaalissa. Lisää `↓`-merkki juurilistan
  riveille, joilla `keys_under > 0`, jotta ongelmarepot erottuvat ilman visuaalia.
- **Riskitiedostot** (README:n "Future Ideas"): lista avaimista, jotka ovat vain
  untrusted-remoteilla tai alle numcopies — `RepoNode::children`-listaan uusi
  `RiskFilesNode`, joka suodattaa `locations`-mapin `copy_health_counts`-logiikalla.
  Tämä on työkalun luonteva ydinominaisuus (offline-levyjen turvallisuus).
- **"Mitä levyjä pitää kytkeä"**: valitulle repolle/hakemistolle lista remoteista,
  joilla on sisältöä jota `here` ei omista. Data on jo `locations`-mapissa.

## 7. Testaus ja CI

**Havainto.**
- `app.rs`, `worker.rs`, `node.rs`, `tui.rs` ja `main.rs` ovat kokonaan
  testaamatta. Navigointilogiikassa (`replace_open_repo`, `apply_background_snapshot`,
  `apply_nav` + suodatin) on eniten reunatapauksia.
- `test_find_and_load_demo` (`annex.rs:2248`) riippuu käsin luodusta
  `/tmp/annex-demo`-hakemistosta ja on muuten hiljainen no-op.
- `load_annexed_files_includes_dropped_and_missing_with_annex_size` ohitetaan, jos
  `git-annex` puuttuu — CI:n `ubuntu-latest`-kuvassa se puuttuu, joten ainoa
  oikeaa git-annexia vasten ajettava testi ei koskaan aja CI:ssä.
- Tilapäishakemistot tehdään käsin `temp_dir()`-kutsulla ja siivotaan `remove_dir_all`-
  kutsulla, joka jää ajamatta jos assert paniikkii.

**Toteutus.**
- Lisää CI:hin `sudo apt-get install -y git-annex` ennen `cargo test` ja tee
  testistä pakollinen (`panic!` jos annex puuttuu ja `CI`-ympäristömuuttuja on
  asetettu).
- Poista `test_find_and_load_demo` tai muuta se käyttämään samaa
  fixture-generaattoria kuin ncdu-testi; eriytä generaattori `tests/common.rs`-
  moduuliin ja lisää fixture, jossa on kaksi kloonia + `directory`-remote +
  `dead`-merkitty remote, jotta `omit_dead_remotes` ja `aggregate_remote_usage`
  testataan oikeaa dataa vasten.
- `tempfile`-crate dev-riippuvuudeksi (siivous myös paniikissa).
- Yksikkötestit `App`-logiikalle: rakenna `AnnexMetadata` käsin (kuten
  `dummy_meta`), aja `ingest_meta` → `execute(Descend)` → `replace_open_repo` ja
  tarkista pino. `UiState::handle_key` (5.3) testataan ilman terminaalia.
- Snapshot-testit renderöinnille `ratatui::backend::TestBackend`-taustalla
  (`insta`-crate), vähintään juurinäkymälle ja usage-listalle.
- CI: lisää `cargo audit` tai `cargo deny check` ja `cargo build --release`
  release-workflow'na (`cargo-dist` tuottaa binaarit Linux/macOS-alustoille
  tageista). Ohjelma ei ole crates.io:ssa; `cargo publish` on ilmainen näkyvyys.

## 8. CLI ja skriptattavuus

| Lippu | Tarkoitus |
|-------|-----------|
| `--json` (yhdessä `--dump`) | Konekielinen tuloste; sama data kuin `RepoSummary` + remote-rivit. Mahdollistaa esim. Prometheus/Grafana-integraation cronista. |
| `--cache <path>` | Nyt vain ympäristömuuttuja `GIT_ANNEX_BROWSER_CACHE`. |
| `--no-scan` / `--offline` | Näytä pelkkä välimuisti, älä koske levyihin. Tärkeä nimenomaan offline-käytössä ja hitailla verkkolevyillä. |
| `--jobs N` | `MAX_HYDRATE` on kovakoodattu 2 (`worker.rs:123`); `--scan` on täysin sarjallinen. |
| `--max-depth N`, `--one-file-system` | Rajaa discoveryä (ks. 1.5). |
| `--force-rescan` | Ohita haara-sha-tarkistus (ks. 2.3). |
| `--prune` | Poista välimuistista repot, joita ei löydy (ks. 2.4). |

`--dump`-tulosteen otsikkorivit (`REPORT:` jne.) kannattaa vakioida, jos niitä
parsitaan skripteissä; `--json` on kestävämpi ratkaisu.

## 9. Riippuvuudet ja pienet siivoukset

- `chrono` (0.4.45) on mukana vain `fmt_unix`-funktion takia (`util.rs:21-27`).
  `time`-crate tai `jiff` on kevyempi, tai muotoile käsin (UTC, ei aikavyöhykkeitä).
  Harkitse myös paikallisen ajan näyttämistä — nyt fsck-ajat ovat UTC:ssä ilman
  merkintää.
- `libc`: `flock` ja `statvfs`. `std::fs::File::lock` on vakaa Rust 1.89:stä
  alkaen (projekti vaatii jo edition 2024 → rustc ≥ 1.85, ja `Cargo.toml`-
  tiedostoon kannattaa lisätä `rust-version`). Kun `statvfs` poistetaan (5.4),
  `libc` voi lähteä kokonaan.
- `Cargo.toml`: lisää `rust-version = "1.89"` ja `[lints] clippy = { pedantic =
  "warn" }` -osio, jos halutaan tiukempi laatu. `README.md` mainitsee "recent stable
  rustc" mutta ei versiota.
- `.gitignore` sisältää yleisiä rivejä (`*.o`, `*.dll`), jotka eivät koske
  projektia; ei haittaa, mutta kannattaa siistiä.
- `README.md`: `x`-näppäin toimii myös usage-listassa (`UsageDirNode::raw_text`
  antaa TSV-muotoisen listan) — dokumentoi; se on hyödyllinen `xclip`-liitäntään.

## Ehdotettu toteutusjärjestys

1. **Nopeat korjaukset (1 ilta):** 1.1 (`OnceCell` usage-puulle), 3.1 (uuid-
   tarkistus), 3.2 (UTF-8), 3.3 (`q` ohjeessa), 5.4 (kuollut koodi), 1.4
   (dirty-piirto).
2. **Välimuisti v2 (1–2 päivää):** 2.1 per-repo-tiedostot + `Arc` (2.2) +
   haara-sha-tarkistus (2.3) + `--offline`/`--force-rescan`. Tämä on suurin
   käyttäjälle näkyvä parannus: käynnistys välitön, ei `[busy]`-jäätymisiä.
3. **Työntekijän rakenne (1 päivä):** 1.3 discovery ja on-demand-lataus omiin
   säikeisiin, `whereis`-fallback pois, yhteinen `scan::run` (5.1 viimeinen rivi).
4. **Rakennesiivous (1–2 päivää):** 5.2 `NodeKind`, 5.3 `UiState`, 5.1 toistot.
   Tee tämä ennen uusia ominaisuuksia, jotta ne eivät kasvata `main()`-matchia.
5. **Testit ja CI (½ päivää):** 7.
6. **Ominaisuudet:** riskitiedostot, lajittelu, `--json`, hiiri, muisti (4.1)
   vasta jos suuret annexit (> 500 k avainta) tulevat vastaan.
