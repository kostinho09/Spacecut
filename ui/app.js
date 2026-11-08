'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

/* ── Helpers ────────────────────────────────────────── */

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

function h(tag, props = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'html') el.innerHTML = v;
    else if (k === 'value') el.value = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k === 'style' && typeof v === 'object') for (const [p, x] of Object.entries(v)) el.style.setProperty(p, x);
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) if (kid != null && kid !== false) el.append(kid);
  return el;
}

const svg = body => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
const ICON = {
  play: '<svg viewBox="0 0 24 24"><path fill="currentColor" d="M7 4.8v14.4c0 .8.9 1.3 1.6.9l11.2-7.2a1 1 0 0 0 0-1.8L8.6 3.9C7.9 3.5 7 4 7 4.8z"/></svg>',
  pause: '<svg viewBox="0 0 24 24"><rect fill="currentColor" x="5.5" y="4" width="4.6" height="16" rx="1.6"/><rect fill="currentColor" x="13.9" y="4" width="4.6" height="16" rx="1.6"/></svg>',
  sun: svg('<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>'),
  moon: svg('<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>'),
  auto: svg('<circle cx="12" cy="12" r="8.5"/><path d="M12 3.5v17" /><path d="M12 3.5a8.5 8.5 0 0 1 0 17z" fill="currentColor"/>'),
  copy: svg('<rect x="8.5" y="8.5" width="11" height="11" rx="2.5"/><path d="M15.5 8.5V6.5a2 2 0 0 0-2-2h-7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h2"/>'),
  check: svg('<path d="M5 12.5l4.5 4.5L19 7.5"/>'),
  dots: '<svg viewBox="0 0 24 24"><circle fill="currentColor" cx="5.5" cy="12" r="1.8"/><circle fill="currentColor" cx="12" cy="12" r="1.8"/><circle fill="currentColor" cx="18.5" cy="12" r="1.8"/></svg>',
  folder: svg('<path d="M3.5 7.5a2 2 0 0 1 2-2h3.6l2 2h7.4a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>'),
  // The scissors of the app icon: round handles, pointed blades, a hole at the pivot
  scissors: '<svg viewBox="0 0 24 24" class="scis"><g class="blade up"><circle cx="6.6" cy="17" r="3.2" fill="none" stroke="currentColor" stroke-width="2.3"/><path d="M8.7 15.1 12 12.4 20.8 6.3a.95.95 0 0 1 1.2 1.45L14.1 13.9z" fill="currentColor"/></g><g class="blade down"><circle cx="6.6" cy="7" r="3.2" fill="none" stroke="currentColor" stroke-width="2.3"/><path d="M8.7 8.9 12 11.6 20.8 17.7a.95.95 0 0 0 1.2-1.45L14.1 10.1z" fill="currentColor"/></g><path d="M12.9 10.35a1.65 1.65 0 1 0 .01 0zm0 .95a.7.7 0 1 1-.01 0z" fill="currentColor" fill-rule="evenodd"/></svg>',
  close: svg('<path d="M6 6l12 12M18 6L6 18"/>'),
  grip: '<svg viewBox="0 0 24 24"><g fill="currentColor"><circle cx="9" cy="6" r="1.7"/><circle cx="15" cy="6" r="1.7"/><circle cx="9" cy="12" r="1.7"/><circle cx="15" cy="12" r="1.7"/><circle cx="9" cy="18" r="1.7"/><circle cx="15" cy="18" r="1.7"/></g></svg>',
  arrow: svg('<path d="M5 12h14M13 6l6 6-6 6"/>'),
  // Round arrows with the seconds inside, as on the iPhone
  back10: '<svg viewBox="0 0 24 24"><g class="ring"><path d="M12 3.6A8.4 8.4 0 1 1 6.06 6.06" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><path d="M12.6 0.9 8.7 3.6l3.9 2.7z" fill="currentColor" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/></g><text x="12" y="15.2" text-anchor="middle">10</text></svg>',
  fwd10: '<svg viewBox="0 0 24 24"><g class="ring"><path d="M12 3.6A8.4 8.4 0 1 0 17.94 6.06" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><path d="M11.4 0.9 15.3 3.6l-3.9 2.7z" fill="currentColor" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/></g><text x="12" y="15.2" text-anchor="middle">10</text></svg>',
};

/** Tag colours, each with the dark tone used for text on it. */
const COLORS = [
  ['#f2a93b', '#412402'], ['#3bbf95', '#04342c'], ['#8c84e8', '#1a1650'],
  ['#f08a6c', '#4a1b0c'], ['#e58bb0', '#4b1528'], ['#6aa8e8', '#042c53'],
];

const pad = n => String(n).padStart(2, '0');
/** A length as m:ss, or h:mm:ss from an hour up. */
function fmtLen(sec) {
  const s = Math.max(0, Math.floor(sec || 0));
  const h = Math.floor(s / 3600);
  return h ? `${h}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}` : `${Math.floor(s / 60)}:${pad(s % 60)}`;
}
function fmt(sec) {
  const s = Math.max(0, Math.floor(sec || 0));
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
}
/** "HH:MM:SS" (also "MM:SS" or plain seconds) to seconds, or null. */
function parseHMS(text) {
  const t = (text || '').trim();
  if (!t) return null;
  const parts = t.split(':');
  if (parts.length > 3 || parts.some(p => !/^\d+$/.test(p))) return null;
  return parts.reduce((acc, p) => acc * 60 + Number(p), 0);
}
/** 013045 → 01:30:45 */
function autoFormat(v) {
  const d = v.replace(/\D/g, '');
  return d.length === 6 ? `${d.slice(0, 2)}:${d.slice(2, 4)}:${d.slice(4)}` : v;
}

const MEDIA_EXTS = ['mp3', 'm4a', 'aac', 'wav', 'flac', 'ogg', 'opus', 'ts', 'aiff',
  'mp4', 'mkv', 'mov', 'avi', 'webm', 'm4v', 'flv', 'wmv', 'mpg', 'mpeg', '3gp'];
const baseName = p => p.split(/[\\/]/).pop();
const stem = p => baseName(p).replace(/\.[^.]+$/, '');
const extOf = p => { const b = baseName(p); return b.includes('.') ? b.split('.').pop().toLowerCase() : ''; };
const isLocal = u => !/^[a-z][a-z0-9+.-]*:\/\//i.test(u.trim());
const savePref = (key, value) => invoke('set_pref', { key, value }).catch(() => {});

/* ── State ──────────────────────────────────────────── */

const S = {
  env: { platform: 'macos', ffmpeg: true, output_folder: '', effects: [], lang: 'en', theme: 'auto' },
  sources: [],      // { id, url, local, name, color, include, lastPos }
  clips: [],        // { id, src, name, full, start, end, status, auto }
  nextId: 1,
  spaceNo: 0,
  active: null,     // source shown in the player
  sel: null,        // clip highlighted on the bar
  pending: null,    // clip whose start is marked and still waits for its end
  join: false,
  joinName: '',
  keep: true,
  order: [],        // clip ids in the order the merged file plays them
  gaps: { before: { path: '', gap: 0 }, after: { path: '', gap: 0 }, mid: [] },  // sound or silence around and between them
  newTag: null,
  running: false,
  cancelled: false,
  resetTimer: 0,
  done: 0,
  total: 0,
  // The Space shown in the player; `view`: only shown, while another one plays
  p: { loaded: false, loading: false, url: '', duration: 0, pos: 0, at: 0, playing: false, buffering: false, ended: false, error: '', live: false, view: false },
  player: { url: '', playing: false, live: false },  // what the player itself has
  shownLive: '',
  loadToken: 0,
  actionAt: 0,
  shownSec: -1,
  shownRec: -1,
  speed: 1,
  msgTimer: 0,
  fxPath: null,
  fxSince: 0,
  drag: null,
};

/** Status of a clip: its icon, a class, and what the icon means. */
const ST_ICON = {
  // The three dots turning into squares one by one while the clip is cut
  cut: '<span class="dots"><i></i><i></i><i></i></span>',
  // The same dots, faded and still, on the clips still waiting their turn
  wait: '<span class="dots wait"><i></i><i></i><i></i></span>',
  conv: '<svg viewBox="0 0 24 24" class="turn" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M19.5 12a7.5 7.5 0 0 1-12.8 5.3M4.5 12a7.5 7.5 0 0 1 12.8-5.3"/><path d="M17.6 2.8v3.9h-3.9M6.4 21.2v-3.9h3.9"/></svg>',
  done: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" fill="currentColor"/><path d="M7.4 12.4l3 3 6.2-6.6" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>',
  bad: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" fill="currentColor"/><path d="M12 7v6" stroke="#fff" stroke-width="2.6" stroke-linecap="round"/><circle cx="12" cy="16.8" r="1.5" fill="#fff"/></svg>',
};
const STATUS = {
  Pending: ['', ''],
  Waiting: ['wait', '', 'Waiting its turn'],
  Cutting: ['cut', 'busy', 'Cutting…'],
  Slow: ['cut', 'busy', 'Still cutting…'],
  Converting: ['conv', 'busy', 'Converting…'],
  Done: ['done', 'ok', 'Saved'],
  'Cut Error': ['bad', 'bad', 'Cut error'],
  'Convert Error': ['bad', 'bad', 'Convert error'],
  'Invalid Time': ['bad', 'bad', 'The end has to come after the start'],
  'Past the End': ['bad', 'bad', 'Starts after the Space ends'],
  'FFmpeg Not Found': ['bad', 'bad', 'FFmpeg not found'],
};

const ui = {
  bar: $('#bar'), knob: $('#knob'), played: $('#played'), tip: $('#tip'), pending: $('#pending'),
  regions: $('#regions'), snap: $('#snap'), pos: $('#pos'), play: $('#play'), msg: $('#msg'), clips: $('#clips'), tags: $('#tags'),
};

const srcById = id => S.sources.find(s => s.id === id);
const clipById = id => S.clips.find(c => c.id === id);
const clipsOf = id => S.clips.filter(c => c.src === id);
const active = () => srcById(S.active);
const colorVars = s => ({ '--c': s.color[0], '--ct': s.color[1] });
const usable = c => c.full || (parseHMS(c.start) != null && parseHMS(c.end) != null);
/** A clip the running cut is working on: it stays as it is until the cut ends. */
const inJob = c => S.running && S.jobIds.includes(c.id);
/** A clip of the running cut that hasn't started yet shows that it waits. */
const statusOf = c => (c.status === 'Pending' && inJob(c) ? STATUS.Waiting : STATUS[c.status] || STATUS.Pending);

/* ── Sources (tags) ─────────────────────────────────── */

function nextColor() {
  const used = new Set(S.sources.map(s => s.color[0]));
  return COLORS.find(c => !used.has(c[0])) || COLORS[S.sources.length % COLORS.length];
}

/** The key all of a Space's links share (dynamic, replay, on any server); null for a file. */
function spaceKey(url) {
  const m = /\/hls\/([^/?#]+)\//.exec(url || '');
  return m ? m[1] : null;
}

/** The tag already open for this Space or file, if any. While a Space is
 *  live its replay link comes out different each time, so Spaces are matched by key. */
function knownSource(url) {
  const key = spaceKey(url);
  return S.sources.find(x => x.url === url || (key && spaceKey(x.url) === key));
}

/** `live` is the Space's dynamic link, when it came from one: where its
 *  pieces can still be found if the replay doesn't give them out. */
function addSource(url, live = '') {
  let s = knownSource(url);
  if (s) {
    if (live && !s.live) s.live = live;
    return s;
  }
  const local = isLocal(url);
  s = { id: S.nextId++, url, live, local, name: local ? baseName(url) : `Space ${++S.spaceNo}`,
        color: nextColor(), include: true, lastPos: 0 };
  S.sources.push(s);
  S.newTag = s.id;
  // A file is usually wanted whole; marking a period replaces this
  if (local) S.clips.push({ ...newClip(s.id), full: true, auto: true });
  return s;
}

const wait = ms => new Promise(r => setTimeout(r, ms));
let adding = false;

/** The note at the right end of the link field. */
function srcNote(html, kind) {
  const n = $('#srcNote');
  clearTimeout(srcNote.timer);
  n.parentElement.classList.toggle('noted', !!html);
  if (!html) { n.classList.add('hidden'); return; }
  n.className = `src-note ${kind || ''}`;
  n.innerHTML = html;
}

/** The link drops out of the field and a green tick says it went in. */
function sendOff(input) {
  const ghost = h('span', { class: 'src-ghost' }, input.value);
  input.parentElement.append(ghost);
  input.value = '';
  input.classList.add('quiet');
  setTimeout(() => { ghost.remove(); input.classList.remove('quiet'); }, 420);
  srcNote(`${ICON.check}Added`, 'ok');
  srcNote.timer = setTimeout(() => srcNote(''), 1400);
}

async function addLink(raw) {
  const input = $('#src');
  let url = (raw || '').trim();
  if (!url || adding) return;
  adding = true;
  let live = '';
  try {
    // A Space already open: back to its tag, with nothing to convert
    const known = knownSource(url);
    if (known) {
      if (/dynamic_playlist\.m3u8/i.test(url) && !known.live) known.live = url;
      sendOff(input);
      await activate(known.id);
      return;
    }
    if (/dynamic_playlist\.m3u8/i.test(url) && /type=live/i.test(url)) {
      live = url;
      srcNote(`Live ${ICON.arrow} <b>Replay</b>`, 'conv');
      try { url = await invoke('convert_live', { url }); }
      catch (e) { srcNote(''); flash(`Could not convert the live link (${e})`, 'bad'); return; }
      input.value = url;
      input.classList.remove('swap');
      void input.offsetWidth;
      input.classList.add('swap');
    }
    sendOff(input);
    await activate(addSource(url, live).id);
  } finally {
    adding = false;
  }
}

async function activate(id) {
  const s = srcById(id);
  if (!s) return;
  const cur = active();
  if (cur && S.p.loaded && S.p.url === cur.url) cur.lastPos = currentPos();
  S.active = id;
  S.sel = null;
  if (S.pending && clipById(S.pending)?.src !== id) S.pending = null;
  render();
  await loadPlayer(s);
}

/** Removes a Space or file and its clips at once, without asking. */
function removeSource(id) {
  if (!srcById(id)) return;
  const gone = srcById(id);
  S.sources = S.sources.filter(x => x.id !== id);
  S.clips = S.clips.filter(c => c.src !== id);
  // Its sound stops with it, even if another Space is shown
  if (S.player.url === gone.url) { invoke('player_pause'); S.player = { url: '', playing: false, live: false }; }
  if (S.active === id) {
    S.active = null;
    Object.assign(S.p, { loaded: false, url: '', duration: 0, pos: 0, playing: false, error: '', live: false, view: false });
    const next = S.sources[S.sources.length - 1];
    if (next) { activate(next.id); return; }
  }
  render();
}

async function renameSource(s) {
  const v = await ask('Rename', 'Name:', s.name);
  if (v && v.trim()) { s.name = v.trim(); render(); }
}

function tagMenu(e, s) {
  e.preventDefault();
  openMenu(e.clientX, e.clientY, [
    { label: 'Rename…', run: () => renameSource(s) },
    { label: s.local ? 'Add the whole file' : 'Add the whole Space', run: () => {
      S.clips.push({ ...newClip(s.id), full: true });
      activate(s.id);
    } },
    '-',
    { label: 'Remove', danger: true, off: clipsOf(s.id).some(inJob), run: () => removeSource(s.id) },
  ]);
}

async function copyLink(s, btn) {
  let text = s.url;
  let span = '';
  // A Space on air: a replay link made now, holding all that is out so far,
  // and the bar brought up to it, so the two agree
  if (s.isLive) {
    try {
      const info = await invoke('fresh_replay', { url: s.url, live: s.live || null });
      text = info.url;
      span = `${fmt(0)} – ${fmt(info.duration)}`;
      Object.assign(s, { duration: info.duration, ...liveAt(info.edge, true) });
      if (S.p.url === s.url && S.p.view) {
        Object.assign(S.p, { duration: info.duration, edge: s.edge, edgeAt: s.edgeAt });
        renderPlayer();
        renderRegions();
      }
      pollSoon();
    } catch { /* the link it was opened with */ }
  }
  await invoke('clipboard_write', { text });
  // The tags may have been drawn again meanwhile: this Space's as it is now
  const tag = ui.tags.querySelector(`.tag[data-id="${s.id}"]`);
  btn = tag?.querySelector('.tcopy') || btn;
  if (span) copyNote(tag, 'Link copied', span);
  btn.innerHTML = ICON.check;
  btn.classList.add('done');
  btn.title = 'Copied';
  setTimeout(() => { btn.innerHTML = ICON.copy; btn.classList.remove('done'); btn.title = 'Copy replay link'; }, 1200);
}

/* ── Clips ──────────────────────────────────────────── */

function newClip(src) {
  return { id: S.nextId++, src, name: '', full: false, start: '', end: '', status: 'Pending', auto: false };
}

function removeClip(id) {
  S.clips = S.clips.filter(c => c.id !== id);
  if (S.sel === id) S.sel = null;
  if (S.pending === id) S.pending = null;
  render();
}

function select(id) {
  if (S.sel === id) return;
  S.sel = id;
  $$('.clip', ui.clips).forEach(el => el.classList.toggle('sel', Number(el.dataset.id) === id));
  renderRegions();
}

function flashClip(id) {
  const el = ui.clips.querySelector(`.clip[data-id="${id}"]`);
  if (!el) return;
  el.classList.add('flash');
  el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  setTimeout(() => el.classList.remove('flash'), 500);
}

/** The clip a new period goes into: one of this source still without times, or a new one. */
function freshClip() {
  const s = active();
  S.clips = S.clips.filter(c => !(c.src === s.id && c.auto));
  let c = S.clips.find(x => x.src === s.id && !x.full && !x.start.trim() && !x.end.trim());
  if (!c) { c = newClip(s.id); S.clips.push(c); }
  return c;
}

/* ── Rendering ──────────────────────────────────────── */

function render() {
  renderTags();
  renderClips();
  renderPlayer();
  renderRegions();
  renderMerge();
  renderFoot();
}

function renderTags() {
  ui.tags.textContent = '';
  for (const s of S.sources) {
    const copy = s.local ? null : h('span', {
      class: 'tcopy', role: 'button', title: 'Copy replay link', html: ICON.copy,
      onclick: e => { e.stopPropagation(); copyLink(s, e.currentTarget); },
      ondblclick: e => e.stopPropagation(),
    });
    ui.tags.append(h('div', {
      class: `tag${s.id === S.active ? ' on' : ''}${s.id === S.newTag ? ' new' : ''}`,
      role: 'button',
      dataset: { id: String(s.id) },
      style: colorVars(s),
      title: s.local ? s.url : s.name,
      onclick: () => activate(s.id),
      ondblclick: () => renameSource(s),
      oncontextmenu: e => tagMenu(e, s),
    }, h('span', { class: 't' }, s.name), tagMark(s), copy));
  }
  S.newTag = null;
}

/** Another tag's mark in its colour, a dot or three bars of sound (as chosen
 *  by the time): still, and alive while that Space is the one heard. The open
 *  Space has its own by the time. */
function tagMark(s) {
  if (s.id === S.active) return null;
  const heard = S.player.url === s.url && S.player.playing;
  return h('span', { class: 'sm', title: heard ? 'Playing' : '' },
    h('span', { class: `dot${heard ? ' heard' : ''}` }),
    h('span', { class: `wave${heard ? '' : ' still'}` }, h('i'), h('i'), h('i')));
}

function renderClips() {
  const focus = captureFocus();
  ui.clips.textContent = '';
  const s = active();
  if (!s) return;
  const list = clipsOf(s.id);
  if (!list.length) {
    ui.clips.append(h('div', { class: 'empty' }, 'Press Mark start while you listen, or drag across the bar'));
    return;
  }
  list.forEach((c, i) => ui.clips.append(buildClip(c, i, s)));
  restoreFocus(focus);
}

function buildClip(c, i, s) {
  const locked = inJob(c);
  const st = statusOf(c);
  const el = h('div', { class: `clip${S.sel === c.id ? ' sel' : ''}`, dataset: { id: String(c.id) }, style: colorVars(s) },
    h('span', { class: 'cnum', title: 'Play from here', 'data-act': 'seek' }, String(i + 1)),
    h('input', { class: 'cname', 'data-col': 'name', value: c.name, placeholder: 'Add a title, or get a random name', spellcheck: 'false', autocomplete: 'off', disabled: locked,
      title: 'File name. Left empty, the clip gets a unique name such as cut_1_20261030-041522' }));
  if (c.full) {
    el.append(h('span', { class: 'whole' }, s.local ? 'Whole file' : 'Whole Space'));
  } else {
    el.append(
      h('input', { class: 'ts', 'data-col': 'start', value: c.start, placeholder: '00:00:00', spellcheck: 'false', autocomplete: 'off', disabled: locked }),
      h('input', { class: 'ts', 'data-col': 'end', value: c.end, placeholder: '00:00:00', spellcheck: 'false', autocomplete: 'off', disabled: locked }));
  }
  el.append(
    h('span', { class: `st ${st[1]}`, title: (st[1] === 'bad' && c.reason) || st[2] || '', html: ST_ICON[st[0]] || '' }),
    h('button', { class: 'more', 'data-act': 'menu', html: ICON.dots, title: 'More', disabled: locked }));
  return el;
}

function syncStatus(c) {
  const el = ui.clips.querySelector(`.clip[data-id="${c.id}"] .st`);
  if (!el) return;
  const st = statusOf(c);
  el.className = `st ${st[1]}${c.status === 'Done' ? ' fresh' : ''}`;
  el.innerHTML = ST_ICON[st[0]] || '';
  el.title = (st[1] === 'bad' && c.reason) || st[2] || '';
}

function captureFocus() {
  const a = document.activeElement;
  const row = a && a.closest && a.closest('.clip');
  if (!row || !a.dataset.col) return null;
  return { id: row.dataset.id, col: a.dataset.col, start: a.selectionStart, end: a.selectionEnd };
}

function restoreFocus(f) {
  if (!f) return;
  const el = ui.clips.querySelector(`.clip[data-id="${f.id}"] [data-col="${f.col}"]`);
  if (!el || el.disabled) return;
  el.focus();
  try { el.setSelectionRange(f.start, f.end); } catch { /* not a text field */ }
}

function renderFoot() {
  const n = S.sources.reduce((a, s) => a + clipsOf(s.id).filter(usable).length, 0);
  const cut = $('#cut');
  $('.label', cut).textContent = S.running ? `Cutting ${Math.min(S.done + 1, S.total)} of ${S.total}…` : n ? `Cut ${n} clip${n === 1 ? '' : 's'}` : 'Cut';
  cut.disabled = !S.running && !n;
  $('#cancel').classList.toggle('hidden', !S.running);
  $('#join').checked = S.join;
  $('#join').disabled = S.running;
  const keep = $('#keep');
  keep.classList.toggle('on', S.keep);
  keep.title = S.keep ? 'Each clip is also saved on its own' : 'Only the merged file is saved';
  if (!keep.firstChild) {
    keep.innerHTML = '<span class="kbox"><svg viewBox="0 0 16 16"><path d="M4 8.4l2.6 2.6L12 5.6"/></svg></span><span>Keep clips</span>';
  }
}

function renderFolder() {
  const f = S.env.output_folder;
  const b = $('#folder');
  b.innerHTML = ICON.folder;
  b.append(h('span', {}, f ? baseName(f.replace(/[\\/]+$/, '')) || f : 'Choose folder'));
  b.title = f ? `Saves to ${f}` : 'Choose where to save the clips';
}

/** The folder the user picks; false if they cancel. */
async function chooseFolder() {
  const f = await invoke('pick_folder').catch(() => null);
  if (!f) return false;
  S.env.output_folder = f;
  savePref('output_folder', f);
  renderFolder();
  return true;
}

/* ── Player ─────────────────────────────────────────── */

function currentPos() {
  const p = S.p;
  let pos = p.pos;
  if (p.playing && !p.buffering) pos += ((performance.now() - p.at) / 1000) * S.speed;
  return p.duration ? Math.min(pos, p.duration) : pos;
}

function pendingClip() {
  const c = clipById(S.pending);
  return c && c.src === S.active && !c.full && parseHMS(c.start) != null && parseHMS(c.end) == null ? c : null;
}

function showDur() {
  $('#dur').textContent = `/ ${fmt(S.p.duration)}`;
}

/** The big time: where the playhead is, or, clicked, what is left until the end (as on YouTube). */
S.timeLeft = (() => { try { return localStorage.getItem('timeLeft') === '1'; } catch { return false; } })();
function showPos(pos) {
  const t = S.timeLeft ? `−${fmt(Math.max(0, S.p.duration - pos))}` : fmt(pos);
  if (ui.pos.textContent !== t) ui.pos.textContent = t;
}
ui.pos.title = 'Click for the time left';
ui.pos.addEventListener('click', () => {
  S.timeLeft = !S.timeLeft;
  try { localStorage.setItem('timeLeft', S.timeLeft ? '1' : '0'); } catch { /* kept for this time only */ }
  ui.pos.title = S.timeLeft ? 'Time left: click for where it is' : 'Click for the time left';
  S.shownSec = -1;
  kick();
});

function renderPlayer() {
  const p = S.p;
  const s = active();
  showDur();
  showPlaying();
  const ready = !!s && p.loaded && !p.loading;
  for (const id of ['speed', 'play', 'back', 'fwd', 'mark']) $(`#${id}`).disabled = !ready;
  // Marking follows what is heard, and that is another Space now
  if (p.view) for (const id of ['speed', 'mark']) $(`#${id}`).disabled = true;
  S.shownLive = '';
  renderLive();
  ui.bar.classList.toggle('off', !ready || !p.duration);
  if (s) for (const [k, v] of Object.entries(colorVars(s))) document.documentElement.style.setProperty(k, v);
  const mark = $('#mark');
  const pend = !!pendingClip();
  // While a clip is being marked: a blinking red ring and how long it is so far
  mark.classList.toggle('rec', pend);
  mark.innerHTML = pend
    ? '<span>Mark end</span><span class="rec-len"></span>'
    : '<span>Mark start</span>';
  S.shownSec = -1;
  S.shownRec = -1;
  renderMsg();
  kick();
}

/** The line next to the mark button; `text` shows a note for a moment. */
function flash(text, kind = '') {
  ui.msg.textContent = text;
  ui.msg.className = `msg ${kind}`;
  clearTimeout(S.msgTimer);
  S.msgTimer = setTimeout(() => { S.msgTimer = 0; renderMsg(); }, 2800);
}

function renderMsg() {
  if (S.msgTimer) return;
  const p = S.p;
  let t = 'Paste a dynamic link at the top to start';
  let k = '';
  if (p.error) { t = p.error; k = 'bad'; }
  else if (p.loading) t = 'Loading…';
  else if (p.playing && p.buffering) t = 'Buffering…';
  else if (pendingClip()) t = 'Now mark the end';
  else if (p.live && !p.view && p.playing && behindLive() < AT_LIVE) t = 'Listening live';
  else if (p.live && !p.view && liveGap()) t = `${fmtLen(liveGap())} behind live`;
  else if (p.loaded) t = 'or drag across the bar';
  if (ui.msg.textContent !== t) ui.msg.textContent = t;
  ui.msg.className = `msg ${k}`;
}

/** How far behind live a Space on air counts as live still, and how far back "live" plays. */
const AT_LIVE = 30;
/** How far behind live the playhead is: from when the newest piece known
 *  came out, as the clock has gone on since, with nothing asked for. */
const behindLive = () => {
  const p = S.p;
  const edge = p.edge ? p.edge + (performance.now() - p.edgeAt) / 1000 : p.duration;
  return Math.round(Math.max(0, edge - currentPos()));
};
/** How far live has got past the end of the bar (it stops while you listen
 *  well behind), in steps: 5 s, 15 s, 30 s, then every 15 s, every 30 s past
 *  5 minutes and every minute past 10. 0 while the bar keeps up with live. */
function liveGap() {
  const p = S.p;
  if (!p.edge) return 0;
  const gap = p.edge + (performance.now() - p.edgeAt) / 1000 - p.duration;
  if (gap < 5) return 0;
  if (gap < 15) return 5;
  const step = gap < 300 ? 15 : gap < 600 ? 30 : 60;
  return Math.floor(gap / step) * step;
}

/** Where live was, for a Space or the player: kept with the moment it was so. */
const liveAt = (edge, live) => (live ? { edge, edgeAt: performance.now() } : { edge: 0, edgeAt: 0 });

/** Shows a Space in the player. `force` loads it even while another one plays. */
async function loadPlayer(s, force = false) {
  if (S.p.url === s.url && (S.p.loaded || S.p.loading)) return;
  const token = ++S.loadToken;
  S.actionAt = performance.now();
  // Still in the player (heard from another tag): carry on with it
  if (S.player.url === s.url) {
    const st = await invoke('player_state').catch(() => null);
    if (token !== S.loadToken) return;
    if (st && st.loaded && st.url === s.url) {
      Object.assign(S.p, { loading: false, loaded: true, view: false, url: s.url, duration: st.duration, pos: st.pos, at: performance.now(),
        playing: st.playing, buffering: st.buffering, ended: st.ended, error: st.error || '', live: st.live, ...liveAt(st.edge, st.live) });
      shown();
      return;
    }
  }
  // What plays keeps playing while another Space is looked at; Play there takes over.
  // A Space seen before shows at once as it was; one on air as a replay of
  // what was out by then, its length asked for only the first time.
  if (!force && S.player.playing) {
    const known = s.duration > 0;
    Object.assign(S.p, { loading: !known, loaded: known, view: true, playing: false, buffering: false, error: '', url: s.url,
      duration: s.duration || 0, pos: Math.min(s.lastPos || 0, s.duration || 0), at: performance.now(), ended: false, live: !!s.isLive,
      edge: s.edge || 0, edgeAt: s.edgeAt || 0 });
    shown();
    if (known) return;
    try {
      const info = await invoke('space_info', { url: s.url, live: s.live || null });
      if (token !== S.loadToken) return;
      Object.assign(s, { duration: info.duration, isLive: info.live, ...liveAt(info.edge, info.live) });
      Object.assign(S.p, { loading: false, loaded: true, duration: info.duration, pos: Math.min(s.lastPos || 0, info.duration), live: info.live,
        edge: s.edge, edgeAt: s.edgeAt });
    } catch (e) {
      if (token !== S.loadToken) return;
      if (!known) Object.assign(S.p, { loading: false, error: String(e) });
    }
    shown();
    return;
  }
  // Loading: the time it was left at shows meanwhile, not 00:00:00
  Object.assign(S.p, { loading: true, loaded: false, view: false, playing: false, buffering: false, error: '', url: s.url, duration: s.duration || 0, pos: s.lastPos || 0, live: false });
  renderPlayer();
  try {
    const info = await invoke('player_load', { url: s.url, live: s.live || null });
    if (token !== S.loadToken) return;
    S.player = { url: s.url, playing: false, live: info.live };
    Object.assign(s, { duration: info.duration, isLive: info.live, ...liveAt(info.edge, info.live) });
    Object.assign(S.p, { loading: false, loaded: true, duration: info.duration, pos: 0, at: performance.now(), ended: false, live: info.live,
      edge: s.edge, edgeAt: s.edgeAt });
    // Back where it was left; a Space on air opens as a replay, GO LIVE syncs it
    if (s.lastPos > 0) await seek(s.lastPos);
  } catch (e) {
    if (token !== S.loadToken) return;
    Object.assign(S.p, { loading: false, error: String(e) });
  }
  shown();
}

function shown() {
  S.actionAt = performance.now();
  renderTags();
  renderPlayer();
  renderRegions();
  renderMerge();
}

/** Play pressed on a Space only looked at: it takes the player over from the one playing. */
async function playHere() {
  const s = active();
  if (!s) return;
  const at = S.p.pos;
  S.p.url = '';
  await loadPlayer(s, true);
  if (!S.p.loaded || S.p.view) return;
  if (at > 0) await seek(at);
  await togglePlay();
}

/** GO LIVE: what came out since (a fresh replay), and on from live, playing. */
async function goLive() {
  const s = active();
  if (!s || !S.p.loaded || !S.p.live) return;
  if (S.p.view) {
    S.p.url = '';
    await loadPlayer(s, true);
    if (!S.p.loaded || S.p.view) return;
  }
  S.actionAt = performance.now();
  Object.assign(S.p, { buffering: true, playing: true, error: '' });
  showPlaying();
  try { await invoke('player_go_live'); }
  catch (e) { S.p.error = String(e); renderMsg(); }
  S.actionAt = 0;
  pollSoon();
}

/** A note of two lines above a tag for a few seconds, pointing at it. */
function copyNote(tag, title, detail) {
  document.querySelectorAll('.copy-note').forEach(n => n.remove());
  if (!tag) return;
  const note = h('div', { class: 'copy-note' }, h('b', {}, title), h('span', {}, detail));
  document.body.append(note);
  const t = tag.getBoundingClientRect();
  const w = note.offsetWidth;
  const centre = t.left + t.width / 2;
  // Kept inside the window; its point stays on the tag
  const left = Math.max(8, Math.min(centre - w / 2, window.innerWidth - w - 8));
  note.style.left = `${left}px`;
  note.style.top = `${t.top - note.offsetHeight - 9}px`;
  note.style.setProperty('--point', `${centre - left}px`);
  setTimeout(() => note.remove(), 3500);
}

/** LIVE beside the time, for a Space on air. */
function renderLive() {
  const p = S.p;
  const on = p.loaded && p.live;
  const heard = on && !p.view && p.playing && behindLive() < AT_LIVE;
  const key = `${on}${heard}`;
  if (key === S.shownLive) return;
  S.shownLive = key;
  const pill = $('#livePill');
  pill.classList.toggle('hidden', !on);
  pill.classList.toggle('on', heard);
  pill.innerHTML = `<i></i>${heard ? 'LIVE' : 'GO LIVE'}`;
  pill.title = heard ? 'You are listening live' : 'Back to live';
  $('#liveDot').classList.toggle('hidden', on || !p.loaded || !active());
}

async function togglePlay() {
  const p = S.p;
  if (!p.loaded || p.loading) return;
  if (p.view) { await playHere(); return; }
  S.actionAt = performance.now();
  try {
    if (p.playing) {
      await invoke('player_pause');
      p.pos = currentPos();
      p.playing = false;
    } else {
      if (!p.live && (p.ended || (p.duration && p.pos >= p.duration - 0.5))) p.pos = 0;
      await invoke('player_play');
      p.playing = true;
      p.buffering = true;
      p.error = '';
    }
  } catch (e) {
    p.error = String(e);
  }
  p.at = performance.now();
  S.actionAt = performance.now();
  showPlaying();
  renderMsg();
  kick();
  pollSoon();
}

async function seek(t) {
  const p = S.p;
  if (!p.loaded) return;
  const to = Math.max(0, p.duration ? Math.min(t, p.duration) : t);
  // Only looked at: the playhead moves here, the player plays on
  if (p.view) {
    Object.assign(p, { pos: to, at: performance.now() });
    S.shownSec = -1;
    kick();
    return;
  }
  S.actionAt = performance.now();
  Object.assign(p, { pos: to, at: performance.now(), buffering: true, ended: false });
  S.shownSec = -1;
  kick();
  try { await invoke('player_seek', { t: to }); }
  catch (e) { p.error = String(e); renderMsg(); }
  S.actionAt = performance.now();
  pollSoon();
}

async function poll() {
  const sent = performance.now();
  let st;
  try { st = await invoke('player_state'); } catch { return; }
  if (S.fxPath && !st.fx && performance.now() - S.fxSince > 500) { S.fxPath = null; repaintFx(); }
  // What the player has, whichever Space is shown: for the tags' dots
  const had = S.player;
  S.player = { url: st.loaded ? st.url : '', playing: st.playing, live: st.live };
  const src = S.sources.find(x => x.url === S.player.url);
  if (src && src.isLive !== st.live) src.isLive = st.live;
  if (src && (had.url !== S.player.url || had.playing !== st.playing || had.live !== st.live)) { renderTags(); showPlaying(); }
  const p = S.p;
  if (sent < S.actionAt || !p.loaded || p.view || st.url !== p.url) return;
  const changed = p.playing !== st.playing || p.buffering !== st.buffering || p.error !== (st.error || '') || p.live !== st.live;
  // A Space on air grows as it plays
  const grew = Math.abs(p.duration - st.duration) > 0.01;
  Object.assign(p, { pos: st.pos, at: performance.now(), playing: st.playing, buffering: st.buffering, ended: st.ended, error: st.error || '', live: st.live, duration: st.duration,
    ...liveAt(st.edge, st.live) });
  const shownSrc = active();
  if (shownSrc && shownSrc.url === p.url) Object.assign(shownSrc, { duration: p.duration, isLive: p.live, edge: p.edge, edgeAt: p.edgeAt });
  if (grew) {
    showDur();
    S.shownSec = -1;
    S.shownPx = -1;
    S.shownBand = null;
    renderRegions();
  }
  if (changed) { showPlaying(); renderPlayer(); }
  // While it plays, each new second does this (see `frame`)
  else if (p.live && !p.playing) { renderLive(); renderMsg(); }
  kick();
}

/** The play button, and the Space's mark by the time: moving while it plays. */
function showPlaying() {
  const p = S.p;
  ui.play.innerHTML = p.playing ? ICON.pause : ICON.play;
  // A clip being marked while paused: the ring stays red without blinking,
  // so it is clear the playhead can be moved, back or on, to set its end
  $('#mark').classList.toggle('still', !(p.playing && !p.buffering));
  const dot = $('#liveDot');
  dot.classList.toggle('hidden', p.live || !p.loaded || !active());
  dot.classList.toggle('on', p.playing && !p.buffering);
  // The logo's dots become a sound meter while a Space plays, even one heard from another tag
  document.body.classList.toggle('playing', (p.playing && !p.buffering) || (p.view && S.player.playing));
}

/** Asks the player often while it plays or loads, and rarely while it sits still. */
function pollLoop() {
  clearTimeout(S.pollTimer);
  poll().finally(() => {
    const busy = S.p.playing || S.p.loading || S.p.buffering || S.fxPath;
    S.pollTimer = setTimeout(pollLoop, busy ? 120 : 1000);
  });
}
const pollSoon = () => { clearTimeout(S.pollTimer); S.pollTimer = setTimeout(pollLoop, 60); };

/** Draws the playhead on the next frame; frames keep coming only while it moves. */
function kick() {
  if (S.frameQueued) return;
  S.frameQueued = true;
  requestAnimationFrame(frame);
}

function frame() {
  S.frameQueued = false;
  const p = S.p;
  const pos = currentPos();
  const sec = Math.floor(pos);
  if (sec !== S.shownSec) {
    S.shownSec = sec;
    showPos(sec);
    if (p.live) { renderLive(); renderMsg(); }
  }
  // Touch the page only when the playhead moves a whole pixel
  const w = ui.bar.clientWidth;
  const px = p.duration ? Math.round((pos / p.duration) * w) : 0;
  if (px !== S.shownPx) {
    S.shownPx = px;
    ui.knob.style.left = `${px}px`;
    ui.played.style.width = `${px}px`;
  }
  // A marked start stretches toward the playhead until its end is marked
  const c = pendingClip();
  const a = c ? parseHMS(c.start) : null;
  const rec = a != null ? Math.abs(sec - a) : -1;
  if (rec !== S.shownRec) {
    S.shownRec = rec;
    const len = $('#mark .rec-len');
    if (len) len.textContent = rec >= 0 ? fmtLen(rec) : '';
  }
  // ... either way: the playhead taken back before it stretches the same
  let band = '';
  if (a != null && p.duration && Math.abs(pos - a) >= 0.5) {
    const ax = Math.round((a / p.duration) * w);
    band = `${Math.min(ax, px)}:${Math.max(ax, px)}`;
  }
  if (band !== S.shownBand) {
    S.shownBand = band;
    ui.pending.classList.toggle('hidden', !band);
    if (band) {
      const [from, to] = band.split(':').map(Number);
      ui.pending.style.left = `${from}px`;
      ui.pending.style.width = `${to - from}px`;
    }
  }
  if (S.drag) { kick(); return; }
  // Playing, the page only changes with a new second or a pixel more of the
  // bar: wake for that, not on every frame
  if (p.playing && !p.buffering) {
    const toSec = Math.floor(pos) + 1 - pos;
    const toPx = p.duration && w ? ((px + 0.5) / w) * p.duration - pos : toSec;
    const wait = Math.max(0.016, Math.min(toSec, toPx) / (S.speed || 1));
    clearTimeout(S.frameTimer);
    S.frameTimer = setTimeout(kick, wait * 1000);
  }
}
new ResizeObserver(() => { S.shownPx = -1; S.shownBand = null; kick(); }).observe(ui.bar);

/* ── Marking periods ────────────────────────────────── */

const pctOf = t => Math.max(0, Math.min(100, (t / S.p.duration) * 100));
const timeAt = x => {
  const b = ui.bar.getBoundingClientRect();
  return Math.max(0, Math.min(1, (x - b.left) / b.width)) * S.p.duration;
};

function renderRegions() {
  ui.regions.textContent = '';
  const s = active();
  if (!s || !S.p.loaded || !S.p.duration || S.p.url !== s.url) return;
  clipsOf(s.id).forEach((c, i) => {
    if (c.full) return;
    const a = parseHMS(c.start);
    const b = parseHMS(c.end);
    if (a == null) return;
    if (b == null || b <= a) {
      ui.regions.append(h('div', { class: 'markline', style: { left: `${pctOf(a)}%` } }));
      return;
    }
    ui.regions.append(h('div', {
      class: `rg${S.sel === c.id ? ' sel' : ''}`,
      style: { left: `${pctOf(a)}%`, width: `${pctOf(b) - pctOf(a)}%` },
      dataset: { id: String(c.id) },
    }, h('span', {}, String(i + 1))));
  });
  const d = S.drag;
  if (d && d.preview) {
    const [a, b] = d.preview;
    ui.regions.append(h('div', { class: 'rg ghost', style: { left: `${pctOf(a)}%`, width: `${pctOf(b) - pctOf(a)}%` } }));
  }
}

function markStart() {
  if (!S.p.loaded || S.p.view) return;
  const t = Math.round(currentPos());
  const c = freshClip();
  touched(c);
  c.start = fmt(t);
  S.pending = c.id;
  S.sel = c.id;
  render();
  flashClip(c.id);
}

function markEnd() {
  const c = pendingClip();
  if (!c) return;
  touched(c);
  const t = Math.round(currentPos());
  const a = parseHMS(c.start);
  if (t === a) { flash('The end has to come after the start'); return; }
  if (t < a) { c.end = c.start; c.start = fmt(t); } else { c.end = fmt(t); }
  S.pending = null;
  render();
  flashClip(c.id);
}

const toggleMark = () => (pendingClip() ? markEnd() : markStart());

function addPeriod(a, b) {
  const c = freshClip();
  c.start = fmt(a);
  c.end = fmt(b);
  S.sel = c.id;
  if (S.pending === c.id) S.pending = null;
  render();
  flashClip(c.id);
}

/** How close (in pixels, either side) the pointer has to come to a period's edge to stick to it. */
const SNAP_PX = 10;

/** The period edge nearest the pointer, if it is close enough to stick to. */
function snapAt(x) {
  const s = active();
  if (!s || !S.p.duration) return null;
  const b = ui.bar.getBoundingClientRect();
  let best = null;
  clipsOf(s.id).forEach((c, i) => {
    if (c.full) return;
    for (const [part, t] of [['start', parseHMS(c.start)], ['end', parseHMS(c.end)]]) {
      if (t == null) continue;
      const d = Math.abs(x - (b.left + (t / S.p.duration) * b.width));
      if (d <= SNAP_PX && (!best || d < best.d)) best = { c, part, t, d, n: i + 1 };
    }
  });
  return best;
}

/** What the pointer is over on the bar: a period's edge, its body, or nothing. */
function hitAt(x) {
  const edge = snapAt(x);
  if (edge && parseHMS(edge.c.start) != null && parseHMS(edge.c.end) != null) return { c: edge.c, part: edge.part };
  const regs = $$('.rg:not(.ghost)', ui.regions).reverse();
  for (const el of regs) {
    const b = el.getBoundingClientRect();
    const c = clipById(Number(el.dataset.id));
    if (c && x > b.left && x < b.right) return { c, part: 'body' };
  }
  return null;
}

/** A period under the pointer that can be moved: not one being cut right now. */
function freeHit(x) {
  const hit = hitAt(x);
  return hit && !inJob(hit.c) ? hit : null;
}

function showTip(x) {
  const b = ui.bar.getBoundingClientRect();
  const snap = S.drag && S.drag.moved ? null : snapAt(x);
  const frac = snap ? snap.t / S.p.duration : Math.max(0, Math.min(1, (x - b.left) / b.width));
  ui.tip.textContent = snap ? `${fmt(snap.t)} · ${snap.part} of ${snap.n}` : fmt(frac * S.p.duration);
  ui.tip.classList.toggle('snap', !!snap);
  ui.tip.classList.remove('hidden');
  const half = ui.tip.offsetWidth / 2;
  ui.tip.style.left = `${Math.max(half, Math.min(b.width - half, frac * b.width))}px`;
  ui.snap.classList.toggle('hidden', !snap);
  if (snap) ui.snap.style.left = `${frac * 100}%`;
}

function syncTimes(c) {
  const row = ui.clips.querySelector(`.clip[data-id="${c.id}"]`);
  if (!row) return;
  const a = $('[data-col=start]', row);
  const b = $('[data-col=end]', row);
  if (a) a.value = c.start;
  if (b) b.value = c.end;
}

ui.bar.addEventListener('pointerdown', e => {
  if (e.button !== 0 || !S.p.loaded || !S.p.duration) return;
  e.preventDefault();
  ui.bar.setPointerCapture(e.pointerId);
  const hit = freeHit(e.clientX);
  S.drag = { x0: e.clientX, t0: timeAt(e.clientX), hit, moved: false, preview: null,
             a: hit ? parseHMS(hit.c.start) : 0, b: hit ? parseHMS(hit.c.end) : 0 };
  if (hit) select(hit.c.id);
});

ui.bar.addEventListener('pointermove', e => {
  if (!S.p.loaded || !S.p.duration) return;
  showTip(e.clientX);
  const d = S.drag;
  if (!d) {
    const hit = freeHit(e.clientX);
    ui.bar.style.cursor = !hit ? 'pointer' : hit.part === 'body' ? 'grab' : 'ew-resize';
    return;
  }
  if (!d.moved && Math.abs(e.clientX - d.x0) < 4) return;
  d.moved = true;
  const t = Math.round(timeAt(e.clientX));
  const end = Math.floor(S.p.duration);
  if (d.hit) {
    let { a, b } = d;
    if (d.hit.part === 'start') a = Math.min(t, b - 1);
    else if (d.hit.part === 'end') b = Math.max(t, a + 1);
    else {
      const len = d.b - d.a;
      a = Math.max(0, Math.min(end - len, d.a + Math.round(timeAt(e.clientX) - d.t0)));
      b = a + len;
    }
    d.hit.c.start = fmt(a);
    d.hit.c.end = fmt(Math.min(b, end));
    touched(d.hit.c);
    syncTimes(d.hit.c);
    renderRegions();
    renderMerge();
  } else {
    const t0 = Math.round(d.t0);
    d.preview = [Math.min(t0, t), Math.max(t0, t)];
    renderRegions();
  }
});

ui.bar.addEventListener('pointerup', e => {
  const d = S.drag;
  S.drag = null;
  if (!d) return;
  // A click near a period's edge lands exactly on it, so play starts right there
  if (!d.moved) { const snap = snapAt(e.clientX); seek(snap ? snap.t : timeAt(e.clientX)); }
  else if (d.preview && d.preview[1] - d.preview[0] >= 1) addPeriod(d.preview[0], d.preview[1]);
  renderRegions();
});
ui.bar.addEventListener('pointercancel', () => { S.drag = null; renderRegions(); });
ui.bar.addEventListener('pointerleave', () => {
  if (S.drag) return;
  ui.tip.classList.add('hidden');
  ui.snap.classList.add('hidden');
});

/* ── Clip list events ───────────────────────────────── */

ui.clips.addEventListener('input', e => {
  const t = e.target;
  const c = clipById(Number(t.closest('.clip')?.dataset.id));
  if (!c) return;
  const col = t.dataset.col;
  touched(c);
  if (col === 'name') { c.name = t.value; renderMerge(); return; }
  const v = autoFormat(t.value);
  if (v !== t.value) t.value = v;
  c[col] = v;
  c.auto = false;
  renderRegions();
  renderMerge();
  renderFoot();
});

ui.clips.addEventListener('change', e => {
  const t = e.target;
  const c = clipById(Number(t.closest('.clip')?.dataset.id));
  if (!c || (t.dataset.col !== 'start' && t.dataset.col !== 'end')) return;
  c[t.dataset.col] = t.value = autoFormat(t.value.trim());
  renderRegions();
  renderMerge();
  renderFoot();
});

ui.clips.addEventListener('focusin', e => {
  const c = clipById(Number(e.target.closest('.clip')?.dataset.id));
  if (c) select(c.id);
  if (e.target.classList.contains('ts')) e.target.select();
});

// One click on a timestamp selects all of it, so typing replaces it straight away
ui.clips.addEventListener('mousedown', e => {
  const t = e.target;
  if (!t.classList.contains('ts') || t.disabled || document.activeElement === t) return;
  e.preventDefault();
  t.focus();
  t.select();
});

ui.clips.addEventListener('click', async e => {
  const btn = e.target.closest('[data-act]');
  const c = clipById(Number(e.target.closest('.clip')?.dataset.id));
  if (!btn || !c) return;
  if (btn.dataset.act === 'seek') {
    select(c.id);
    const a = parseHMS(c.start);
    if (!c.full && a == null) return;
    if (!c.full) await seek(a);
    // It plays from there, even if paused or another Space was playing
    if (S.p.loaded && (!S.p.playing || S.p.view)) await togglePlay();
  } else if (btn.dataset.act === 'menu') {
    const r = btn.getBoundingClientRect();
    clipMenu(r.right - 200, r.bottom + 4, c);
  }
});

ui.clips.addEventListener('contextmenu', e => {
  const c = clipById(Number(e.target.closest('.clip')?.dataset.id));
  if (!c || inJob(c) || e.target.closest('input')) return;
  e.preventDefault();
  clipMenu(e.clientX, e.clientY, c);
});

function clipMenu(x, y, c) {
  const s = srcById(c.src);
  openMenu(x, y, [
    { label: 'Play from the start', off: c.full || parseHMS(c.start) == null, run: () => seek(parseHMS(c.start)) },
    { label: c.full ? 'Cut a part instead' : (s.local ? 'Whole file instead' : 'Whole Space instead'), run: () => {
      touched(c);
      c.full = !c.full;
      c.auto = false;
      render();
    } },
    '-',
    { label: 'Delete', danger: true, run: () => removeClip(c.id) },
  ]);
}

/* ── Merged file: order, effects and silence ───────── */

/** Clips going into the merged file, in their order; new clips join at the end. */
function mergeClips() {
  const known = new Set(S.clips.map(c => c.id));
  S.order = S.order.filter(id => known.has(id));
  for (const s of S.sources) for (const c of clipsOf(s.id)) if (!S.order.includes(c.id)) S.order.push(c.id);
  return S.order.map(clipById).filter(c => usable(c));
}

const clipLen = c => {
  if (c.full) return srcById(c.src).duration || 0;
  return Math.max(0, parseHMS(c.end) - parseHMS(c.start));
};
const soundName = path => (S.env.effects.find(e => e.path === path) || {}).name || stem(path);
/** Lengths of the sound effects, asked of ffmpeg once each; the merge redraws when one comes in. */
const soundLens = new Map();
function soundLen(path) {
  if (!soundLens.has(path)) {
    soundLens.set(path, 0);
    invoke('sound_length', { path })
      .then(len => { soundLens.set(path, len || 0); if (len) renderMerge(); })
      .catch(() => {});
  }
  return soundLens.get(path);
}
const gapAt = key => (key === 'before' || key === 'after' ? S.gaps[key] : S.gaps.mid[key] || { path: '', gap: 0 });
function setGap(key, v) {
  if (key === 'before' || key === 'after') S.gaps[key] = v;
  else S.gaps.mid[key] = v;
}

function renderMerge() {
  const box = $('#mergeBox');
  box.classList.toggle('hidden', !S.join);
  if (!S.join) return;
  const list = $('#mergeList');
  list.textContent = '';
  const items = mergeClips();
  if (!items.length) {
    list.append(h('div', { class: 'mempty' }, 'Mark some clips and they line up here'));
    $('#mergeLen').textContent = '';
    return;
  }
  const sep = key => {
    const g = gapAt(key);
    let label = key === 'before' ? '+  Add sound effect or silence' : '+';
    let kind = '';
    if (g.path) { label = `♪  ${soundName(g.path)}`; kind = 'fx'; }
    else if (g.gap > 0) { label = `${g.gap.toFixed(1)}s silence`; kind = 'gap'; }
    return h('div', { class: `msep ${kind}`, dataset: { key: String(key) }, title: 'Add a sound effect or a silence here' }, h('i'), h('span', {}, label), h('i'));
  };
  let total = 0;
  list.append(sep('before'));
  items.forEach((c, i) => {
    const s = srcById(c.src);
    const k = clipsOf(c.src).indexOf(c) + 1;
    const len = clipLen(c);
    total += len;
    list.append(h('div', { class: 'mitem', dataset: { id: String(c.id) }, style: colorVars(s) },
      h('span', { class: 'grip', html: ICON.grip, title: 'Drag to move' }),
      h('span', { class: 'mdot' }),
      h('span', { class: 'mname' }, c.name.trim() || `${s.name} · clip ${k}`),
      h('span', { class: 'mlen' }, len ? fmt(len) : '—')));
    list.append(sep(i < items.length - 1 ? i : 'after'));
  });
  const extra = ['before', 'after', ...items.slice(1).map((_, i) => i)]
    .reduce((a, k) => { const g = gapAt(k); return a + (g.path ? soundLen(g.path) : g.gap); }, 0);
  $('#mergeLen').textContent = `≈ ${fmt(Math.round(total + extra))}`;
}

$('#mergeList').addEventListener('click', e => {
  const sep = e.target.closest('.msep');
  if (!sep || S.running) return;
  const key = sep.dataset.key === 'before' || sep.dataset.key === 'after' ? sep.dataset.key : Number(sep.dataset.key);
  openGap(key, $('span', sep));
});

// Pick up a clip and move it: it follows the pointer and the others make room
$('#mergeList').addEventListener('pointerdown', e => {
  const item = e.target.closest('.mitem');
  if (!item || S.running || e.button !== 0) return;
  e.preventDefault();
  const list = $('#mergeList');
  const items = $$('.mitem', list);
  const from = items.indexOf(item);
  const tops = items.map(el => el.getBoundingClientRect().top);
  const step = items.length > 1 ? tops[1] - tops[0] : 0;
  const y0 = e.clientY;
  let moved = false;
  let to = from;

  const move = ev => {
    const dy = ev.clientY - y0;
    if (!moved && Math.abs(dy) < 3) return;
    if (!moved) {
      moved = true;
      item.classList.add('lifted');
      list.classList.add('sorting');
    }
    const d = Math.max(tops[0] - tops[from], Math.min(tops[tops.length - 1] - tops[from], dy));
    item.style.transform = `translateY(${d}px) scale(1.02)`;
    to = step ? Math.max(0, Math.min(items.length - 1, Math.round((tops[from] + d - tops[0]) / step))) : from;
    items.forEach((el, k) => {
      if (el === item) return;
      const shift = from < to && k > from && k <= to ? -step : from > to && k >= to && k < from ? step : 0;
      el.style.transform = shift ? `translateY(${shift}px)` : '';
    });
  };

  const up = () => {
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', up);
    if (!moved) return;
    // Settle into the new spot, then redraw in the new order
    item.classList.remove('lifted');
    item.style.transform = `translateY(${tops[to] - tops[from]}px)`;
    const shown = mergeClips().map(c => c.id);
    const [id] = shown.splice(from, 1);
    shown.splice(to, 0, id);
    // Clips left out of the merge keep their place at the end
    S.order = [...shown, ...S.order.filter(x => !shown.includes(x))];
    setTimeout(() => { list.classList.remove('sorting'); renderMerge(); }, 180);
  };

  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', up);
});

function repaintFx() {
  $$('.fx-item').forEach(el => {
    const b = $('.icon', el);
    if (!b) return;
    const on = S.fxPath === el.dataset.path;
    b.classList.toggle('on', on);
    b.innerHTML = on ? ICON.pause : ICON.play;
  });
}

async function toggleFx(path) {
  if (S.fxPath === path) { S.fxPath = null; repaintFx(); invoke('fx_stop'); return; }
  S.fxPath = path;
  S.fxSince = performance.now();
  repaintFx();
  try { await invoke('fx_play', { path }); S.fxSince = performance.now(); }
  catch (e) { S.fxPath = null; repaintFx(); setStatus(String(e), 'bad'); }
}

function saveEffects() {
  invoke('save_effects', { effects: S.env.effects }).catch(() => {});
}

async function addEffect() {
  const [path] = await invoke('pick_files', { kind: 'effect', multiple: false }).catch(() => []);
  if (!path) return null;
  const name = await ask('Name', 'Sound name:', stem(path));
  if (!name || !name.trim()) return null;
  S.env.effects.push({ name: name.trim(), path });
  saveEffects();
  return path;
}

/** The sound or silence at one spot of the merged file. */
function openGap(key, anchor) {
  const cur = gapAt(key);
  let path = cur.path;
  let gap = cur.gap;
  const list = h('div', { class: 'fx-list' });
  const slider = h('input', { type: 'range', min: '0', max: '20', step: '1', value: String(Math.round(gap * 10)) });
  const val = h('span', { class: 'v' }, `${gap.toFixed(1)}s`);
  const gapRow = h('div', { class: 'gap' }, h('label', {}, 'Silence'), slider, val);
  const paint = () => {
    $$('.fx-item', list).forEach(el => el.classList.toggle('on', el.dataset.path === path));
    gapRow.classList.toggle('off', !!path);
    slider.disabled = !!path;
  };
  list.append(h('div', { class: 'fx-item', dataset: { path: '' }, onclick: () => { path = ''; paint(); } }, h('span', { class: 't' }, 'No sound')));
  S.env.effects.forEach((fx, i) => list.append(h('div', {
    class: 'fx-item', dataset: { path: fx.path },
    onclick: ev => { if (ev.target.closest('.icon')) return; path = fx.path; paint(); },
    oncontextmenu: ev => {
      ev.preventDefault();
      ev.stopPropagation();
      openMenu(ev.clientX, ev.clientY, [
        { label: 'Rename…', run: async () => {
          const v = await ask('Rename', 'Sound name:', fx.name);
          if (v && v.trim()) { fx.name = v.trim(); saveEffects(); }
        } },
        { label: 'Remove', danger: true, run: () => { S.env.effects.splice(i, 1); saveEffects(); } },
      ]);
    },
  }, h('span', { class: 't' }, fx.name),
     h('button', { class: 'icon', title: 'Listen', html: ICON.play, onclick: () => toggleFx(fx.path) }))));
  list.append(h('button', { class: 'fx-add', onclick: async () => {
    if (await addEffect()) openGap(key, anchor);
  } }, '+ Add a sound…'));
  slider.addEventListener('input', () => { gap = Number(slider.value) / 10; val.textContent = `${gap.toFixed(1)}s`; });
  const title = key === 'before' ? 'Before the first clip' : key === 'after' ? 'After the last clip' : `Between clip ${key + 1} and ${key + 2}`;
  openPopover(h('div', { class: 'popover' },
    h('div', { class: 'pop-head' }, title),
    list, gapRow,
    h('div', { class: 'pop-actions' },
      h('button', { class: 'soft small', onclick: () => { setGap(key, { path: '', gap: 0 }); closeFloating(); renderMerge(); } }, 'Clear'),
      h('button', { class: 'main', style: { height: '32px', padding: '0 16px', 'font-size': '13px' },
        onclick: () => { setGap(key, { path, gap }); closeFloating(); renderMerge(); } }, 'Done'))), anchor);
  paint();
  repaintFx();
}

$('#join').addEventListener('change', e => { S.join = e.target.checked; renderMerge(); renderFoot(); if (S.join) $('#mergeBox').scrollIntoView({ block: 'nearest', behavior: 'smooth' }); });
$('#keep').addEventListener('click', e => {
  S.keep = !S.keep;
  const b = e.currentTarget;
  b.classList.remove('bump');
  void b.offsetWidth;
  b.classList.add('bump');
  renderFoot();
});
$('#joinName').addEventListener('input', e => { S.joinName = e.target.value; });

/* ── Cutting ────────────────────────────────────────── */

function setStatus(text, kind = '') {
  const el = $('#status');
  el.textContent = text;
  el.className = `status ${kind}`;
}

function setProgress(v) {
  $('#cut .fill').style.width = `${v}%`;
}

function setRunning(on) {
  S.running = on;
  document.body.classList.toggle('cutting', on);
  if (!on) setProgress(0);
  render();
}

async function startCut() {
  if (S.running) return;
  const row = c => {
    const s = srcById(c.src);
    return { id: c.id, url: s.url, live: s.live || '', full: c.full, start: c.start, end: c.end, name: c.name.trim(), length: clipLen(c) };
  };
  // Merging cuts in the merged file's order; otherwise Space by Space
  const rows = S.join
    ? mergeClips().map(row)
    : S.sources.flatMap(s => clipsOf(s.id).filter(usable)).map(row);
  if (!rows.length) return;
  if (!S.env.ffmpeg) { setStatus('FFmpeg not found', 'bad'); return; }
  // Nothing is saved anywhere the user hasn't chosen
  if (!S.env.output_folder && !(await chooseFolder())) {
    // Cancelled: say why nothing happened, for a moment
    const why = 'Choose a folder to save the clips';
    setStatus(why);
    setTimeout(() => { if ($('#status').textContent === why) setStatus(''); }, 3000);
    return;
  }
  S.clips.forEach(c => { c.status = 'Pending'; });
  const jingles = {};
  if (S.join) {
    jingles.j_before = S.gaps.before;
    jingles.j_after = S.gaps.after;
    for (let i = 0; i < rows.length - 1; i++) jingles[`j_${i}`] = gapAt(i);
  }
  const job = { rows, output_folder: S.env.output_folder, merge: S.join, merge_name: S.joinName.trim(), keep_clips: S.keep, jingles };
  S.cancelled = false;
  try { await invoke('start_job', { job }); }
  catch (e) { setStatus(String(e), 'bad'); return; }
  S.jobIds = rows.map(r => r.id);
  S.done = 0;
  S.total = rows.length;
  setRunning(true);
  setProgress(0);
  setStatus('Starting…');
}

function cancelCut() {
  if (!S.running) return;
  S.cancelled = true;
  invoke('cancel_job');
  S.clips.forEach(c => { if (['Cutting', 'Converting', 'Slow'].includes(c.status)) c.status = 'Pending'; });
  setStatus('');
  setRunning(false);
}

listen('job-status', ({ payload }) => {
  const c = clipById(payload.id);
  if (!c || S.cancelled) return;
  c.status = payload.status;
  c.reason = payload.reason || '';
  syncStatus(c);
  if (payload.status === 'Cutting') { S.done = S.jobIds.indexOf(c.id); renderFoot(); }
  // Where this clip is among all the clips being cut: 2/5
  const where = `${S.jobIds.indexOf(c.id) + 1}/${S.total}`;
  const msg = {
    Cutting: `${where} — cutting…`, Converting: `${where} — converting…`, Done: `${where} — done`,
    'Cut Error': `${where} — ${c.reason || 'cut error'}`, 'Convert Error': `${where} — ${c.reason || 'convert error'}`,
    'Invalid Time': `${where} — the end comes before the start`, 'FFmpeg Not Found': 'FFmpeg not found',
    'Past the End': `${where} — ${c.reason}`,
  }[payload.status];
  if (msg) setStatus(msg, ['Error', 'Invalid', 'Past'].some(w => payload.status.includes(w)) ? 'bad' : '');
});

listen('job-slow', ({ payload }) => {
  const c = clipById(payload.id);
  if (!c || S.cancelled) return;
  c.status = 'Slow';
  syncStatus(c);
  setStatus('Still cutting — long Spaces take a while');
});

listen('job-progress', ({ payload }) => {
  if (S.cancelled) return;
  setProgress(payload.value);
});

listen('job-note', ({ payload }) => { if (!S.cancelled) setStatus(payload.text); });

listen('job-done', ({ payload }) => {
  if (S.cancelled || payload.cancelled) { S.cancelled = false; return; }
  const failed = S.clips.some(c => STATUS[c.status]?.[1] === 'bad');
  setRunning(false);
  const saved = S.clips.filter(c => c.status === 'Done').length;
  const lost = S.clips.filter(c => STATUS[c.status]?.[1] === 'bad').length;
  const done = failed ? `${saved} saved, ${lost} couldn't be cut`
    : `${saved} clip${saved === 1 ? '' : 's'} saved to ${baseName(S.env.output_folder)}`;
  setStatus(failed ? `${done} — hover the red ! to see why` : `✓ ${done}`, failed ? 'bad' : 'ok');
  invoke('notify_done', { body: done }).catch(() => {});
});

/** A clip that changes after a cut loses its tick (or its error). */
function touched(c) {
  if (!c || inJob(c) || c.status === 'Pending') return;
  c.status = 'Pending';
  syncStatus(c);
  if (!S.clips.some(x => x.status !== 'Pending')) setStatus('');
}

$('#cut').addEventListener('click', startCut);
$('#cancel').addEventListener('click', cancelCut);

$('#folder').addEventListener('click', chooseFolder);

/* ── Source field, transport ────────────────────────── */

$('#src').addEventListener('keydown', e => {
  if (e.key === 'Enter') { e.preventDefault(); addLink(e.target.value); }
});
$('#src').addEventListener('paste', () => setTimeout(() => addLink($('#src').value), 0));
$('#open').addEventListener('click', async () => {
  const paths = await invoke('pick_files', { kind: 'media', multiple: true }).catch(() => []);
  let last = null;
  for (const p of paths) last = addSource(p);
  if (last) activate(last.id);
});
ui.play.addEventListener('click', togglePlay);
/** The arrow turns a little each press, as on the iPhone. */
function spin(btn, dir) {
  btn.classList.remove('spin', 'back', 'fwd');
  void btn.offsetWidth;
  btn.classList.add('spin', dir);
}
/** Skips 10 seconds back (-1) or forward (1). Held down it keeps skipping,
 *  but the ring only turns for the first two. */
let skipRun = 0;
function skip(dir, fresh) {
  if (!S.p.loaded) return;
  skipRun = fresh ? 0 : skipRun + 1;
  if (skipRun < 2) spin($(dir < 0 ? '#back' : '#fwd'), dir < 0 ? 'back' : 'fwd');
  seek(currentPos() + 10 * dir);
}
// Holding a skip button repeats it, the way holding an arrow key does
for (const [id, dir] of [['#back', -1], ['#fwd', 1]]) {
  const btn = $(id);
  let timer = null;
  const stop = () => { clearTimeout(timer); timer = null; };
  btn.addEventListener('pointerdown', e => {
    if (e.button !== 0 || btn.disabled) return;
    btn.setPointerCapture(e.pointerId);
    stop();
    skip(dir, true);
    const again = wait => { timer = setTimeout(() => { skip(dir, false); again(110); }, wait); };
    again(400);
  });
  for (const ev of ['pointerup', 'pointercancel', 'lostpointercapture']) btn.addEventListener(ev, stop);
  // Enter on a focused button has no pointer behind it, and skips once
  btn.addEventListener('click', e => { if (e.detail === 0) skip(dir, true); });
}
$('#mark').addEventListener('click', toggleMark);
$('#livePill').addEventListener('click', goLive);

// Playback speed: one button, each click moves to the next speed
const SPEEDS = [1, 1.25, 1.5, 2, 0.5];
$('#speed').addEventListener('click', async e => {
  const p = S.p;
  p.pos = currentPos();
  p.at = performance.now();
  S.speed = SPEEDS[(SPEEDS.indexOf(S.speed) + 1) % SPEEDS.length];
  e.currentTarget.textContent = `${S.speed}×`;
  e.currentTarget.classList.toggle('on', S.speed !== 1);
  if (p.playing) p.buffering = true;
  try { await invoke('player_speed', { speed: S.speed }); }
  catch (err) { setStatus(String(err), 'bad'); }
  pollSoon();
});

/* ── Theme ──────────────────────────────────────────── */

function applyTheme(t) {
  S.env.theme = t;
  document.documentElement.dataset.theme = t;
  $$('#theme button').forEach(b => b.classList.toggle('on', b.dataset.theme === t));
  try { window.__TAURI__.window.getCurrentWindow().setTheme(t === 'auto' ? null : t); } catch { /* older webview */ }
}

$$('#theme button').forEach(b => {
  b.innerHTML = ICON[b.dataset.theme === 'light' ? 'sun' : b.dataset.theme === 'dark' ? 'moon' : 'auto'];
  b.addEventListener('click', () => { applyTheme(b.dataset.theme); savePref('theme', b.dataset.theme); });
});

/* ── Menus, popovers, dialogs ───────────────────────── */

let floating = null;

function closeFloating() {
  if (floating) { floating.remove(); floating = null; }
}

function place(el, x, y) {
  el.style.left = `${Math.max(8, Math.min(x, innerWidth - el.offsetWidth - 8))}px`;
  el.style.top = `${Math.max(8, Math.min(y, innerHeight - el.offsetHeight - 8))}px`;
}

function openMenu(x, y, items) {
  closeFloating();
  const menu = h('div', { class: 'menu' });
  for (const it of items) {
    if (it === '-') { menu.append(h('div', { class: 'ms' })); continue; }
    menu.append(h('button', { class: `mi${it.danger ? ' danger' : ''}`, disabled: !!it.off,
      onclick: () => { closeFloating(); it.run(); } }, it.label));
  }
  document.body.append(menu);
  place(menu, x, y);
  floating = menu;
}

function openPopover(pop, anchor) {
  closeFloating();
  document.body.append(pop);
  const a = anchor.getBoundingClientRect();
  const above = a.top - pop.offsetHeight - 6;
  place(pop, a.left + a.width / 2 - pop.offsetWidth / 2, above > 8 ? above : a.bottom + 6);
  floating = pop;
}

document.addEventListener('pointerdown', e => {
  if (floating && !floating.contains(e.target)) closeFloating();
}, true);
window.addEventListener('blur', closeFloating);
window.addEventListener('resize', closeFloating);

function modal(content, onClose) {
  closeFloating();
  const back = h('div', { class: 'modal-back' }, h('div', { class: 'modal' }, content));
  const onKey = e => {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    close();
  };
  const close = () => {
    back.remove();
    document.removeEventListener('keydown', onKey, true);
    if (onClose) onClose();
  };
  document.addEventListener('keydown', onKey, true);
  back.addEventListener('pointerdown', e => { if (e.target === back) close(); });
  document.body.append(back);
  return close;
}

/** A one-line text prompt; resolves to the text, or null when cancelled. */
function ask(title, label, value = '') {
  return new Promise(resolve => {
    let done = false;
    const input = h('input', { class: 'field', value, spellcheck: 'false', autocomplete: 'off' });
    const finish = v => {
      if (done) return;
      done = true;
      close();
      resolve(v);
    };
    const close = modal(h('div', {},
      h('div', { class: 'm-title' }, title),
      h('div', { class: 'm-label' }, label),
      input,
      h('div', { class: 'm-actions' },
        h('button', { class: 'soft', onclick: () => finish(null) }, 'Cancel'),
        h('button', { class: 'main', onclick: () => finish(input.value) }, 'OK'))),
    () => { if (!done) { done = true; resolve(null); } });
    input.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); finish(input.value); } });
    setTimeout(() => { input.focus(); input.select(); }, 0);
  });
}

const HELP = {
  link: `<b>The Dynamic Link</b>A Space provides its audio through a <strong>Dynamic link</strong> while it's live, but this can be converted into a <strong>Replay link</strong> at any time, for up to 30 days after the Space has ended. Spacecut does this <strong>automatically</strong>: it converts the <strong>Dynamic link</strong> into a <strong>Replay link</strong> and then cuts the audio.

<b>Get It While the Space Is Live</b>1. Open the Space in your browser.
2. Press F12 to open the Developer Tools.
3. Go to the <strong>Network</strong> tab.
4. Type <code>dynamic</code> in the filter box.
5. Find the request containing <code>dynamic_playlist.m3u8?type=live</code>.
6. Right-click it → Copy → Copy URL.
7. Paste it into the field at the top of Spacecut. It will <strong>automatically</strong> be converted into a <strong>Replay link</strong>.

<b>Good to Know</b><strong>Dynamic</strong> and <strong>Replay links</strong> stop working 30 days after the Space has ended. Cut whatever you want to keep before then.
Save the <strong>Dynamic link</strong> in a note while the Space is live, so you can still retrieve the <strong>Replay link</strong> later.
Already have a <strong>Replay link</strong> (<code>…playlist_….m3u8?type=replay</code>)? Paste it directly. There's nothing to convert.`,
  app: `<b>Add a Space</b>Paste a <strong>Dynamic link</strong> or <strong>Replay link</strong> into the field at the top. Each Space gets its own coloured tag. Click the <strong>Copy</strong> icon inside a tag to copy its Replay link, whether you want to listen to the Space in your browser or share it with others. Right-click a tag to rename it, add the entire Space as a single clip, or remove it.

<b>Why Add a File?</b>Many people already have screen recordings of Spaces saved as video files. Spacecut makes it easy to convert them into <code>.m4a</code> audio files. <strong>Merge</strong> lets you combine clips from the same Space, different Spaces, or even older clips you've previously saved, with or without Spacecut, into one continuous audio file.
You can also add, save, and preview your favourite sound effects to use between clips, making your final audio more playful and engaging.

<b>Mark What to Cut</b>Press <strong>Mark Start</strong> and <strong>Mark End</strong> while listening, or simply drag across the bar to select the exact section you want to cut.
You don’t drag the <strong>playhead</strong>, you simply point to the timeline. To select a <strong>section</strong>, drag across the timeline to mark the portion you want to cut. Each selected section becomes a separate clip, displayed in the colour of its Space tag.

Alternatively, type the timestamps directly into the corresponding row. For example, <code>013045</code> automatically becomes <code>01:30:45</code>.`,
};

function openHelp(chapter = 'link') {
  let cur = chapter;
  const text = h('div', { class: 'm-text' });
  const a = h('button', {}, 'Dynamic link');
  const b = h('button', {}, 'How it works');
  const paint = () => {
    text.innerHTML = HELP[cur];
    text.scrollTop = 0;
    a.classList.toggle('on', cur === 'link');
    b.classList.toggle('on', cur === 'app');
  };
  a.addEventListener('click', () => { cur = 'link'; paint(); });
  b.addEventListener('click', () => { cur = 'app'; paint(); });
  const close = modal(h('div', {},
    h('div', { class: 'm-head' }, h('div', { class: 'mode chapters' }, a, b)),
    text,
    h('div', { class: 'm-actions' }, h('button', { class: 'main', onclick: () => close() }, 'OK'))));
  paint();
}
$('#help').addEventListener('click', () => openHelp('link'));

// Only text fields keep the system menu (cut, copy, paste)
document.addEventListener('contextmenu', e => {
  if (!e.target.closest('input')) e.preventDefault();
});

/* ── Drag & drop ────────────────────────────────────── */

const dropBox = $('#drop');
listen('tauri://drag-enter', () => { if (!S.running) dropBox.classList.remove('hidden'); });
listen('tauri://drag-leave', () => dropBox.classList.add('hidden'));
listen('tauri://drag-drop', ({ payload }) => {
  dropBox.classList.add('hidden');
  if (S.running || !payload) return;
  const paths = (payload.paths || []).filter(p => MEDIA_EXTS.includes(extOf(p)));
  let last = null;
  for (const p of paths) last = addSource(p);
  if (last) activate(last.id);
});

/* ── Keyboard ───────────────────────────────────────── */

document.addEventListener('keydown', e => {
  if ($('.modal-back')) return;
  const t = e.target;
  const typing = t.tagName === 'TEXTAREA' || (t.tagName === 'INPUT' && !['checkbox', 'range'].includes(t.type));
  if (e.key === 'Escape') {
    closeFloating();
    if (typing) t.blur();
    else if (S.pending) { S.pending = null; render(); }
    return;
  }
  if (typing || e.metaKey || e.ctrlKey || e.altKey) return;
  const key = e.key.toLowerCase();
  if (key === ' ') { e.preventDefault(); togglePlay(); }
  else if (key === 'arrowleft') { e.preventDefault(); skip(-1, !e.repeat); }
  else if (key === 'arrowright') { e.preventDefault(); skip(1, !e.repeat); }
  else if (key === '[' || key === 'i') { e.preventDefault(); if (!pendingClip()) markStart(); }
  else if (key === ']' || key === 'o') { e.preventDefault(); markEnd(); }
  else if (key === 'm') { e.preventDefault(); toggleMark(); }
}, true);

/* ── Mini player ────────────────────────────────────── */

/** The window as a small player (tags, the bar and Mark), or back as it was. */
async function toggleMini() {
  const on = !document.body.classList.contains('mini');
  document.body.classList.toggle('mini', on);
  // As tall as what is left to show: down to the bottom of the player
  const main = $('main');
  main.scrollTop = 0;
  const height = on ? Math.ceil($('section.player').getBoundingClientRect().bottom + parseFloat(getComputedStyle(main).paddingBottom)) : 0;
  try { await invoke('set_mini', { on, height }); } catch { /* the window stays as it is */ }
  S.shownPx = -1;
  kick();
}
// The logo's dots switch it on and off; the button on the right only brings it back
$('.dots.logo').addEventListener('click', toggleMini);
$('#fullBtn').innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 4h6v6M10 20H4v-6M20 4l-6.5 6.5M4 20l6.5-6.5"/></svg>';
$('#fullBtn').addEventListener('click', toggleMini);

/* ── The Space's mark ───────────────────────────────── */

/** A dot at first; a click on the one by the time turns it into three bars of
 *  sound, for every Space, and back. Kept for the next time. */
function showMark(wave) { document.documentElement.classList.toggle('waves', wave); }
showMark((() => { try { return localStorage.getItem('mark') === 'wave'; } catch { return false; } })());
$('#liveDot').addEventListener('click', () => {
  const wave = !document.documentElement.classList.contains('waves');
  showMark(wave);
  try { localStorage.setItem('mark', wave ? 'wave' : 'dot'); } catch { /* kept for this time only */ }
});

/* ── Spaces on air ──────────────────────────────────── */

/** Once a minute the Spaces on air not in the player are asked, by their
 *  small live lists, whether they still are (the player asks for its own). */
async function checkLive() {
  const others = S.sources.filter(x => x.isLive && x.url !== S.player.url);
  for (const x of others) {
    const on = await invoke('space_live', { url: x.url, live: x.live || null }).catch(() => null);
    if (on !== false) continue;
    x.isLive = false;
    if (S.p.url === x.url) { S.p.live = false; renderPlayer(); }
  }
}
setInterval(checkLive, 60000);

/* ── Start ──────────────────────────────────────────── */

async function init() {
  try { Object.assign(S.env, await invoke('env_info')); } catch { /* defaults */ }
  document.documentElement.classList.add(S.env.platform);
  applyTheme(S.env.theme || 'auto');
  $('#logoScissors').innerHTML = ICON.scissors;
  $('#back').innerHTML = ICON.back10;
  $('#fwd').innerHTML = ICON.fwd10;
  $('#cancel').innerHTML = ICON.close;
  renderFolder();
  render();
  if (!S.env.ffmpeg) setStatus('FFmpeg not found', 'bad');
  kick();
  pollLoop();
  setTimeout(checkUpdate, 3000);
}

/** When a newer version is out: a tag at the top, its dot in a colour of its
 *  own for each version. Nothing otherwise. */
async function checkUpdate() {
  const u = await invoke('check_update').catch(() => null);
  if (!u) return;
  const box = $('#update');
  const name = u.version.replace(/^v/i, '').replace(/\.0$/, '');
  const [major = 0, minor = 0, patch = 0] = name.split('.').map(Number);
  box.style.setProperty('--uc', COLORS[(major * 3 + minor + patch) % COLORS.length][0]);
  $('.upd-open', box).textContent = `Update ${name}`;
  $('.upd-open', box).title = `Spacecut ${name} is out: open its download page`;
  $('.upd-open', box).onclick = () => invoke('open_update', { url: u.url }).catch(() => {});
  $('.upd-close', box).innerHTML = ICON.close;
  $('.upd-close', box).onclick = () => { box.classList.add('hidden'); invoke('dismiss_update', { version: u.version }); };
  box.classList.remove('hidden');
}

init();
