# Lumen

Notch dinamica per Windows: una "pillola" in cima allo schermo che si espande al passaggio del mouse e mostra musica, controlli di sistema, risorse del PC, meteo e le riunioni di Outlook. Animazioni a molla, colori presi dalla copertina del brano, visualizzatore audio reale, notifiche e popup dei dispositivi.

Costruita con **Tauri 2** (backend Rust) e un frontend HTML/CSS/JS senza bundler.

## Funzionalità

| Vista | Cosa fa |
|---|---|
| **Notch chiusa** | Prossima riunione ("Riunione tra 10 min" + titolo) se manca meno di un'ora, oppure il brano in riproduzione con mini-visualizzatore, oppure l'ora. Negli ultimi 15 minuti e nei primi 10 della riunione questa ha la precedenza sulla musica. Al cambio brano si allarga per qualche secondo con titolo e artista. |
| **Musica** | Copertina, titolo, artista, app sorgente, barra di avanzamento cliccabile/trascinabile per spostarsi nel brano (se il player la fornisce), precedente / play-pausa / successivo, visualizzatore a 12 bande. |
| **Command Center** | Wi-Fi (rete connessa, on/off), Bluetooth (dispositivi connessi, on/off), Screenshot (apre lo strumento di cattura di Windows, come Win+Maiusc+S, dopo aver richiuso la notch), **Git pull** (porta ogni repository della cartella progetti su `main`/`master` e fa `git pull --rebase`), accesso alle impostazioni, volume con muto, luminosità dello schermo del portatile. |
| **PC** | Anelli CPU e RAM, modello del processore, memoria usata, spazio libero sul disco di sistema, velocità di rete in download/upload, da quanto è acceso il PC. Campionato solo mentre la vista è aperta. |
| **Stato** | Ora, data, meteo della città scelta (Open-Meteo: ogni 15 minuti, nuovo tentativo dopo 1 minuto se fallisce, aggiornato all'apertura della vista se vecchio; senza rete resta l'ultimo valore valido), batteria come piccola icona con percentuale (verde con fulmine in carica, rossa sotto il 20%). |
| **Calendario** | Mese corrente con un punto sui giorni che hanno impegni; a destra gli impegni del giorno scelto (oggi di default, clic su un giorno per cambiarlo) con il pulsante **Partecipa** per Teams/Zoom/Meet. Quando una riunione è vicina, aprendo la notch compare direttamente questa vista. |

Popup automatici (la notch si allarga per qualche secondo):

- **Notifiche di Windows**: Outlook, Teams, Slack… con l'icona vera dell'app (la stessa del menu Start, presa da `shell:AppsFolder`), titolo e testo (il testo si può nascondere). Le notifiche di Outlook classico mentre Lumen lo usa per leggere il calendario vengono ignorate, per non duplicare quelle del nuovo Outlook. **Cliccabili**: passandoci sopra il popup resta aperto, cliccandolo si apre l'app che l'ha inviata.
- **Riunioni**: un minuto prima dell'inizio un popup con il titolo; cliccandolo si entra nella riunione (i link di Teams si aprono direttamente nell'app Teams).
- **Git pull**: a fine aggiornamento un riepilogo (aggiornati, già aggiornati, saltati, errori); cliccandolo si apre il log dettagliato.
- **Dispositivi**: Bluetooth connesso/disconnesso (con la batteria, se Windows la conosce), chiavetta USB inserita/rimossa, alimentatore collegato/scollegato.
- **Download completati**: nome, dimensione e icona del file, con i pulsanti **Apri** e **Mostra** (nella cartella). Controlla la cartella Download di Windows; i file temporanei dei browser (.crdownload, .part…) vengono ignorati.
- **Aggiornamenti**: quando su GitHub esce una versione nuova, un popup con **Installa**.
- **Volume e luminosità** cambiati da tastiera o da Windows.

Altro:

- **Schermo intero**: la notch si nasconde durante giochi, video e presentazioni a schermo intero sul suo monitor.
- **Posizione**: la notch si trascina a destra o a sinistra lungo il bordo superiore tenendo premuto il tasto sinistro del mouse (non sui pulsanti); la posizione viene salvata. Si può regolare anche dalle impostazioni ("Posizione", con il pulsante "Centra").
- **Calendario di Outlook** (Impostazioni → Calendario di Outlook → Origine):
  - **Outlook classico (predefinito)**: il nuovo Outlook non ha un'API, ma Outlook classico usa lo stesso account. Ogni 10 minuti Lumen esegue in modo nascosto `outlook-calendar.ps1` (COM): se Outlook non è aperto lo avvia invisibile, lo sincronizza per ~20 secondi, legge gli impegni (da 40 giorni fa a 2 mesi avanti, escluse riunioni annullate o rifiutate) e lo richiude; se è già aperto lo usa senza chiuderlo. Serve il profilo di Outlook classico configurato.
  - **Link ICS pubblicato**: Outlook sul web → Impostazioni → Calendario → Calendari condivisi → Pubblica un calendario → link ICS. Ogni 5 minuti. Il link dà accesso in lettura a chiunque lo conosca.
  - In caso di errore restano gli ultimi dati validi e si riprova (3 minuti per Outlook, 1 per il link).
- **Git pull**: per ogni repository direttamente dentro la cartella scelta (default `D:\Project`), quattro alla volta: salta quelli con modifiche non committate o rebase/merge in corso; se è su un altro branch (o in HEAD staccato) passa a `main`, oppure a `master` se `main` non esiste, e ci resta; poi esegue `git pull --rebase` e, se va in conflitto, `git rebase --abort` (il repository resta com'era). Log in `%APPDATA%\dev.lumen.notch\git-pull.log`.
- **Istanza singola**: avviare Lumen una seconda volta apre le impostazioni invece di creare una seconda notch.
- **Impostazioni** (icona nella tray → "Impostazioni…", oppure il riquadro nel Command Center): viste attive, colore (dalla copertina o fisso), dimensione, monitor, posizione, schermo intero, notifiche, popup, città del meteo, link ICS del calendario di Outlook, cartella dei repository Git, avvio automatico.

Uso: passa il mouse sulla notch per aprirla, usa la rotellina per cambiare vista. Per chiudere Lumen: icona nella system tray → "Esci da Lumen".

## Aggiornamenti automatici e versioni

Il repository è [github.com/Azzo00/Lumen](https://github.com/Azzo00/Lumen). Ogni tag `v*` avvia la GitHub Action [`release.yml`](.github/workflows/release.yml), che esegue i test, costruisce l'installer firmato e lo pubblica nelle **Release** insieme a `latest.json`.

Lumen controlla `https://github.com/Azzo00/Lumen/releases/latest/download/latest.json` un minuto dopo l'avvio e poi ogni 6 ore (oppure Impostazioni → Sistema → "Controlla aggiornamenti"). Se c'è una versione nuova mostra un popup; "Installa" scarica l'installer, ne verifica la firma con la chiave pubblica in `tauri.conf.json`, lo esegue e riavvia Lumen.

Per pubblicare una versione:

```bash
# 1. stesso numero in package.json, src-tauri/tauri.conf.json, src-tauri/Cargo.toml
# 2. commit, poi:
git tag v0.4.1
git push origin main --tags
```

**Chiave di firma**: la chiave privata è in `%USERPROFILE%\.tauri\lumen-updater.key` (senza password) e nel secret `TAURI_SIGNING_PRIVATE_KEY` del repository. Non va mai messa nel repository. Se si perde, le installazioni esistenti non accettano più aggiornamenti: bisogna generarne una nuova (`npx tauri signer generate`), aggiornare `pubkey` e reinstallare a mano una volta.

## Requisiti per lo sviluppo

- Windows 10/11 (WebView2 è già incluso in Windows 11)
- [Rust](https://rustup.rs/) (toolchain `stable-x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools con il carico di lavoro **C++** (MSVC + Windows SDK)
- Node.js + npm (solo per la CLI di Tauri)

Installazione via winget:

```bash
winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup
```

## Comandi

```bash
npm install        # installa la CLI di Tauri
npm run dev        # avvio in sviluppo, ricompila a ogni modifica
npm run build      # crea l'installer
```

`npm run build` firma anche l'installer per l'aggiornamento automatico, quindi in locale servono le variabili della chiave:

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content "$env:USERPROFILE\.tauri\lumen-updater.key" -Raw
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""
npm run build
```

L'installer viene generato in:

```
src-tauri/target/release/bundle/nsis/Lumen_<versione>_x64-setup.exe
```

Installa per l'utente corrente in `%LOCALAPPDATA%\Lumen\`, senza permessi di amministratore; si disinstalla da Impostazioni → App. Reinstallare una versione nuova sopra la vecchia la aggiorna (`Lumen_<versione>_x64-setup.exe /S` per un'installazione silenziosa). La cartella `target/` è esclusa da git: per distribuire l'installer usa le **Release** di GitHub.

Per cambiare versione aggiorna lo stesso numero in `package.json`, `src-tauri/tauri.conf.json` e `src-tauri/Cargo.toml`.

> Nota: l'avvio automatico va attivato dalla versione installata. In `npm run dev` registrerebbe l'eseguibile di debug, che da solo non funziona.

## Struttura

```
src/                     frontend (caricato così com'è, niente build step)
  index.html             viste della notch
  style.css              stile della notch
  app.js                 molle, viste, interazione, colori, meteo, popup
  settings.html/.css/.js finestra delle impostazioni
  logo.png               logo usato nelle impostazioni
src-tauri/
  tauri.conf.json        finestra trasparente 600×340, sempre in primo piano, fuori dalla taskbar; CSP
  capabilities/          permessi (core + autostart) per le finestre main e settings
  src/main.rs            avvio, posizionamento sul monitor scelto, tray, plugin, comandi
  src/settings.rs        impostazioni (JSON in %APPDATA%\dev.lumen.notch), finestra impostazioni
  src/hover.rs           click-through (la finestra cattura il mouse solo sopra la pillola) e trascinamento
  src/fullscreen.rs      nasconde la notch con app a schermo intero
  src/media.rs           sessione multimediale di Windows (titolo, copertina, timeline, comandi)
  src/audio.rs           cattura WASAPI loopback + FFT → evento `audio`
  src/system.rs          volume, luminosità (WMI), radio Wi-Fi/Bluetooth
  src/devices.rs         popup Bluetooth (con batteria) e chiavette USB
  src/notify.rs          notifiche di Windows (UserNotificationListener)
  src/pcstats.rs         CPU, RAM, disco, rete, uptime (sysinfo)
  src/agenda.rs          calendario: Outlook classico (COM) o link ICS (ricorrenze, fusi orari di Windows), link Teams
  src/outlook-calendar.ps1  lettura del calendario di Outlook classico via COM (incluso nell'eseguibile)
  src/git.rs             git pull --rebase di tutti i repository della cartella progetti
  src/launch.rs          apertura di app (AUMID), link delle riunioni, file
  src/icons.rs           icona di un'app o di un file (come in Esplora file) in PNG
  src/downloads.rs       popup dei download completati (cartella Download)
  src/updater.rs         aggiornamenti automatici da GitHub Releases
.github/workflows/release.yml  build, test e Release a ogni tag v*
```

### Come funziona

- **Finestra**: trasparente, centrata in alto sul monitor scelto, con `WS_EX_NOACTIVATE` (cliccarla non toglie il focus all'app in uso). Ignora il mouse finché il cursore non entra nel rettangolo della pillola, che il frontend comunica con `set_hit_rect`; un thread Rust controlla il cursore ogni 30 ms ed emette `hover`.
- **Eventi backend → frontend**: `media`, `audio`, `system`, `hover`, `notification`, `device`, `agenda`, `git-progress`, `git-done`, `download`, `update-available`, `settings-changed`. Per gli stati emessi prima che la pagina sia pronta ci sono `media_snapshot`, `system_snapshot` e `agenda_snapshot`.
- **Comandi**: `media_command`, `set_volume`, `toggle_mute`, `set_brightness`, `set_radio`, `set_hit_rect`, `drag_start`, `take_screenshot`, `agenda_snapshot`, `refresh_agenda`, `git_pull_all`, `open_git_log`, `open_app`, `open_link`, `open_download`, `reveal_download`, `check_update`, `install_update`, `pc_stats`, `get_settings`, `save_settings`, `list_monitors`, `open_settings`, più `plugin:autostart|*`.
- **Notifiche**: lette con `UserNotificationListener` ogni 1,5 s (l'evento di cambio richiede un'app pacchettizzata). Il banner di Windows non viene soppresso: per non vederlo due volte si disattivano i banner delle app in Windows lasciando attivo il centro notifiche.
- **Schermo intero**: `SHQueryUserNotificationState` (giochi D3D, presentazioni) più un controllo sulla finestra in primo piano senza barra del titolo che copre tutto il monitor.
- **Risorse**: il visualizzatore smette di emettere quando non c'è audio; la vista PC viene campionata solo quando è aperta; luminosità ogni 2 s; radio, Wi-Fi e dispositivi ogni 2–3 s; meteo ogni 15 minuti.

## Limiti noti

- La luminosità funziona solo sullo schermo del portatile (WMI). I monitor esterni richiedono DDC/CI, non ancora implementato.
- "Non disturbare" non è disponibile: Windows non offre un'API pubblica.
- La barra di avanzamento compare solo se il player fornisce la durata (Spotify sì, molti browser no).
- La batteria dei dispositivi Bluetooth compare solo se Windows la mostra anche nelle sue Impostazioni.
- Calendario via link ICS: serve che l'amministratore di Microsoft 365 permetta di pubblicare il calendario (con l'account di lavoro attuale non è possibile: si usa Outlook classico). Le ricorrenze coperte sono quelle che usa Outlook (giornaliera, settimanale, mensile, annuale, con giorni/ordinali/eccezioni); i fusi orari non Windows (es. `Europe/Rome`) vengono trattati come ora locale.
- Clic sulle notifiche: apre l'app, non la chat o la mail precisa (Windows non espone il comando della notifica alle altre app).
- Test automatici: `cargo test` in `src-tauri` (calendario e git su repository di prova).
- Non ancora testati a fondo: avvio invisibile di Outlook classico quando è chiuso, calendario con un link ICS reale, clic sui popup, interruttori Wi-Fi/Bluetooth, popup Bluetooth/USB/alimentatore, Pomodoro, nascondimento a schermo intero, avvio automatico.

## Roadmap

1. **Scorciatoia da tastiera** per aprire la notch.
2. **Appunti**: ultimi testi copiati.
3. **Temperatura CPU/GPU** nella vista PC (richiede driver o permessi di amministratore).
4. **Luminosità monitor esterni** via DDC/CI.
