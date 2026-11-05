const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const pill = $('pill');

// Views reachable with the wheel; replaced by the user's choice in the settings.
let MODES = ['music', 'controls', 'pc', 'status', 'calendar'];
let settings = null;
// [width, height, radius] of the pill in each state
const SIZE = {
  collapsed: [200, 34, 17],
  peek: [300, 46, 23],
  osd: [260, 34, 17],
  toast: [380, 64, 26],
  toastWide: [450, 64, 26],
  music: [420, 196, 32],
  controls: [480, 214, 30],
  pc: [430, 172, 30],
  status: [440, 130, 30],
  calendar: [500, 222, 30],
  meeting: [250, 40, 20],
};
const PILL_TOP = 6;
const ICON = {
  play: '<svg viewBox="0 0 24 24"><path d="M8 5v14l11-7z"/></svg>',
  pause: '<svg viewBox="0 0 24 24"><path d="M6 5h4v14H6zM14 5h4v14h-4z"/></svg>',
};

let mode = 0;
let expanded = false;
let hovering = false;
let peeking = false;
let collapseTimer = null;

/* =====================================================================
   Spring physics: every animated value is a damped spring
   ===================================================================== */
function spring(v, k, c) {
  return { v, t: v, vel: 0, k, c };
}
function stepSpring(s, dt) {
  const accel = -s.k * (s.v - s.t) - s.c * s.vel;
  s.vel += accel * dt;
  s.v += s.vel * dt;
  return Math.abs(s.v - s.t) > 0.05 || Math.abs(s.vel) > 0.05;
}

// Shape: slightly under-damped so it overshoots a touch, like a physical object.
const shape = { w: spring(200, 320, 22), h: spring(34, 320, 22), r: spring(17, 320, 22) };

// Visualizer bars: stiffer springs, they chase the audio levels.
const bigBars = [...document.querySelectorAll('#big-bars i')];
const miniBars = [...document.querySelectorAll('#mini-bars i')];
const barSprings = [...bigBars, ...miniBars].map(() => spring(0, 600, 26));
let levels = new Array(12).fill(0);

let lastFrame = performance.now();
function frame(now) {
  const dt = Math.min((now - lastFrame) / 1000, 1 / 30);
  lastFrame = now;

  let moving = false;
  for (const s of Object.values(shape)) moving = stepSpring(s, dt) || moving;
  if (!moving) for (const s of Object.values(shape)) (s.v = s.t), (s.vel = 0);
  pill.style.width = `${shape.w.v}px`;
  pill.style.height = `${shape.h.v}px`;
  pill.style.borderRadius = `${shape.r.v}px`;

  // big view: 12 bands; mini: 5 bands sampled across the spectrum
  bigBars.forEach((_, i) => (barSprings[i].t = levels[i]));
  const miniIdx = [1, 3, 5, 7, 9];
  miniBars.forEach((_, i) => (barSprings[bigBars.length + i].t = levels[miniIdx[i]]));
  barSprings.forEach((s) => stepSpring(s, dt));
  bigBars.forEach((el, i) => (el.style.height = `${3 + Math.max(0, barSprings[i].v) * 41}px`));
  miniBars.forEach((el, i) => (el.style.height = `${3 + Math.max(0, barSprings[bigBars.length + i].v) * 15}px`));

  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

/* =====================================================================
   Layout state
   ===================================================================== */
let osdActive = false;
let toastActive = false;

// Priority: open view > toast > volume popup > new-track peek > collapsed.
function currentState() {
  if (expanded) return MODES[mode];
  if (toastActive) return 'toast';
  if (osdActive) return 'osd';
  return 'collapsed';
}
function currentSize() {
  const state = currentState();
  if (state === 'toast' && currentToast?.buttons) return SIZE.toastWide;
  if (state === 'collapsed' && peeking) return SIZE.peek;
  if (state === 'collapsed' && meetingShown) return SIZE.meeting;
  return SIZE[state];
}

function renderDots() {
  const dots = $('dots');
  if (dots.children.length !== MODES.length) {
    dots.textContent = '';
    MODES.forEach(() => dots.appendChild(document.createElement('b')));
  }
  [...dots.children].forEach((d, i) => d.classList.toggle('on', i === mode));
}

function render() {
  const state = currentState();
  pill.dataset.view = state;
  pill.classList.toggle('peek', peeking && state === 'collapsed');
  pill.classList.toggle('expanded', expanded);
  renderDots();
  const [w, h, r] = currentSize();
  shape.w.t = w;
  shape.h.t = h;
  shape.r.t = r;
  // Tell the backend where the pill is, so it only captures the mouse there.
  // While collapsed we use a slightly taller strip so it's easy to hit the top edge.
  const scale = settings?.scale ?? 1;
  invoke('set_hit_rect', {
    rect: {
      x: (window.innerWidth - w * scale) / 2,
      y: 0,
      w: w * scale,
      h: h * scale + PILL_TOP + (expanded ? 0 : 4),
    },
  });
  onViewChanged();
}

function expand() {
  clearTimeout(collapseTimer);
  if (!expanded) {
    expanded = true;
    render();
  }
}
function scheduleCollapse(delay = 300) {
  clearTimeout(collapseTimer);
  collapseTimer = setTimeout(() => {
    if (hovering) return;
    expanded = false;
    pill.classList.remove('alert');
    if (selDay) {
      selDay = null;
      buildCalendar();
    }
    render();
  }, delay);
}

listen('hover', ({ payload }) => {
  hovering = payload;
  // A clickable popup (notification, meeting, git) stays put under the mouse instead of opening the views.
  if (toastActive && !expanded && currentToast?.action) {
    clearTimeout(toastTimer);
    pill.classList.toggle('toast-hover', hovering);
    if (!hovering) toastTimer = setTimeout(nextToast, 1500);
    return;
  }
  if (hovering) {
    openCalendarForMeeting();
    expand();
  } else scheduleCollapse();
});

let wheelLockUntil = 0;
pill.addEventListener(
  'wheel',
  (e) => {
    if (!expanded) return;
    const now = Date.now();
    if (now < wheelLockUntil) return;
    wheelLockUntil = now + 350;
    mode = (mode + (e.deltaY > 0 ? 1 : -1) + MODES.length) % MODES.length;
    render();
  },
  { passive: true },
);
pill.addEventListener('contextmenu', (e) => e.preventDefault());

/* =====================================================================
   Music
   ===================================================================== */
let media = { active: false };
let lastTrackKey = '';
let peekTimer = null;

function prettyApp(id = '') {
  const known = {
    spotify: 'Spotify',
    chrome: 'Chrome',
    msedge: 'Edge',
    firefox: 'Firefox',
    zunemusic: 'Lettore multimediale',
    vlc: 'VLC',
    opera: 'Opera',
    brave: 'Brave',
    applemusic: 'Apple Music',
  };
  const low = id.toLowerCase();
  for (const k of Object.keys(known)) if (low.includes(k)) return known[k];
  return id.split(/[\\!]/).pop().replace(/\.exe$/i, '');
}

function setArt(el) {
  if (media.active && media.art) {
    el.style.backgroundImage = `url("${media.art}")`;
    el.textContent = '';
  } else {
    el.style.backgroundImage = '';
    el.textContent = media.active && media.title ? media.title.charAt(0).toUpperCase() : '♪';
  }
}

function updateMusic() {
  const playing = media.active && media.playing;
  $('m-title').textContent = media.active ? media.title || 'Brano sconosciuto' : 'Niente in riproduzione';
  $('m-artist').textContent = media.active ? media.artist || '' : 'Avvia musica da Spotify, browser…';
  $('m-app').textContent = media.active ? prettyApp(media.app) : '';
  setArt($('art'));
  setArt($('mini-art'));
  $('btn-toggle').innerHTML = playing ? ICON.pause : ICON.play;
  pill.classList.toggle('playing', playing);
  updateProgress();
  updateMini();
}

function fmtDur(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

let seeking = null; // 0..1 while the user drags on the progress bar

function updateProgress() {
  const show = media.active && media.durationMs > 0;
  $('m-progress').classList.toggle('hidden', !show);
  if (!show) return;
  // The OS reports the position at `updatedMs`; extrapolate while playing.
  let pos = media.positionMs + (media.playing ? Date.now() - media.updatedMs : 0);
  if (seeking !== null) pos = seeking * media.durationMs;
  pos = Math.min(Math.max(pos, 0), media.durationMs);
  $('m-pos').textContent = fmtDur(pos);
  $('m-dur').textContent = fmtDur(media.durationMs);
  $('m-fill').style.width = `${(pos / media.durationMs) * 100}%`;
}
setInterval(updateProgress, 250);

{
  const track = $('m-track');
  const frac = (e) => {
    const r = track.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
  };
  track.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || !(media.active && media.durationMs > 0)) return;
    track.setPointerCapture(e.pointerId);
    seeking = frac(e);
    track.classList.add('seeking');
    updateProgress();
  });
  track.addEventListener('pointermove', (e) => {
    if (seeking === null) return;
    seeking = frac(e);
    updateProgress();
  });
  const end = () => {
    if (seeking === null) return;
    const positionMs = Math.round(seeking * media.durationMs);
    seeking = null;
    track.classList.remove('seeking');
    invoke('media_command', { cmd: 'seek', positionMs });
    media.positionMs = positionMs; // optimistic, the next poll confirms it
    media.updatedMs = Date.now();
    updateProgress();
  };
  track.addEventListener('pointerup', end);
  track.addEventListener('pointercancel', end);
}

function peekNewTrack() {
  if (expanded) return;
  peeking = true;
  render();
  clearTimeout(peekTimer);
  peekTimer = setTimeout(() => {
    peeking = false;
    render();
  }, 3500);
}

function onMedia(payload) {
  const prevArt = media.art;
  media = payload;
  const key = media.active ? `${media.app}|${media.title}|${media.artist}` : '';
  if (key && key !== lastTrackKey && media.playing && lastTrackKey !== '') peekNewTrack();
  if (key) lastTrackKey = key;
  if (media.art !== prevArt && settings?.accentMode !== 'fixed') applyAccentFromArt(media.art);
  updateMusic();
}
listen('media', ({ payload }) => onMedia(payload));
// state that was emitted before this page was listening
invoke('media_snapshot').then((m) => m && onMedia(m));

listen('audio', ({ payload }) => {
  levels = payload;
});

document.querySelectorAll('[data-cmd]').forEach((btn) => {
  btn.addEventListener('click', () => {
    const cmd = btn.dataset.cmd;
    invoke('media_command', { cmd });
    if (cmd === 'toggle' && media.active) {
      // optimistic: the next poll from the backend confirms it
      media.playing = !media.playing;
      media.positionMs += media.playing ? 0 : Date.now() - media.updatedMs;
      media.updatedMs = Date.now();
      updateMusic();
    }
  });
});

/* ---------- accent colour from the album cover ---------- */
const DEFAULT_ACCENT = ['#ff8a5c', '#ffc46b'];

function applyAccentFromArt(src) {
  const root = document.documentElement.style;
  if (!src) {
    root.setProperty('--accent', DEFAULT_ACCENT[0]);
    root.setProperty('--accent-2', DEFAULT_ACCENT[1]);
    return;
  }
  const img = new Image();
  img.onload = () => {
    const c = document.createElement('canvas');
    c.width = c.height = 32;
    const ctx = c.getContext('2d', { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, 32, 32);
    const px = ctx.getImageData(0, 0, 32, 32).data;
    // Weight each pixel by saturation × brightness: picks the "vivid" colour, not the average grey.
    let r = 0, g = 0, b = 0, wsum = 0;
    for (let i = 0; i < px.length; i += 4) {
      const [pr, pg, pb] = [px[i], px[i + 1], px[i + 2]];
      const max = Math.max(pr, pg, pb);
      const min = Math.min(pr, pg, pb);
      const sat = max ? (max - min) / max : 0;
      const w = sat * sat * (max / 255) + 0.001;
      r += pr * w;
      g += pg * w;
      b += pb * w;
      wsum += w;
    }
    let [h, s, l] = rgbToHsl(r / wsum, g / wsum, b / wsum);
    // keep it readable on the black pill
    s = Math.max(s, 0.45);
    l = Math.min(Math.max(l, 0.55), 0.7);
    root.setProperty('--accent', `hsl(${h} ${s * 100}% ${l * 100}%)`);
    root.setProperty('--accent-2', `hsl(${(h + 30) % 360} ${s * 100}% ${Math.min(l + 0.12, 0.85) * 100}%)`);
  };
  img.src = src;
}

function rgbToHsl(r, g, b) {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h;
  if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return [Math.round(h * 60), s, l];
}

/* =====================================================================
   Command Center
   ===================================================================== */
const SVG = {
  speaker:
    '<svg class="ico" viewBox="0 0 24 24"><path d="M4 9h4l5-4v14l-5-4H4z"/><path d="M16 9a4 4 0 0 1 0 6M18.5 6.5a7.5 7.5 0 0 1 0 11" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  muted:
    '<svg class="ico" viewBox="0 0 24 24"><path d="M4 9h4l5-4v14l-5-4H4z"/><path d="M16 9.5l5 5M21 9.5l-5 5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
  sun: '<svg class="ico" viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>',
};

let sys = null;
// While the user drags a slider, ignore the backend echo for that slider.
const userTouched = { volume: 0, brightness: 0 };
const TOUCH_GRACE_MS = 1200;

function setRange(el, value) {
  el.value = value;
  el.style.setProperty('--pct', `${value}%`);
}

function renderControls() {
  if (!sys) return;
  const now = Date.now();

  // tiles
  const wifi = $('t-wifi');
  wifi.classList.toggle('unavailable', sys.wifiOn === null);
  wifi.classList.toggle('on', !!sys.wifiOn);
  $('t-wifi-sub').textContent = !sys.wifiOn ? 'Spento' : sys.wifiSsid || 'Non connesso';

  const bt = $('t-bt');
  bt.classList.toggle('unavailable', sys.btOn === null);
  bt.classList.toggle('on', !!sys.btOn);
  $('t-bt-sub').textContent = !sys.btOn
    ? 'Spento'
    : sys.btDevices.length
      ? sys.btDevices.join(', ')
      : 'Nessun dispositivo';


  // volume
  $('c-mute').innerHTML = sys.muted || sys.volume === 0 ? SVG.muted : SVG.speaker;
  if (now - userTouched.volume > TOUCH_GRACE_MS) setRange($('c-volume'), sys.volume);
  $('c-volume-val').textContent = sys.muted ? 'Muto' : `${$('c-volume').value}%`;

  // brightness (only laptop panels support it)
  $('c-bright-row').classList.toggle('hidden', sys.brightness === null);
  $('c-bright-ico').innerHTML = SVG.sun;
  if (sys.brightness !== null && now - userTouched.brightness > TOUCH_GRACE_MS) setRange($('c-bright'), sys.brightness);
  $('c-bright-val').textContent = `${$('c-bright').value}%`;
}

function onSystem(next) {
  const prev = sys;
  sys = next;
  renderControls();
  if (!prev) return;
  // changed from outside Lumen (keyboard keys, Windows flyout...) → show the popup
  const now = Date.now();
  if ((prev.volume !== sys.volume || prev.muted !== sys.muted) && now - userTouched.volume > TOUCH_GRACE_MS) {
    showOsd('volume');
  } else if (
    prev.brightness !== sys.brightness &&
    sys.brightness !== null &&
    prev.brightness !== null &&
    now - userTouched.brightness > TOUCH_GRACE_MS
  ) {
    showOsd('brightness');
  }
}
listen('system', ({ payload }) => onSystem(payload));
invoke('system_snapshot').then((s) => s && onSystem(s));

// throttle slider writes: one every 60 ms is plenty
function throttle(fn, ms) {
  let last = 0;
  let timer = null;
  return (...args) => {
    clearTimeout(timer);
    const wait = ms - (Date.now() - last);
    if (wait <= 0) {
      last = Date.now();
      fn(...args);
    } else {
      timer = setTimeout(() => {
        last = Date.now();
        fn(...args);
      }, wait);
    }
  };
}
const sendVolume = throttle((v) => invoke('set_volume', { level: v }), 60);
const sendBrightness = throttle((v) => invoke('set_brightness', { level: v }), 120);

$('c-volume').addEventListener('input', (e) => {
  const v = Number(e.target.value);
  userTouched.volume = Date.now();
  setRange(e.target, v);
  $('c-volume-val').textContent = `${v}%`;
  sendVolume(v);
});
$('c-bright').addEventListener('input', (e) => {
  const v = Number(e.target.value);
  userTouched.brightness = Date.now();
  setRange(e.target, v);
  $('c-bright-val').textContent = `${v}%`;
  sendBrightness(v);
});
$('c-mute').addEventListener('click', () => {
  userTouched.volume = Date.now();
  invoke('toggle_mute');
});

$('t-wifi').addEventListener('click', () => {
  if (!sys) return;
  invoke('set_radio', { kind: 'wifi', on: !sys.wifiOn });
  sys.wifiOn = !sys.wifiOn; // optimistic
  renderControls();
});
$('t-bt').addEventListener('click', () => {
  if (!sys) return;
  invoke('set_radio', { kind: 'bluetooth', on: !sys.btOn });
  sys.btOn = !sys.btOn;
  renderControls();
});

$('t-settings').addEventListener('click', () => invoke('open_settings'));

// Screenshot: close the notch first so it isn't in the capture, then open Windows' snipping overlay.
$('t-shot').addEventListener('click', () => {
  clearTimeout(collapseTimer);
  expanded = false;
  render();
  invoke('take_screenshot');
});

/* ---------- Git: pull --rebase of every repository in the projects folder ---------- */
const GIT_ICON = $('t-git').querySelector('svg').outerHTML;
let gitRunning = false;
function setGitTile(busy, sub) {
  gitRunning = busy;
  $('t-git').classList.toggle('on', busy);
  $('t-git').classList.toggle('busy', busy);
  $('t-git-sub').textContent = sub;
}
$('t-git').addEventListener('click', async () => {
  if (gitRunning) return;
  if (await invoke('git_pull_all')) setGitTile(true, 'Avvio…');
});
listen('git-progress', ({ payload: p }) => setGitTile(true, `${p.done}/${p.total} repo`));
listen('git-done', ({ payload: s }) => {
  setGitTile(false, `Fatto alle ${fmtTime.format(new Date())}`);
  if (s.error) {
    showToast({ app: 'Git pull', title: 'Non eseguito', body: s.error, svg: GIT_ICON, sideClass: 'low' });
    return;
  }
  const parts = [];
  if (s.skipped.length) parts.push(`Saltati: ${s.skipped.map(([n]) => n).join(', ')}`);
  if (s.failed.length) parts.push(`Errori: ${s.failed.map(([n]) => n).join(', ')}`);
  showToast({
    app: `Git pull --rebase · ${s.total} repo`,
    title: `${s.updated.length} aggiornati · ${s.upToDate} già aggiornati`,
    body: parts.join(' · ') || 'Nessun problema',
    svg: GIT_ICON,
    side: s.failed.length ? `${s.failed.length} errori` : s.skipped.length ? `${s.skipped.length} saltati` : 'OK',
    sideClass: s.failed.length ? 'low' : 'good',
    duration: 7000,
    action: () => invoke('open_git_log'),
  });
});

/* ---------- popup for volume / brightness changes ---------- */
let osdTimer = null;
function showOsd(kind) {
  if (expanded) return;
  const value = kind === 'volume' ? (sys.muted ? 0 : sys.volume) : sys.brightness;
  $('osd-ico').innerHTML = kind === 'volume' ? (value === 0 ? SVG.muted : SVG.speaker) : SVG.sun;
  $('osd-fill').style.width = `${value}%`;
  $('osd-val').textContent = value;
  osdActive = true;
  render();
  clearTimeout(osdTimer);
  osdTimer = setTimeout(() => {
    osdActive = false;
    render();
  }, 1600);
}

/* =====================================================================
   Status
   ===================================================================== */
const fmtTime = new Intl.DateTimeFormat('it-IT', { hour: '2-digit', minute: '2-digit' });
const fmtDate = new Intl.DateTimeFormat('it-IT', { weekday: 'long', day: 'numeric', month: 'long' });

let lastDay = new Date().getDate();
function updateClock() {
  const now = new Date();
  $('s-time').textContent = fmtTime.format(now);
  $('s-date').textContent = fmtDate.format(now);
  updateMini();
  checkMeetingStart();
  if (now.getDate() !== lastDay) {
    lastDay = now.getDate();
    buildCalendar();
  } else if (now.getSeconds() % 20 === 0 && expanded && MODES[mode] === 'calendar') {
    renderAgenda(); // past meetings fade, join buttons disappear
  }
}

async function initBattery() {
  if (!navigator.getBattery) return;
  const batt = await navigator.getBattery();
  const update = () => {
    const pct = Math.round(batt.level * 100);
    $('s-batt-pct').textContent = `${pct}%`;
    $('s-batt-fill').style.width = `${pct}%`;
    $('s-battery').classList.toggle('charging', batt.charging);
    $('s-battery').classList.toggle('low', pct <= 20 && !batt.charging);
    $('s-battery').title = batt.charging ? 'In carica' : 'Batteria';
  };
  ['levelchange', 'chargingchange'].forEach((ev) => batt.addEventListener(ev, update));
  update();
}

/* =====================================================================
   Calendar + Outlook agenda (published ICS link, read by the backend)
   ===================================================================== */
const MIN = 60 * 1000;
const DAY = 24 * 60 * MIN;
let agenda = { configured: false, events: [], error: null };
let selDay = null; // Date (local midnight) chosen in the grid; null = today

const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate());
function eventsOn(day) {
  const s = startOfDay(day).getTime();
  const e = new Date(day.getFullYear(), day.getMonth(), day.getDate() + 1).getTime();
  return agenda.events.filter((ev) => ev.start < e && ev.end > s);
}

function buildCalendar() {
  const now = new Date();
  const y = now.getFullYear();
  const m = now.getMonth();
  $('c-month').textContent = new Intl.DateTimeFormat('it-IT', { month: 'long', year: 'numeric' }).format(now);
  const grid = $('c-grid');
  grid.textContent = '';
  const cell = (text, cls) => {
    const el = document.createElement('div');
    el.textContent = text;
    if (cls) el.className = cls;
    grid.appendChild(el);
    return el;
  };
  ['L', 'M', 'M', 'G', 'V', 'S', 'D'].forEach((d) => cell(d, 'dow'));
  const offset = (new Date(y, m, 1).getDay() + 6) % 7; // weeks start on Monday
  for (let i = 0; i < offset; i++) cell('·', 'muted');
  const days = new Date(y, m + 1, 0).getDate();
  const sel = selDay || startOfDay(now);
  for (let d = 1; d <= days; d++) {
    const date = new Date(y, m, d);
    const cls = ['day'];
    if (d === now.getDate()) cls.push('today');
    if (date.getTime() === sel.getTime()) cls.push('sel');
    if (eventsOn(date).some((ev) => !ev.free)) cls.push('has-ev');
    const el = cell(d, cls.join(' '));
    el.addEventListener('click', () => {
      selDay = date.getTime() === startOfDay(new Date()).getTime() ? null : date;
      buildCalendar();
    });
  }
  renderAgenda();
}

const fmtHM = new Intl.DateTimeFormat('it-IT', { hour: '2-digit', minute: '2-digit' });
const fmtDayShort = new Intl.DateTimeFormat('it-IT', { weekday: 'short', day: 'numeric', month: 'short' });

function renderAgenda() {
  const now = Date.now();
  const day = selDay || startOfDay(new Date());
  const today = startOfDay(new Date()).getTime();
  const diff = Math.round((day.getTime() - today) / DAY);
  $('a-head').textContent = diff === 0 ? 'Oggi' : diff === 1 ? 'Domani' : diff === -1 ? 'Ieri' : fmtDayShort.format(day);
  const list = $('a-list');
  list.textContent = '';
  const note = (text, btn) => {
    const el = document.createElement('div');
    el.className = 'agenda-note';
    el.textContent = text;
    if (btn) {
      const b = document.createElement('button');
      b.className = 'agenda-btn';
      b.textContent = btn.label;
      b.addEventListener('click', btn.run);
      el.appendChild(b);
    }
    list.appendChild(el);
  };
  if (!agenda.configured) {
    note('Collega il calendario di Outlook per vedere qui le riunioni.', { label: 'Collega', run: () => invoke('open_settings') });
    return;
  }
  let evs = eventsOn(day);
  if (!evs.length) {
    note(agenda.error && !agenda.events.length ? 'Calendario non raggiungibile.' : 'Nessun impegno.');
    return;
  }
  // today: start from the first meeting not over yet
  const MAX = 4;
  if (diff === 0) {
    const firstOpen = evs.findIndex((e) => e.allDay || e.end > now);
    if (firstOpen > 0 && evs.length > MAX) evs = evs.slice(Math.min(firstOpen, evs.length - MAX));
  }
  for (const ev of evs.slice(0, MAX)) {
    const row = document.createElement('div');
    row.className = 'agenda-item';
    if (!ev.allDay && ev.end <= now) row.classList.add('past');
    if (!ev.allDay && ev.start <= now && ev.end > now) row.classList.add('live');
    if (ev.free) row.classList.add('free');
    const time = document.createElement('span');
    time.className = 'agenda-time';
    time.textContent = ev.allDay ? 'Giorno' : fmtHM.format(new Date(ev.start));
    const title = document.createElement('span');
    title.className = 'agenda-title';
    title.textContent = ev.title;
    title.title = `${ev.title}${ev.location ? ` · ${ev.location}` : ''}`;
    row.append(time, title);
    if (ev.join && ev.end > now) {
      const b = document.createElement('button');
      b.className = 'agenda-btn';
      b.textContent = 'Partecipa';
      b.addEventListener('click', () => invoke('open_link', { url: ev.join }));
      row.appendChild(b);
    }
    list.appendChild(row);
  }
  if (evs.length > MAX) note(`+${evs.length - MAX} altri`);
}

function onAgenda(a) {
  agenda = a || { configured: false, events: [], error: null };
  buildCalendar();
  updateMini();
}
listen('agenda', ({ payload }) => onAgenda(payload));
invoke('agenda_snapshot').then((a) => a && onAgenda(a));

/** The meeting the collapsed notch should talk about, if any. */
function meetingNow(now = Date.now()) {
  const evs = agenda.events.filter((e) => !e.allDay && !e.free && e.end > now);
  // a meeting that has started stays in the collapsed notch for one minute only
  const live = evs.find((e) => e.start <= now && now - e.start < MIN);
  if (live) return { e: live, live: true, mins: Math.floor((now - live.start) / MIN) };
  const next = evs.find((e) => e.start > now);
  if (next && next.start - now <= 60 * MIN) return { e: next, live: false, mins: Math.ceil((next.start - now) / MIN) };
  return null;
}
const isUrgent = (m) => !!m && (m.live || m.mins <= 15);

// When a meeting is close, opening the notch shows the calendar (once per meeting).
let autoOpenedFor = null;
function openCalendarForMeeting() {
  const m = meetingNow();
  const idx = MODES.indexOf('calendar');
  if (!isUrgent(m) || idx < 0 || autoOpenedFor === m.e.id || expanded) return;
  autoOpenedFor = m.e.id;
  mode = idx;
  selDay = null;
  buildCalendar();
}

// One minute before a meeting: a popup you can click to join.
const announced = new Set();
function checkMeetingStart() {
  const now = Date.now();
  for (const e of agenda.events) {
    if (e.allDay || e.free || announced.has(e.id)) continue;
    const left = e.start - now;
    if (left <= 60 * 1000 && left > -2 * MIN) {
      announced.add(e.id);
      showToast({
        app: 'Calendario',
        title: e.title,
        body: left > 0 ? 'Inizia tra un minuto' : 'Sta iniziando',
        glyph: '📅',
        side: e.join ? 'Partecipa' : fmtHM.format(new Date(e.start)),
        sideClass: e.join ? 'join' : '',
        duration: 15000,
        alert: true,
        action: e.join ? () => invoke('open_link', { url: e.join }) : () => openViewNow('calendar'),
      });
    }
  }
}

function openViewNow(name) {
  const idx = MODES.indexOf(name);
  if (idx >= 0) mode = idx;
  expand();
  if (!hovering) scheduleCollapse(4000);
}

/* =====================================================================
   Collapsed content
   ===================================================================== */
let meetingShown = false;

function updateMini() {
  const text = $('mini-text');
  const sub = $('mini-sub');
  const playing = media.active && media.playing;
  const m = meetingNow();
  // An imminent meeting beats the music; otherwise it only replaces the clock.
  const showMeeting = !!m && (isUrgent(m) || !playing);
  if (showMeeting) {
    text.textContent = m.live
      ? 'Inizia ora'
      : m.mins >= 60 ? 'Riunione tra 1 ora' : `Riunione tra ${m.mins} min`;
    sub.textContent = m.e.title;
    const art = $('mini-art');
    art.style.backgroundImage = '';
    art.textContent = '📅';
  } else {
    setArt($('mini-art'));
    if (playing) {
      text.textContent = media.title || 'In riproduzione';
      sub.textContent = media.artist || prettyApp(media.app);
    } else {
      text.textContent = fmtTime.format(new Date());
    }
  }
  pill.classList.toggle('urgent', showMeeting && isUrgent(m));
  if (showMeeting !== meetingShown) {
    meetingShown = showMeeting;
    pill.classList.toggle('meeting', showMeeting);
    render();
  }
}

/* =====================================================================
   Settings
   ===================================================================== */
function applySettings(next) {
  const previousMode = MODES[mode];
  settings = next;
  MODES = settings.modes.length ? settings.modes : ['music'];
  mode = Math.max(0, MODES.indexOf(previousMode));
  document.documentElement.style.setProperty('--scale', settings.scale);
  if (settings.accentMode === 'fixed') {
    document.documentElement.style.setProperty('--accent', settings.accentColor);
    document.documentElement.style.setProperty('--accent-2', settings.accentColor);
  } else {
    applyAccentFromArt(media.art);
  }
  loadWeather();
  render();
}
listen('settings-changed', ({ payload }) => applySettings(payload));

/* =====================================================================
   PC view (sampled only while it is open)
   ===================================================================== */
let pcTimer = null;

function fmtBytes(b) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (b >= 1024 && i < units.length - 1) {
    b /= 1024;
    i++;
  }
  return `${b.toFixed(b < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}
function fmtUptime(secs) {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return d ? `${d}g ${h}h` : h ? `${h}h ${m}m` : `${m}m`;
}
function setRing(id, pct) {
  const ring = $(id);
  ring.querySelector('.ring-fg').style.strokeDashoffset = `${169.6 * (1 - Math.min(pct, 100) / 100)}`;
  ring.classList.toggle('hot', pct >= 85);
}

async function samplePc() {
  try {
    const p = await invoke('pc_stats');
    const ramPct = p.ramTotal ? (p.ramUsed / p.ramTotal) * 100 : 0;
    const diskPct = p.diskTotal ? (p.diskUsed / p.diskTotal) * 100 : 0;
    $('pc-cpu').textContent = `${Math.round(p.cpu)}%`;
    $('pc-ram').textContent = `${Math.round(ramPct)}%`;
    setRing('r-cpu', p.cpu);
    setRing('r-ram', ramPct);
    $('pc-cpu-name').textContent = `${p.cpuName || 'Processore'} · ${p.cores} thread`;
    $('pc-ram-text').textContent = `${fmtBytes(p.ramUsed)} / ${fmtBytes(p.ramTotal)}`;
    $('pc-disk-name').textContent = `Disco ${p.diskName}`;
    $('pc-disk-text').textContent = `${fmtBytes(p.diskTotal - p.diskUsed)} liberi`;
    $('pc-disk-fill').style.width = `${diskPct}%`;
    $('pc-net').textContent = `↓ ${fmtBytes(p.netDown)}/s  ↑ ${fmtBytes(p.netUp)}/s`;
    $('pc-uptime').textContent = fmtUptime(p.uptimeSecs);
  } catch {
    /* backend busy: next tick */
  }
}

function onViewChanged() {
  const pcOpen = expanded && MODES[mode] === 'pc';
  if (pcOpen && !pcTimer) {
    samplePc();
    pcTimer = setInterval(samplePc, 1500);
  } else if (!pcOpen && pcTimer) {
    clearInterval(pcTimer);
    pcTimer = null;
  }
  if (expanded && MODES[mode] === 'status') refreshWeatherIfStale();
}

/* =====================================================================
   Weather (Open-Meteo, no API key; the city is chosen in the settings)
   ===================================================================== */
const WMO = [
  [[0], '☀️', 'Sereno'],
  [[1], '🌤️', 'Poco nuvoloso'],
  [[2], '⛅', 'Parzialmente nuvoloso'],
  [[3], '☁️', 'Coperto'],
  [[45, 48], '🌫️', 'Nebbia'],
  [[51, 53, 55, 56, 57], '🌦️', 'Pioviggine'],
  [[61, 63, 65, 66, 67, 80, 81, 82], '🌧️', 'Pioggia'],
  [[71, 73, 75, 77, 85, 86], '🌨️', 'Neve'],
  [[95, 96, 99], '⛈️', 'Temporale'],
];
const WEATHER_EVERY = 15 * 60 * 1000; // normal refresh
const WEATHER_RETRY = 60 * 1000;      // retry after a failed request
let weatherTimer = null;
let weatherAt = 0;      // time of the last successful update (0 = none yet)
let weatherKey = '';    // city the shown data belongs to
let weatherBusy = false;

const hasWeatherCity = () => settings && settings.weatherLat != null && settings.weatherLon != null;
const weatherStale = () => Date.now() - weatherAt >= WEATHER_EVERY;

function scheduleWeather(ms) {
  clearTimeout(weatherTimer);
  weatherTimer = setTimeout(fetchWeather, ms);
}

async function fetchWeather() {
  if (!hasWeatherCity() || weatherBusy) return;
  weatherBusy = true;
  const key = weatherKey;
  try {
    const url =
      `https://api.open-meteo.com/v1/forecast?latitude=${settings.weatherLat}&longitude=${settings.weatherLon}` +
      '&current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1';
    const res = await fetch(url, { signal: AbortSignal.timeout(10000) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    if (key !== weatherKey) return; // city changed while waiting
    const code = data.current.weather_code;
    const [, icon, label] = WMO.find(([codes]) => codes.includes(code)) || [[], '🌡️', ''];
    $('w-icon').textContent = icon;
    $('w-temp').textContent = `${Math.round(data.current.temperature_2m)}°`;
    $('w-desc').textContent =
      `${settings.weatherName} · ${label} · ${Math.round(data.daily.temperature_2m_min[0])}°/${Math.round(data.daily.temperature_2m_max[0])}°`;
    weatherAt = Date.now();
    scheduleWeather(WEATHER_EVERY);
  } catch {
    // Keep showing the last valid value; only say "unavailable" if there is none yet.
    if (!weatherAt && key === weatherKey) {
      $('w-icon').textContent = '🌡️';
      $('w-temp').textContent = '–';
      $('w-desc').textContent = `${settings.weatherName} · Meteo non disponibile`;
    }
    scheduleWeather(WEATHER_RETRY);
  } finally {
    weatherBusy = false;
  }
}

/** Called when the settings change: refetch only if the city changed or the data is old. */
function loadWeather() {
  $('s-weather').classList.remove('hidden');
  if (!hasWeatherCity()) {
    clearTimeout(weatherTimer);
    weatherAt = 0;
    weatherKey = '';
    $('w-icon').textContent = '📍';
    $('w-temp').textContent = 'Meteo';
    $('w-desc').textContent = 'Scegli la città nelle impostazioni';
    return;
  }
  const key = `${settings.weatherLat},${settings.weatherLon},${settings.weatherName}`;
  if (key !== weatherKey) {
    weatherKey = key;
    weatherAt = 0;
    fetchWeather();
  } else if (weatherStale()) {
    fetchWeather();
  }
}

/** Refresh if the data is old (after sleep, or when the network comes back). */
function refreshWeatherIfStale() {
  if (hasWeatherCity() && weatherStale()) fetchWeather();
}
window.addEventListener('online', refreshWeatherIfStale);
document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshWeatherIfStale(); });

/* =====================================================================
   Toasts: notifications, devices, charger
   ===================================================================== */
const DEVICE_ICON = {
  bluetooth:
    '<svg class="ico" viewBox="0 0 24 24"><path d="M7 7l10 10-5 5V2l5 5L7 17" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  usb: '<svg class="ico" viewBox="0 0 24 24"><path d="M8 2h8v6H8zM6 8h12v9a5 5 0 0 1-5 5h-2a5 5 0 0 1-5-5z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/></svg>',
  charger: '<svg class="ico" viewBox="0 0 24 24"><path d="M13 2 4 14h7l-1 8 9-12h-7z"/></svg>',
};
const toastQueue = [];
let toastTimer = null;
let currentToast = null;

function showToast(t) {
  toastQueue.push(t);
  if (!toastActive) nextToast();
}

function nextToast() {
  const t = toastQueue.shift();
  currentToast = t || null;
  pill.classList.remove('toast-hover', 'toast-action');
  if (!t) {
    toastActive = false;
    pill.classList.remove('alert');
    // still under the mouse: open the views as a normal hover would
    if (hovering) expand();
    else render();
    return;
  }
  const icon = $('toast-icon');
  icon.classList.toggle('plain', !t.icon);
  icon.classList.toggle('has-img', !!t.icon);
  icon.style.backgroundImage = t.icon ? `url("${t.icon}")` : '';
  icon.innerHTML = t.icon ? '' : t.svg || '';
  if (!t.icon && !t.svg) icon.textContent = t.glyph || (t.app || '?').charAt(0).toUpperCase();
  $('toast-app').textContent = t.app || '';
  $('toast-title').textContent = t.title || '';
  $('toast-body').textContent = t.body || '';
  const side = $('toast-side');
  side.textContent = t.side || '';
  side.className = `toast-side ${t.sideClass || ''}`;
  for (const b of t.buttons || []) {
    const btn = document.createElement('button');
    btn.className = 'toast-btn';
    btn.textContent = b.label;
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      b.run();
      clearTimeout(toastTimer);
      nextToast();
    });
    side.appendChild(btn);
  }
  pill.classList.toggle('toast-action', !!t.action);
  pill.classList.toggle('alert', !!t.alert);
  toastActive = true;
  render();
  clearTimeout(toastTimer);
  if (hovering && t.action) pill.classList.add('toast-hover');
  else toastTimer = setTimeout(nextToast, t.duration || (t.body && t.body.length > 30 ? 5000 : 3200));
}

$('v-toast').addEventListener('click', () => {
  const t = currentToast;
  if (!t?.action || !toastActive) return;
  t.action();
  clearTimeout(toastTimer);
  nextToast();
});

listen('notification', ({ payload: n }) => {
  showToast({
    app: n.app,
    title: n.title,
    body: n.body,
    icon: n.icon,
    side: n.aumid ? '›' : '',
    sideClass: 'chev',
    action: n.aumid ? () => invoke('open_app', { aumid: n.aumid }) : null,
  });
});

listen('download', ({ payload: d }) => {
  showToast({
    app: 'Download completato',
    title: d.name,
    body: fmtBytes(d.size),
    icon: d.icon,
    glyph: '⬇',
    duration: 8000,
    action: () => invoke('open_download', { path: d.path }),
    buttons: [
      { label: 'Apri', run: () => invoke('open_download', { path: d.path }) },
      { label: 'Mostra', run: () => invoke('reveal_download', { path: d.path }) },
    ],
  });
});

listen('update-available', ({ payload: u }) => {
  showToast({
    app: 'Aggiornamento di Lumen',
    title: `Versione ${u.version} disponibile`,
    body: `Hai la ${u.current}. Lumen si riavvia da solo dopo l'installazione.`,
    glyph: '↑',
    duration: 15000,
    action: () => invoke('install_update'),
    buttons: [
      { label: 'Installa', run: () => invoke('install_update') },
      { label: 'Dopo', run: () => {} },
    ],
  });
});

listen('device', ({ payload: d }) => {
  showToast({
    app: d.kind === 'usb' ? 'Unità USB' : 'Bluetooth',
    title: d.name,
    body: d.connected ? 'Connesso' : 'Disconnesso',
    svg: DEVICE_ICON[d.kind],
    side: d.battery != null ? `${d.battery}%` : '',
    sideClass: d.battery != null && d.battery <= 20 ? 'low' : 'good',
  });
});

// Charger plugged / unplugged: Battery API of the webview
async function watchCharger() {
  if (!navigator.getBattery) return;
  const batt = await navigator.getBattery();
  batt.addEventListener('chargingchange', () => {
    if (settings && !settings.devicePopups) return;
    const pct = Math.round(batt.level * 100);
    showToast({
      app: 'Alimentazione',
      title: batt.charging ? 'In carica' : 'Alimentatore scollegato',
      svg: DEVICE_ICON.charger,
      side: `${pct}%`,
      sideClass: batt.charging ? 'good' : pct <= 20 ? 'low' : '',
    });
  });
}

/* =====================================================================
   Init
   ===================================================================== */
updateMusic();
updateClock();
buildCalendar();
initBattery();
watchCharger();
render();
invoke('get_settings').then(applySettings);
setInterval(updateClock, 1000);
setInterval(buildCalendar, 60 * 60 * 1000);
window.addEventListener('resize', render);

/* =====================================================================
   Drag the notch left/right along the top edge (position is saved)
   ===================================================================== */
{
  const pill = $('pill');
  let downX = null;
  let dragged = false;
  pill.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || e.target.closest('button, input, select, a, .progress-track')) return;
    downX = e.screenX;
    dragged = false;
  });
  pill.addEventListener('pointermove', (e) => {
    if (downX === null || dragged || !(e.buttons & 1)) return;
    if (Math.abs(e.screenX - downX) > 6) {
      dragged = true;
      pill.classList.add('dragging');
      invoke('drag_start');
    }
  });
  const stop = () => {
    downX = null;
    pill.classList.remove('dragging');
  };
  window.addEventListener('pointerup', stop);
  window.addEventListener('pointercancel', stop);
  // A drag must not count as a click on whatever is under the cursor.
  pill.addEventListener('click', (e) => {
    if (dragged) {
      e.stopPropagation();
      e.preventDefault();
      dragged = false;
    }
  }, true);
}
