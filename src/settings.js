const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

const MODE_LABELS = {
  music: 'Musica',
  controls: 'Command Center',
  pc: 'PC (CPU, RAM, disco)',
  status: 'Ora, meteo e batteria',
  calendar: 'Calendario e impegni di Outlook',
  shelf: 'Mensola (file da trascinare, anche verso il telefono)',
};
const ALL_MODES = Object.keys(MODE_LABELS);

let s = null;

/* ---------- save (debounced) ---------- */
let saveTimer = null;
function save() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      s = await invoke('save_settings', { settings: s });
      const el = $('saved');
      el.classList.add('show');
      setTimeout(() => el.classList.remove('show'), 1200);
    } catch (e) {
      alert(`Impossibile salvare: ${e}`);
    }
  }, 250);
}

/* ---------- render ---------- */
function renderModes() {
  const box = $('modes');
  box.textContent = '';
  // keep the user's order first, then the disabled ones
  const order = [...s.modes, ...ALL_MODES.filter((m) => !s.modes.includes(m))];
  for (const m of order) {
    const label = document.createElement('label');
    label.className = 'check';
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = s.modes.includes(m);
    cb.addEventListener('change', () => {
      if (cb.checked) s.modes = ALL_MODES.filter((x) => x === m || s.modes.includes(x));
      else if (s.modes.length > 1) s.modes = s.modes.filter((x) => x !== m);
      else cb.checked = true; // at least one view must stay
      save();
    });
    label.append(cb, ` ${MODE_LABELS[m]}`);
    box.appendChild(label);
  }
}

function renderCalendarStatus(a) {
  const el = $('calendar-status');
  if (!a || !a.configured) {
    el.textContent = 'Nessun calendario collegato.';
    el.className = 'hint';
    return;
  }
  const when = a.updatedMs ? new Date(a.updatedMs).toLocaleTimeString('it-IT', { hour: '2-digit', minute: '2-digit' }) : null;
  if (a.error) {
    const retry = s?.calendarSource === 'outlook' ? 'tre minuti' : 'un minuto';
    el.textContent = `${a.error}${when ? ` · ultimo aggiornamento riuscito alle ${when}` : ''}. Nuovo tentativo tra ${retry}.`;
    el.className = 'hint error';
  } else {
    const every = s?.calendarSource === 'outlook' ? 10 : 5;
    el.textContent = `Collegato · ${a.events.length} impegni · aggiornato alle ${when}. Si aggiorna ogni ${every} minuti.`;
    el.className = 'hint ok';
  }
}

function renderCity() {
  $('city-current').textContent = s.weatherName
    ? `Città attuale: ${s.weatherName}`
    : 'Nessuna città impostata: il meteo non viene mostrato.';
}

async function init() {
  s = await invoke('get_settings');

  renderModes();

  $('accent-mode').value = s.accentMode;
  $('accent-color').value = s.accentColor;
  $('accent-color').disabled = s.accentMode !== 'fixed';
  $('accent-mode').addEventListener('change', (e) => {
    s.accentMode = e.target.value;
    $('accent-color').disabled = s.accentMode !== 'fixed';
    save();
  });
  $('accent-color').addEventListener('input', (e) => {
    s.accentColor = e.target.value;
    save();
  });

  const scale = $('scale');
  scale.value = Math.round(s.scale * 100);
  $('scale-val').textContent = `${scale.value}%`;
  scale.addEventListener('input', () => {
    s.scale = Number(scale.value) / 100;
    $('scale-val').textContent = `${scale.value}%`;
    save();
  });

  const pos = $('position');
  pos.value = Math.round((s.position ?? 0.5) * 100);
  pos.addEventListener('input', () => {
    s.position = Number(pos.value) / 100;
    save();
  });
  $('position-center').addEventListener('click', () => {
    pos.value = 50;
    s.position = 0.5;
    save();
  });
  // The notch can be dragged while this window is open: keep the position in sync.
  window.__TAURI__.event.listen('settings-changed', ({ payload }) => {
    if (!s || payload.position == null) return;
    s.position = payload.position;
    if (document.activeElement !== pos) pos.value = Math.round(payload.position * 100);
  });

  const monitors = await invoke('list_monitors');
  const sel = $('monitor');
  sel.append(new Option('Monitor principale', ''));
  monitors.forEach((m, i) => sel.append(new Option(`Monitor ${i + 1} (${m.replace(/^\\\\\.\\/, '')})`, m)));
  sel.value = monitors.includes(s.monitor) ? s.monitor : '';
  sel.addEventListener('change', () => {
    s.monitor = sel.value;
    save();
  });

  const bind = (id, key) => {
    $(id).checked = s[key];
    $(id).addEventListener('change', (e) => {
      s[key] = e.target.checked;
      $('notification-preview').disabled = !s.notifications;
      save();
    });
  };
  bind('hide-fullscreen', 'hideFullscreen');
  bind('hide-in-share', 'hideInShare');
  bind('notifications', 'notifications');
  bind('notification-preview', 'notificationPreview');
  bind('device-popups', 'devicePopups');
  bind('download-popups', 'downloadPopups');
  $('notification-preview').disabled = !s.notifications;

  const text = (id, key) => {
    $(id).value = s[key] || '';
    $(id).addEventListener('change', (e) => {
      s[key] = e.target.value.trim();
      save();
    });
  };
  text('calendar-url', 'calendarUrl');
  const source = $('calendar-source');
  const showSource = () => {
    const ics = source.value === 'ics';
    $('calendar-url-row').classList.toggle('hidden', !ics);
    $('calendar-ics-help').classList.toggle('hidden', !ics);
    $('calendar-outlook-hint').classList.toggle('hidden', source.value !== 'outlook');
    $('calendar-refresh').disabled = source.value === 'off';
  };
  source.value = s.calendarSource || 'outlook';
  showSource();
  source.addEventListener('change', () => {
    s.calendarSource = source.value;
    showSource();
    $('calendar-status').textContent = source.value === 'off' ? '' : 'Lettura del calendario…';
    save();
  });
  text('git-root', 'gitRoot');
  $('calendar-refresh').addEventListener('click', () => {
    $('calendar-status').textContent = 'Aggiornamento…';
    invoke('refresh_agenda');
  });
  window.__TAURI__.event.listen('agenda', ({ payload }) => renderCalendarStatus(payload));
  renderCalendarStatus(await invoke('agenda_snapshot'));

  renderCity();
  $('city-search').addEventListener('click', searchCity);
  $('city').addEventListener('keydown', (e) => e.key === 'Enter' && searchCity());

  try {
    $('app-version').textContent = await window.__TAURI__.app.getVersion();
  } catch {
    /* version not available */
  }
  $('update-check').addEventListener('click', async () => {
    const st = $('update-status');
    st.textContent = 'Controllo in corso…';
    st.className = 'hint';
    try {
      const u = await invoke('check_update');
      if (u) {
        st.textContent = `Disponibile la versione ${u.version}.`;
        st.className = 'hint ok';
        $('update-install').classList.remove('hidden');
      } else {
        st.textContent = 'Hai già l\'ultima versione.';
        st.className = 'hint ok';
      }
    } catch (e) {
      st.textContent = String(e);
      st.className = 'hint error';
    }
  });
  $('update-install').addEventListener('click', async () => {
    $('update-status').textContent = 'Download e installazione… Lumen si riavvierà.';
    try {
      await invoke('install_update');
    } catch (e) {
      $('update-status').textContent = String(e);
      $('update-status').className = 'hint error';
    }
  });

  try {
    $('autostart').checked = await invoke('plugin:autostart|is_enabled');
  } catch {
    $('autostart').disabled = true;
  }
  $('autostart').addEventListener('change', async (e) => {
    try {
      await invoke(e.target.checked ? 'plugin:autostart|enable' : 'plugin:autostart|disable');
    } catch (err) {
      e.target.checked = !e.target.checked;
      alert(`Impossibile modificare l'avvio automatico: ${err}`);
    }
  });
}

/* ---------- weather city lookup (Open-Meteo geocoding) ---------- */
async function searchCity() {
  const q = $('city').value.trim();
  const box = $('city-results');
  box.textContent = '';
  if (q.length < 2) return;
  try {
    const url = `https://geocoding-api.open-meteo.com/v1/search?name=${encodeURIComponent(q)}&count=5&language=it&format=json`;
    const data = await (await fetch(url)).json();
    if (!data.results?.length) {
      box.textContent = 'Nessuna città trovata.';
      return;
    }
    for (const r of data.results) {
      const btn = document.createElement('button');
      const where = [r.admin1, r.country].filter(Boolean).join(', ');
      btn.textContent = `${r.name}${where ? ` — ${where}` : ''}`;
      btn.addEventListener('click', () => {
        s.weatherName = r.name;
        s.weatherLat = r.latitude;
        s.weatherLon = r.longitude;
        box.textContent = '';
        $('city').value = '';
        renderCity();
        save();
      });
      box.appendChild(btn);
    }
  } catch {
    box.textContent = 'Ricerca non disponibile (connessione?).';
  }
}

init();
