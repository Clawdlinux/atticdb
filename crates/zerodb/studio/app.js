'use strict';
// zerodb studio: a read-only page over the studio bridge on 127.0.0.1.
// Every store value (content, keys, agent and session names) is untrusted text. It reaches the page only
// through text nodes and attribute values: never as markup, never in a class, tag or attribute name, and
// in a URL only as a query parameter built with URLSearchParams.

const SECRET_KEY = 'zerodb-studio-secret';
const EIGHT_MIB = 8388608;
const PAGE = 50;
const SKIP_EMPTY = 20; // a page can come back empty at the visit cap; ask again this many times in a row

// ---------- the secret: from the link's fragment into sessionStorage, then out of the address bar ----------
function takeSecret() {
  if (!location.hash.startsWith('#k=')) return;
  const m = /^#k=([0-9A-Za-z]{1,128})$/.exec(location.hash);
  if (m) {
    try { sessionStorage.setItem(SECRET_KEY, m[1]); } catch (e) { /* no storage: calls go without it and get a 401 */ }
  }
  history.replaceState(null, '', location.pathname + location.search);
}
takeSecret();

class ApiError extends Error {
  constructor(status, code, detail) { super(code); this.status = status; this.code = code; this.detail = detail || ''; }
}

// GET /api/<route>, with the secret as X-Zerodb-Studio. A 401 forgets the secret. An empty string is a value: the
// store takes empty agent and session names, and a cursor past one must keep both halves.
async function api(route, params) {
  const query = new URLSearchParams();
  for (const [k, v] of Object.entries(params || {})) if (v !== undefined && v !== null) query.set(k, String(v));
  const headers = {};
  let secret = null;
  try { secret = sessionStorage.getItem(SECRET_KEY); } catch (e) { secret = null; }
  if (secret) headers['X-Zerodb-Studio'] = secret;
  const qs = query.toString();
  let res;
  let text;
  try {
    res = await fetch('/api/' + route + (qs ? '?' + qs : ''), { headers, cache: 'no-store', credentials: 'omit', redirect: 'error', referrerPolicy: 'no-referrer' });
    // a body cut off in transfer fails here too
    text = await res.text();
  } catch (e) {
    throw new ApiError(0, 'offline');
  }
  if (res.status === 401) {
    try { sessionStorage.removeItem(SECRET_KEY); } catch (e) { /* nothing to clear */ }
    throw new ApiError(401, 'unauthorized');
  }
  if (!res.ok) {
    let body = null;
    try { body = JSON.parse(text); } catch (e) { body = null; }
    const code = body && typeof body.error === 'string' ? body.error : 'http';
    throw new ApiError(res.status, code, body && typeof body.message === 'string' ? body.message : '');
  }
  if (route === 'metrics') return text;
  try { return JSON.parse(text); } catch (e) { throw new ApiError(res.status, 'bad_json'); }
}

// ---------- DOM: elements from code, text from data ----------
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'on') for (const [type, fn] of Object.entries(v)) el.addEventListener(type, fn);
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  return add(el, kids);
}
function put(el, ...kids) { el.replaceChildren(); return add(el, kids); }
function add(el, kids) {
  for (const kid of [kids].flat(Infinity)) if (kid !== undefined && kid !== null && kid !== false) el.append(kid);
  return el;
}
const SVG = 'http://www.w3.org/2000/svg';
function svg(tag, attrs, ...kids) {
  const el = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs || {})) el.setAttribute(k, String(v));
  return add(el, kids);
}
const code = (t) => h('code', null, t);
const fig = (t, cls) => h('span', { class: cls ? 'fig ' + cls : 'fig' }, t);
// An agent or session name. The store takes an empty one, which shows as a muted placeholder, never as nothing.
const nameOf = (t) => (t === '' ? h('span', { class: 'muted' }, '(empty)') : t);
const byId = (id) => document.getElementById(id);
// A turn can hold 4 MiB with no space, and the browser takes over 30 s to wrap 2 MiB of that. A row shows 2 lines,
// so it gets the first 1,000 characters. A full view of text past 10,000 characters gets a break point every 32
// characters of a run longer than 64, at a grapheme boundary. Shorter text renders as written.
const preview = (t) => (t.length > 1000 ? t.slice(0, 1000) : t);
const graphemes = Intl.Segmenter ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;
// The end of a chunk of run from i, a grapheme boundary: the first boundary at or past i + 32, so a letter keeps its
// marks and an emoji sequence stays whole. Below U+0300 on both sides there always is one, and the segmenter only
// reads a window from i, never the whole run.
function chunkEnd(run, i) {
  const j = i + 32;
  if (j >= run.length) return run.length;
  if (run.charCodeAt(j - 1) < 0x300 && run.charCodeAt(j) < 0x300) return j;
  if (!graphemes) return /[\uDC00-\uDFFF]/.test(run[j]) ? j + 1 : j;
  for (let size = 64; ; size *= 2) {
    const win = run.slice(i, i + size);
    const s = graphemes.segment(win).containing(32);
    const end = s.index === 32 ? 32 : s.index + s.segment.length;
    // a grapheme that reaches the window's end may go on past it
    if (end < win.length || i + size >= run.length) return i + end;
  }
}
function breakable(t) {
  if (t.length <= 10000) return t;
  const out = [];
  let from = 0;
  for (const m of t.matchAll(/\S{65,}/g)) {
    if (m.index > from) out.push(t.slice(from, m.index));
    const run = m[0];
    for (let i = 0; i < run.length;) {
      const j = chunkEnd(run, i);
      if (i) out.push(h('wbr'));
      out.push(run.slice(i, j));
      i = j;
    }
    from = m.index + run.length;
  }
  if (from < t.length) out.push(t.slice(from));
  return out;
}

// Hash routes: #/<screen>?<params>. The screen name comes from a fixed table; store values ride in params.
function href(screen, params) {
  const qs = params ? new URLSearchParams(params).toString() : '';
  return '#/' + screen + (qs ? '?' + qs : '');
}

// ---------- figures and times ----------
const NUM = new Intl.NumberFormat('en-US');
const n = (x) => (Number.isFinite(x) ? NUM.format(x) : '-');
const count = (x, one, many) => `${n(x)} ${x === 1 ? one : many || one + 's'}`;
function size(b) {
  if (b < 1024) return `${n(b)} B`;
  if (b < 1048576) return `${(b / 1024).toFixed(1)} KiB`;
  return `${(b / 1048576).toFixed(1)} MiB`;
}
const ms3 = (ns) => (Number.isFinite(ns) ? (ns / 1e6).toFixed(3) : '-');
const TIME = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const DAY = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const DAY_Y = new Intl.DateTimeFormat(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
const FULL = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'medium' });
// commit_time_ns is past 2^53, so JSON gives a rounded number: fine for a local time, never sent back
const dateOf = (ns) => new Date(Number(ns) / 1e6);
function daysAgo(d) {
  const a = new Date(); a.setHours(0, 0, 0, 0);
  const b = new Date(d.getTime()); b.setHours(0, 0, 0, 0);
  return Math.round((a - b) / 864e5);
}
function dayName(d) {
  const k = daysAgo(d);
  if (k === 0) return 'Today';
  if (k === 1) return 'Yesterday';
  return (d.getFullYear() === new Date().getFullYear() ? DAY : DAY_Y).format(d);
}
function when(ns) {
  const d = dateOf(ns);
  return (daysAgo(d) === 0 ? '' : dayName(d) + ' ') + TIME.format(d);
}
function whenWords(ns) {
  const d = dateOf(ns);
  const k = daysAgo(d);
  return (k === 0 ? 'today' : k === 1 ? 'yesterday' : 'on ' + dayName(d)) + ' at ' + TIME.format(d);
}
function ago(ns) {
  const min = Math.max(0, Math.round((Date.now() - Number(ns) / 1e6) / 6e4));
  if (min < 1) return 'just now';
  if (min < 60) return `${min} min ago`;
  const hr = Math.round(min / 60);
  return hr < 48 ? `${hr} h ago` : `${Math.round(hr / 24)} days ago`;
}
const timeCell = (ns) => h('span', { class: 'time', title: FULL.format(dateOf(ns)) }, when(ns));

// ---------- block states: loading, empty, error ----------
function busy(el, rows) {
  el.setAttribute('aria-busy', 'true');
  put(el, ...Array.from({ length: rows }, () => h('div', { class: 'sk-row', 'aria-hidden': 'true' }, h('span', { class: 'sk sk-lsn' }),
    h('div', null, h('span', { class: 'sk' }), h('span', { class: 'sk line short' })), h('span', { class: 'sk sk-agent' }), h('span', { class: 'sk' }))));
}
function done(el) { el.removeAttribute('aria-busy'); return el; }
function empty(el, ...lines) {
  put(done(el), h('div', { class: 'state' }, lines.map((line) => (line instanceof Element ? line : h('p', null, line)))));
}
function errorText(err) {
  if (err.status === 401) return ['This link is out of date. Open the link ', code('zerodb studio'), ' printed.'];
  if (err.code === 'store_unreachable') return [`Couldn't reach the store${err.detail ? ': ' + err.detail : ''}. `, storeHint('unreachable')];
  if (err.code === 'store_unauthorized') return ["The store refused the studio's token. Run ", code('zerodb studio'), ' again with a token it takes.'];
  if (err.code === 'store_timeout') return ['The store took more than 10 s to answer. ', storeHint('timeout')];
  if (err.code === 'offline') return ['The studio stopped answering. Run ', code('zerodb studio'), ' again and open the link it prints.'];
  if (err.code === 'invalid_input') return [`The store refused the request${err.detail ? ': ' + err.detail : ''}.`];
  return [`The studio answered ${err.status || 'nothing'}${err.code !== 'http' ? ' ' + err.code : ''}${err.detail ? ': ' + err.detail : ''}.`];
}
// What to do about a store the studio can't reach in time. A home studio starts the home server itself, so that
// server's log tells why it is down. Any other store runs by hand. Filled in once /api/info has answered.
function storeHint(kind) {
  const hint = h('span');
  infoReady.then((info) => {
    const log = info && info.store === 'home' && info.home ? code(`${info.home}/data/serve.log`) : null;
    const serve = code('zerodb serve --config <path> --data-dir <dir>');
    if (kind === 'unreachable') put(hint, ...(log ? ['The studio starts the home server itself. Its log may say more: ', log, '. Then retry.'] : ['Start it with ', serve, ', then retry.']));
    else if (kind === 'timeout') put(hint, ...(log ? ["The home server's log may say why: ", log, '. Then retry.'] : ['Check the server you started with ', serve, ', then retry.']));
    else put(hint, ...(log ? ['See ', log, '.'] : ['Start it with ', serve, '.']));
  });
  return hint;
}
const statsRefused = (err) => err instanceof ApiError && (err.status === 403 || err.status === 404);
const STATS_REFUSED = 'This store does not share its stats over its URL. A home or data dir studio shows them.';
function errorShort(err) {
  if (err.status === 401) return errorText(err);
  if (err.code === 'store_unreachable') return ["Couldn't reach the store. ", storeHint('short')];
  if (err.code === 'store_unauthorized') return ["The store refused the studio's token."];
  if (err.code === 'store_timeout') return ['The store is not answering in time.'];
  if (err.code === 'offline') return ['The studio stopped answering.'];
  return ["Couldn't load this screen."];
}
function failed(el, err) {
  if (!(err instanceof ApiError)) throw err;
  put(done(el), h('div', { class: 'state error', role: 'alert' }, h('p', null, errorText(err)),
    err.status === 401 ? null : h('button', { class: 'text-btn', type: 'button', on: { click: redraw } }, 'Retry')));
}
const nothingSaved = () => ['Nothing saved yet. Connect an agent with ', code('zerodb connect vscode'), ' or ', code('zerodb connect copilot'), ', then ask it to remember something.'];

// ---------- the shell: scope, nav, phone menu ----------
const STORE_NAME = { home: 'Home', data_dir: 'Data dir', url: 'URL' };
const infoReady = api('info').then((info) => {
  // a studio on --url with a token file does not know its tenant's name
  byId('scope-tenant').textContent = info.tenant || '-';
  byId('scope-store').textContent = Object.hasOwn(STORE_NAME, info.store) ? STORE_NAME[info.store] : info.store;
  // a line may break before any slash of the path
  if (info.home) put(byId('scope-path'), info.home.split('/').map((part, i) => (i ? [h('wbr'), '/' + part] : part)));
  byId('version').textContent = `zerodb ${info.version}. Read only.`;
  return info;
}, () => null);

const app = byId('app');
const menuOpen = byId('menu-open');
function setMenu(open) {
  if (app.classList.contains('menu-open') === open) return;
  app.classList.toggle('menu-open', open);
  document.documentElement.classList.toggle('lock', open);
  menuOpen.setAttribute('aria-expanded', String(open));
  // the open menu covers the page, so focus stays in it
  for (const el of [byId('main'), document.querySelector('.topbar')]) el.inert = open;
  if (open) byId('menu-close').focus(); else menuOpen.focus({ preventScroll: true });
}
menuOpen.addEventListener('click', () => setMenu(true));
byId('menu-close').addEventListener('click', () => setMenu(false));
addEventListener('keydown', (e) => { if (e.key === 'Escape') setMenu(false); });
for (const a of document.querySelectorAll('.nav-item')) a.addEventListener('click', () => setMenu(false));
matchMedia('(min-width: 961px)').addEventListener('change', (e) => { if (e.matches) setMenu(false); });

// `title` is text, or the element nameOf gives for an empty name.
function titleRow(main, title, crumbs) {
  const text = typeof title === 'string' ? title : title.textContent;
  const finding = h('p', { class: 'finding quiet' }, h('span', { class: 'sk line', 'aria-hidden': 'true' }));
  const heading = h('h1', { tabindex: '-1' }, title);
  main.append(h('header', { class: 'title-row' },
    h('div', { class: 'title-main' },
      crumbs ? h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, crumbs.map((c, i) => [i ? h('span', { 'aria-hidden': 'true' }, '/') : null, c])) : null,
      heading, finding),
    h('div', { class: 'title-side' },
      h('span', { class: 'stamp' }, 'Loaded ' + TIME.format(new Date())),
      h('button', { class: 'btn', type: 'button', on: { click: redraw } }, 'Refresh'))));
  document.title = `${text} · zerodb studio`;
  byId('topbar-screen').textContent = text;
  return finding;
}
function say(finding, ...parts) { finding.classList.remove('quiet'); put(finding, ...parts.flat(Infinity)); }
function note(finding, ...parts) { finding.classList.add('quiet'); put(finding, ...parts.flat(Infinity)); }

function card(title, aside, cls) {
  const heading = h('h2', null, title);
  const head = h('header', { class: 'card-head' }, heading, aside || null);
  const body = h('div', { class: 'card-body' });
  return { el: h('section', { class: cls ? 'card ' + cls : 'card' }, head, body), head, heading, body };
}

// ---------- records: a turn or a fact revision as one row ----------
// The store's plural rule, Harman's S-stemmer (singular in zerodb-core's engine.rs): keys gives key,
// policies gives policy, and status, class, does and goes stay as they are. Lengths count UTF-8 bytes.
function singular(w) {
  const n = new TextEncoder().encode(w).length;
  if (n < 3 || !w.endsWith('s')) return null;
  const at = (i) => w[w.length - i];
  if (at(2) === 'u' || at(2) === 's') return null;
  if (at(2) === 'e' && n > 3 && at(3) === 'i' && at(4) !== 'a' && at(4) !== 'e') return w.slice(0, -3) + 'y';
  if (at(2) === 'e' && 'iaoe'.includes(at(3))) return null;
  return w.slice(0, -1);
}

// Matched words get <mark> elements around text nodes. Every word matches as a substring, any case;
// a plural also matches its singular, as the store's search does.
function highlight(text, words) {
  const lower = text.toLowerCase();
  if (!words || !words.length || lower.length !== text.length) return text;
  const terms = new Set();
  for (const w of words) {
    terms.add(w);
    const s = singular(w);
    if (s) terms.add(s);
  }
  const spans = [];
  for (const t of terms) for (let i = lower.indexOf(t); i !== -1; i = lower.indexOf(t, i + t.length)) spans.push([i, i + t.length]);
  spans.sort((a, b) => a[0] - b[0]);
  const out = [];
  let pos = 0;
  for (const [a, b] of spans) {
    if (b <= pos) continue;
    const from = Math.max(a, pos);
    if (from > pos) out.push(text.slice(pos, from));
    out.push(h('mark', null, text.slice(from, b)));
    pos = b;
  }
  if (pos < text.length) out.push(text.slice(pos));
  return out;
}

function recordRow(rec, words) {
  const isFact = rec.kind === 'fact';
  const mark = (t) => (words ? highlight(t, words) : t);
  const text = isFact && rec.deleted === true
    ? h('p', { class: 'rec-text is-deleted' }, 'Value deleted.')
    : h('p', { class: 'rec-text' }, mark(preview(rec.content)));
  const where = isFact
    ? ['Fact ', h('a', { href: href('fact', { key: rec.key }) }, code(mark(rec.key)))]
    : ['Turn ', fig(String(rec.seq)), ' in ', h('a', { href: href('session', { agent: rec.agent, session: rec.session, seq: rec.seq }) }, nameOf(rec.session)),
        rec.role === 'assistant' ? null : [' ', h('span', { class: 'pill' }, rec.role)]];
  return h('li', { class: 'rec' },
    h('span', { class: 'rec-lsn' }, String(rec.lsn)),
    text,
    h('p', { class: 'rec-meta' }, where, h('span', { class: 'also' }, ' by ', h('span', { class: 'who' }, nameOf(rec.agent)))),
    h('span', { class: 'rec-agent' }, nameOf(rec.agent)),
    h('span', { class: 'rec-when' }, timeCell(rec.commit_time_ns), h('span', { class: 'lsn' }, 'LSN ' + rec.lsn)));
}

function listHead() {
  return h('div', { class: 'list-head', 'aria-hidden': 'true' }, h('span', null, 'LSN'), h('span', null, 'Record'), h('span', null, 'Agent'), h('span', null, 'Time'));
}

// The foot of a list that pages back: a status line and a button, built once so the button keeps its focus. While
// a page loads the button is aria-disabled, as disabling it would drop the focus. An error takes the status line's
// place and leaves the button for a retry. Once nothing older remains the button goes and the status takes focus.
function pagerFoot(foot, label, load, end) {
  const status = h('span', { class: 'muted', tabindex: '-1', 'aria-live': 'polite' });
  const button = h('button', { class: 'btn', type: 'button' }, label);
  button.addEventListener('click', async () => {
    if (button.getAttribute('aria-disabled') === 'true') return;
    button.setAttribute('aria-disabled', 'true');
    try { await load(); } finally { button.removeAttribute('aria-disabled'); }
  });
  put(foot, status, button);
  return {
    show(text, more) {
      status.className = 'muted';
      status.textContent = text;
      if (more || !button.isConnected) return;
      const focused = document.activeElement === button;
      button.replaceWith(...(end ? [h('span', { class: 'muted' }, end)] : []));
      if (focused) status.focus({ preventScroll: true });
    },
    error(err) {
      if (!(err instanceof ApiError)) throw err;
      status.className = '';
      put(status, errorText(err));
    },
  };
}

// Newest-first paging with an exclusive cursor. An empty page with a cursor means the store hit its
// visit cap, so ask again from that cursor before showing anything.
function pager({ list, foot, get, row, noun, onFirst, emptyLines }) {
  const id = renderId;
  let cursor = null;
  let shown = 0;
  let total = null;
  let started = false;
  let ui = null;
  const paint = () => {
    ui = ui || pagerFoot(foot, 'Older', next, 'Nothing older.');
    ui.show(total !== null && total >= shown ? `Showing ${n(shown)} of ${count(total, noun)}.` : `Showing ${count(shown, noun)}.`, cursor !== null);
  };
  async function next() {
    try {
      let page;
      let tries = 0;
      do {
        page = await get(cursor);
        if (id !== renderId) return;
        cursor = page.next;
        tries++;
      } while (!page.items.length && cursor !== null && tries < SKIP_EMPTY);
      const first = !started;
      if (first) { started = true; put(done(list)); }
      list.append(...page.items.map(row));
      shown += page.items.length;
      if (first && onFirst) onFirst(shown, cursor);
      if (!shown && cursor === null) { empty(list, ...emptyLines); foot.hidden = true; return; }
      paint();
    } catch (err) {
      if (id !== renderId) return;
      if (!started) { failed(list, err); foot.hidden = true; } else if (ui) ui.error(err); else throw err;
    }
  }
  busy(list, 6);
  next();
  return { setTotal(t) { total = t; if (started && !foot.hidden) paint(); } };
}

// ---------- repos, as the chat-start hook names them ----------
// A turn's repo is its session name without an @branch suffix; a fact's is the <repo> in repo/<repo>/.
function repoOf(rec) {
  if (rec.kind === 'turn') return rec.session.split('@')[0] || null;
  const m = /^repo\/([^/]+)\//.exec(rec.key);
  return m ? m[1] : null;
}
function newestRepo(records) {
  for (const r of records) { const repo = repoOf(r); if (repo) return repo; }
  return null;
}
// The newest records, or null when SKIP_EMPTY pages in a row came back empty with a cursor. A records read visits at
// most 1,000,000 LSNs, other tenants' records included, so an empty page with a cursor does not mean an empty store.
async function newestRecords(limit) {
  const id = renderId;
  let before;
  for (let i = 0; i < SKIP_EMPTY && id === renderId; i++) {
    const page = await api('v1/records', { before, limit, max_bytes: EIGHT_MIB });
    if (page.records.length || page.next === null) return page.records;
    before = page.next;
  }
  return null;
}
function groupOf(key) {
  if (key.startsWith('user/')) return 'user/';
  const m = /^repo\/[^/]+\//.exec(key);
  if (m) return m[0];
  const i = key.indexOf('/');
  return i > 0 ? key.slice(0, i + 1) : '';
}
// Reads a list page by page, at most 50 pages at a time: about 50 MiB of facts, or 50,000 sessions. Returns the
// items and the cursor to go on from, which is null once the list is complete. A caller left with a cursor says
// the list is incomplete and offers the rest. Once the user leaves the screen the reads stop, and the caller,
// which checks the render too, drops what came back.
const BATCH = 50;
async function allFacts(after) {
  const id = renderId;
  const facts = [];
  for (let i = 0; i < BATCH && id === renderId; i++) {
    const page = await api('v1/facts/list', { after, limit: 1000 });
    facts.push(...page.facts);
    if (page.next === null) return { facts, next: null };
    after = page.next;
  }
  return { facts, next: after };
}
async function allSessions(after) {
  const id = renderId;
  const rows = [];
  let total = { sessions: 0, turns: 0 };
  for (let i = 0; i < BATCH && id === renderId; i++) {
    const page = await api('v1/sessions', after ? { limit: 1000, after_agent: after.agent, after_session: after.session } : { limit: 1000 });
    rows.push(...page.sessions);
    total = page.total;
    if (page.next === null) return { rows, total, next: null };
    after = page.next;
  }
  return { rows, total, next: after };
}
// A cursor past the keys under `prefix`: keys sort by their bytes, and U+10FFFF is the largest character. Null
// when the cursor would pass the store's 1024 bytes, or would not move past `from`, so the caller reads on from
// `from`. A key with U+10FFFF right after the prefix sorts past the skip, and a skip must never go backward.
function pastGroup(prefix, from) {
  const enc = new TextEncoder();
  const after = prefix + '\u{10FFFF}';
  const bytes = enc.encode(after);
  return bytes.length <= 1024 && byteOrder(bytes, enc.encode(from)) > 0 ? after : null;
}
// The store's key order: byte by byte in UTF-8. JS string order is UTF-16 and differs past U+FFFF.
function byteOrder(a, b) {
  for (let i = 0; i < Math.min(a.length, b.length); i++) if (a[i] !== b[i]) return a[i] - b[i];
  return a.length - b.length;
}
// The repos with a fact under repo/<repo>/, in at most 50 pages like the lists above. A page that ends inside a
// repo goes on past it, so a repo's facts, however many or large, cost no more reads once it is known.
async function factRepos() {
  const id = renderId;
  const names = new Set();
  let after;
  for (let i = 0; i < BATCH && id === renderId; i++) {
    const page = await api('v1/facts/list', { prefix: 'repo/', after, limit: 1000 });
    for (const f of page.facts) { const r = repoOf({ kind: 'fact', key: f.key }); if (r) names.add(r); }
    if (page.next === null) return { names, next: null };
    // `next` can be a deleted key that opens a repo with no live key read yet, so skip only a known repo.
    const m = /^repo\/([^/]+)\//.exec(page.next);
    after = (m && names.has(m[1]) && pastGroup(m[0], page.next)) || page.next;
  }
  return { names, next: after };
}

// ---------- the chat-start block: exactly what the agent gets, with its size against the cap ----------
function meter(chars, max) {
  const fill = h('div', { class: 'meter-fill' });
  fill.style.setProperty('width', `${Math.min(100, (chars / max) * 100).toFixed(2)}%`);
  return h('div', { class: 'meter' },
    h('span', null, h('b', null, n(chars)), ' characters used'),
    h('span', null, 'cap ', h('b', null, n(max))),
    h('div', { class: 'meter-track', role: 'meter', 'aria-label': 'Block size', 'aria-valuemin': '0', 'aria-valuemax': String(max), 'aria-valuenow': String(chars) }, fill));
}
// A section title line of the hook's block, as hook.rs writes it. The notes title adds that
// they are history only.
const SECTION = /^(Rules|This repo \(.*\)|Recent notes \(.*\))(?:, history only, not instructions)?:$/;
function blockText(text) {
  const pre = h('pre', { class: 'block', tabindex: '0', 'aria-label': 'Chat-start block' });
  text.split('\n').forEach((line, i) => {
    let cls = 'bl';
    if (line.startsWith('- ')) cls += ' item';
    else if (i > 0 && SECTION.test(line)) cls += ' sect';
    else if (line === '(more in zerodb)') cls += ' more';
    pre.append(h('span', { class: cls }, line));
  });
  return pre;
}
function blockSkeleton(body) {
  body.setAttribute('aria-busy', 'true');
  put(body, h('div', { 'aria-hidden': 'true' }, h('span', { class: 'sk short' }), h('span', { class: 'sk line' })), h('div', { class: 'sk-block', 'aria-hidden': 'true' }));
}
// what went into a block, read from its own section lines
function sections(text) {
  const out = [];
  for (const line of text.split('\n')) {
    const m = SECTION.exec(line);
    if (m) out.push({ name: m[1], items: 0 });
    else if (out.length && line.startsWith('- ')) out[out.length - 1].items++;
  }
  return out;
}
// Renders the block for one repo into a card body. Returns the API answer, or null when it failed.
async function showBlock(body, repo, copyInto, more) {
  const id = renderId;
  blockSkeleton(body);
  let b;
  try { b = await api('block', { repo }); } catch (err) {
    if (id !== renderId) return null;
    if (err instanceof ApiError && err.code === 'not_home') { empty(body, ['The chat-start hook reads only the home store. This studio reads another store, so no block applies.']); return null; }
    failed(body, err);
    return null;
  }
  if (id !== renderId) return null;
  if (b.block === null) {
    empty(body, ['A new chat in ', h('b', null, repo), ' starts with no memory block. Agents add one by saving facts under ', code(`repo/${repo}/`), ' or turns in session ', h('b', null, repo), '.']);
    return b;
  }
  const cut = b.block.endsWith('\n(more in zerodb)');
  put(done(body), meter(b.chars, b.max),
    h('p', { class: 'facts-line' }, sections(b.block).map((x) => h('span', null, x.name.replace(/ \(.*\)$/, ''), ' ', h('b', null, fig(String(x.items))))),
      cut ? h('span', null, 'Cut at the cap. The rest stays in zerodb.') : null),
    blockText(b.block),
    more ? h('p', { class: 'block-foot' }, h('a', { class: 'link', href: more }, 'Read it in full')) : null);
  if (copyInto && navigator.clipboard) {
    const btn = h('button', { class: 'text-btn', type: 'button' }, 'Copy block');
    btn.addEventListener('click', () => navigator.clipboard.writeText(b.block).then(() => { btn.textContent = 'Copied'; }, () => { btn.textContent = 'Copy failed'; }));
    put(copyInto, btn);
  }
  return b;
}

// ---------- screens ----------
// 1. Overview: what did my agents save lately?
async function overview(main) {
  const finding = titleRow(main, 'Overview');
  const recent = card('Saved lately', h('a', { class: 'link', href: href('log') }, 'Open the log'), 'span-7');
  const actions = h('span', { class: 'card-actions' });
  const chat = card('Next chat', actions, 'span-5 chat-card ov-chat');
  const list = h('ol', { class: 'recs' });
  recent.body.append(list);
  const line = h('section', { class: 'store-line', 'aria-label': 'Store' });
  main.append(h('div', { class: 'grid' }, recent.el, chat.el), line);
  storeLine(line);
  busy(list, 8);
  blockSkeleton(chat.body);
  const id = renderId;
  let records;
  try { records = await newestRecords(8); } catch (err) {
    if (id !== renderId) return;
    failed(list, err); failed(chat.body, err); say(finding, errorShort(err));
    return;
  }
  if (id !== renderId) return;
  if (records === null) {
    empty(list, ['No recent records found yet. ', h('a', { class: 'link', href: href('log') }, 'Open the log'), ' to look further back.']);
    empty(chat.body, ['No recent record names a repo. Pick one on ', h('a', { class: 'link', href: href('chat-start') }, 'Chat start'), '.']);
    note(finding, 'No recent records found yet.');
    return;
  }
  if (!records.length) {
    empty(list, ['Nothing saved yet. To give an agent this memory:'], h('ol', { class: 'steps' },
      h('li', null, 'Run ', code('zerodb connect vscode'), ' or ', code('zerodb connect copilot'), ' in a terminal.'),
      h('li', null, 'Start a chat. The agent saves notes with ', code('memory_append'), ' and rules with ', code('fact_set'), '.'),
      h('li', null, 'Press Refresh here to see them.')));
    empty(chat.body, ['No block yet. A new chat in a repo will start with:'], h('ul', { class: 'steps plain' },
      h('li', null, 'Rules: every fact under ', code('user/'), '.'),
      h('li', null, 'This repo: every fact under ', code('repo/<repo>/'), '.'),
      h('li', null, 'Recent notes: the 5 newest turns in session ', code('<repo>'), '.')));
    say(finding, 'Nothing saved yet. Connect an agent with ', code('zerodb connect vscode'), '.');
    return;
  }
  put(done(list), ...records.map((r) => recordRow(r)));
  const r = records[0];
  const what = r.kind === 'fact'
    ? [nameOf(r.agent), r.deleted ? ' deleted ' : ' set ', h('a', { href: href('fact', { key: r.key }) }, code(r.key))]
    : [nameOf(r.agent), ' saved turn ', String(r.seq), ' in ', h('a', { href: href('session', { agent: r.agent, session: r.session, seq: r.seq }) }, nameOf(r.session))];
  if (daysAgo(dateOf(r.commit_time_ns)) === 0) say(finding, `Newest write ${ago(r.commit_time_ns)}, at ${TIME.format(dateOf(r.commit_time_ns))}: `, what, '.');
  else say(finding, 'Nothing saved today. The newest write was ', whenWords(r.commit_time_ns), ': ', what, '.');

  const repo = newestRepo(records);
  if (!repo) { empty(chat.body, ['The newest records name no repo. Pick one on ', h('a', { class: 'link', href: href('chat-start') }, 'Chat start'), '.']); return; }
  put(chat.heading, 'Next chat in ', repo);
  const info = await infoReady;
  if (id !== renderId) return;
  if (info && info.store !== 'home') { empty(chat.body, ['The chat-start hook reads only the home store. This studio reads another store.']); return; }
  const copy = h('span');
  put(actions, copy);
  await showBlock(chat.body, repo, copy, href('chat-start', { repo }));
}

async function storeLine(line) {
  const id = renderId;
  const head = h('h2', null, 'Store');
  put(line, head, h('span', { class: 'sk short', 'aria-hidden': 'true' }));
  try {
    const [s, ses] = await Promise.all([api('v1/stats'), api('v1/sessions', { limit: 1 })]);
    if (id !== renderId) return;
    const item = (label, v) => h('span', { class: 'item' }, label, h('b', { class: v === 0 ? 'zero' : null }, n(v)));
    put(line, head,
      h('span', { class: 'item' }, h('span', { class: s.health === 'ok' ? 'dot' : 'dot bad' }), s.health === 'ok' ? 'Healthy' : `Health: ${s.health}`),
      item('Durable LSN', s.durable_lsn), item('Fact keys', s.facts), item('Sessions', ses.total.sessions), item('Turns', ses.total.turns),
      h('a', { class: 'link', href: href('health') }, 'Open health'));
  } catch (err) {
    if (!(err instanceof ApiError)) throw err;
    if (id !== renderId) return;
    put(line, head, h('p', null, statsRefused(err) ? STATS_REFUSED : errorShort(err)));
  }
}

// 2. Log: every record, newest first by LSN, with tabs by kind
async function logScreen(main, p) {
  const kind = p.get('kind') === 'turn' || p.get('kind') === 'fact' ? p.get('kind') : '';
  const finding = titleRow(main, 'Log');
  note(finding, 'Every turn and fact revision, newest first by LSN. Superseded revisions and deletes stay in the log.');
  const counts = [fig(''), fig(''), fig('')];
  const tabs = [['All', ''], ['Turns', 'turn'], ['Facts', 'fact']].map(([label, k], i) =>
    h('a', { href: href('log', k ? { kind: k } : null), 'aria-current': k === kind ? 'page' : null }, label, counts[i]));
  const list = h('ol', { class: 'recs' });
  const foot = h('div', { class: 'card-foot' });
  main.append(h('nav', { class: 'tabs', 'aria-label': 'Record kind' }, tabs),
    h('section', { class: 'card wide', 'aria-label': 'Records' }, h('div', { class: 'card-body' }, listHead(), list), foot));
  const pages = pager({
    list, foot, noun: kind === 'turn' ? 'turn' : kind === 'fact' ? 'fact revision' : 'record',
    get: async (before) => { const pg = await api('v1/records', { before, limit: PAGE, kind: kind || undefined, max_bytes: EIGHT_MIB }); return { items: pg.records, next: pg.next }; },
    row: (r) => recordRow(r),
    emptyLines: [kind === 'fact' ? ['No fact revisions yet.'] : kind === 'turn' ? ['No turns yet.'] : nothingSaved()],
  });
  // The tab counts need stats, which count the whole store. Only a home studio shows them: the home holds 1 tenant,
  // while stats count a tenant only after its first write.
  const id = renderId;
  try {
    const info = await infoReady;
    if (id !== renderId || !info || info.store !== 'home') return;
    const [s, ses] = await Promise.all([api('v1/stats'), api('v1/sessions', { limit: 1 })]);
    if (id !== renderId || s.tenants > 1) return;
    // fact_revisions counts the revisions the store keeps: all of a key's up to 17, then its newest 17 or 18.
    // The log keeps every one. A key past 17 keeps 17 and every other key 1 or more, so below facts + 16 they match.
    if (s.fact_revisions >= s.facts + 16) {
      counts[1].textContent = n(ses.total.turns);
      if (kind === 'turn') pages.setTotal(ses.total.turns);
      return;
    }
    const all = [ses.total.turns + s.fact_revisions, ses.total.turns, s.fact_revisions];
    all.forEach((v, i) => { counts[i].textContent = n(v); });
    pages.setTotal(all[kind === 'turn' ? 1 : kind === 'fact' ? 2 : 0]);
    if (all[0]) say(finding, `${count(all[0], 'record')}: ${count(all[1], 'turn')} and ${count(all[2], 'fact revision')}, newest first by LSN. Superseded revisions and deletes stay in the log.`);
  } catch (err) { if (!(err instanceof ApiError)) throw err; }
}

// 5. Search: hits newest first, matched words marked
async function searchScreen(main, p) {
  const q = (p.get('q') || '').trim();
  const finding = titleRow(main, 'Search');
  const input = h('input', { class: 'input', type: 'search', name: 'q', value: q, placeholder: 'Words from a note, a fact or a key', 'aria-label': 'Search memory', autocomplete: 'off', spellcheck: 'false' });
  const form = h('form', { class: 'search-form', role: 'search' }, input, h('button', { class: 'btn primary', type: 'submit' }, 'Search'));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const v = input.value.trim();
    const to = href('search', v ? { q: v } : null);
    if (location.hash === to) redraw(); else location.hash = to;
  });
  main.append(form);
  // every Search screen starts in its box, so a search can be changed and sent again at once
  input.focus();
  input.setSelectionRange(q.length, q.length);
  if (!q) {
    note(finding, 'Find a note or a fact. Every word must appear in it, in any case. A plural also finds its singular.');
    return;
  }
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  const list = h('ol', { class: 'recs' });
  const foot = h('div', { class: 'card-foot' });
  main.append(h('section', { class: 'card wide', 'aria-label': 'Hits' }, h('div', { class: 'card-body' }, listHead(), list), foot));
  pager({
    list, foot, noun: 'hit',
    get: async (before) => { const pg = await api('v1/search', { q, before, limit: PAGE, max_bytes: EIGHT_MIB }); return { items: pg.hits, next: pg.next }; },
    row: (r) => recordRow(r, words),
    emptyLines: [['No note or fact holds every word of ', h('b', null, q), '. Try fewer words.']],
    onFirst: (shown, more) => {
      if (!shown) note(finding, 'No hits.');
      else say(finding, `${count(shown, 'hit')}${more !== null ? ' so far' : ''} for `, h('b', null, q), ', newest first.');
    },
  });
}

// 3. Sessions: which sessions hold turns, by agent and by name
async function sessionsScreen(main) {
  const id = renderId;
  const finding = titleRow(main, 'Sessions');
  const strip = h('div', { class: 'agents', role: 'group', 'aria-label': 'Show one agent' });
  const filter = h('input', { class: 'input', type: 'search', placeholder: 'Filter by session name', 'aria-label': 'Filter sessions by name', autocomplete: 'off', spellcheck: 'false' });
  const tbody = h('tbody');
  const matchNote = h('span', { class: 'card-note' });
  const list = card('Every session', matchNote);
  const table = h('table', null, h('thead', null, h('tr', null, h('th', null, 'Session'), h('th', null, 'Agent'), h('th', { class: 'n' }, 'Turns'))), tbody);
  main.append(strip, h('div', { class: 'toolbar' }, filter), list.el);
  busy(strip, 1);
  busy(list.body, 6);
  let data;
  try { data = await allSessions(); } catch (err) { if (id !== renderId) return; failed(list.body, err); strip.remove(); say(finding, errorShort(err)); return; }
  if (id !== renderId) return;
  const rows = data.rows;
  let { total, next } = data;
  if (!rows.length && next === null) { empty(list.body, nothingSaved()); strip.remove(); note(finding, 'No sessions yet.'); return; }
  let agent = null;
  let more = null;
  // Until the list is complete, the agent strip and the filter cover only the sessions loaded, and the foot says so.
  const build = () => {
    const byAgent = new Map();
    for (const s of rows) {
      const a = byAgent.get(s.agent) || { sessions: 0, turns: 0 };
      a.sessions++; a.turns += s.turns; byAgent.set(s.agent, a);
    }
    if (next === null) say(finding, `${count(total.sessions, 'session')} hold ${count(total.turns, 'turn')}, from ${count(byAgent.size, 'agent')}.`);
    else say(finding, `${count(total.sessions, 'session')} hold ${count(total.turns, 'turn')}. The first ${n(rows.length)} are loaded.`);
    const cells = [[null, 'All agents', total.sessions, total.turns], ...[...byAgent].map(([a, v]) => [a, a, v.sessions, v.turns])];
    const buttons = cells.map(([key, label, ses, turns]) => {
      const b = h('button', { class: 'agent', type: 'button', 'aria-pressed': String(key === agent) },
        h('span', { class: 'agent-name' }, key === null ? label : nameOf(label)),
        h('span', { class: 'agent-value' }, fig(n(ses)), ses === 1 ? 'session' : 'sessions'),
        h('span', { class: 'agent-value' }, count(turns, 'turn')));
      b.addEventListener('click', () => {
        agent = key;
        buttons.forEach((x, i) => x.setAttribute('aria-pressed', String(cells[i][0] === agent)));
        paint();
      });
      return b;
    });
    put(done(strip), buttons);
    if (next === null && !more) return;
    if (!more) {
      const foot = h('div', { class: 'card-foot' });
      list.el.append(foot);
      more = pagerFoot(foot, 'Load more sessions', async () => {
        try {
          const page = await allSessions(next);
          if (id !== renderId) return;
          rows.push(...page.rows);
          ({ total, next } = page);
          build();
          paint();
        } catch (err) { if (id === renderId) more.error(err); }
      }, null);
    }
    more.show(next === null ? `Showing all ${count(total.sessions, 'session')}.` : `Showing ${n(rows.length)} of ${count(total.sessions, 'session')}. The rest are not loaded, so counts by agent cover these.`, next !== null);
  };
  put(done(list.body), table);
  const paint = () => {
    const f = filter.value.trim().toLowerCase();
    const hit = rows.filter((s) => (agent === null || s.agent === agent) && (!f || s.session.toLowerCase().includes(f)));
    put(list.heading, agent === null ? 'Every session' : ['Sessions of ', nameOf(agent)]);
    put(matchNote, f ? `${n(hit.length)} match` : '');
    if (!hit.length) { put(tbody, h('tr', null, h('td', { colspan: '3', class: 'muted' }, 'No session name holds that text.'))); return; }
    put(tbody, hit.map((s) => h('tr', { class: 'linked' },
      h('td', null, h('a', { class: 'row-link', href: href('session', { agent: s.agent, session: s.session }) }, nameOf(s.session))),
      h('td', null, nameOf(s.agent)), h('td', { class: 'n' }, n(s.turns)))));
  };
  build();
  filter.addEventListener('input', paint);
  paint();
}

// 3b. Session: one session's turns, newest first, older pages by since_seq
function turnItem(t, want) {
  return h('li', { class: 'turn', 'aria-current': t.seq === want ? 'true' : null },
    h('p', { class: 'turn-head' }, 'Turn ', fig(String(t.seq)), t.role === 'assistant' ? h('span', null, 'assistant') : h('span', { class: 'pill' }, t.role)),
    h('p', { class: 'rec-when' }, timeCell(t.commit_time_ns), h('span', { class: 'lsn' }, 'LSN ' + t.lsn)),
    h('p', { class: 'turn-body' }, breakable(t.content)),
    t.meta === null || t.meta === undefined ? null : h('p', { class: 'turn-meta' }, breakable('meta ' + JSON.stringify(t.meta))));
}
async function sessionScreen(main, p) {
  // an empty agent or session name is a name: only a missing parameter means no session
  const known = p.has('agent') && p.has('session');
  const agent = p.get('agent') || '';
  const name = p.get('session') || '';
  const want = /^\d{1,15}$/.test(p.get('seq') || '') ? Number(p.get('seq')) : null;
  const finding = titleRow(main, known ? nameOf(name) : 'Session', [h('a', { href: href('sessions') }, 'Sessions'), known ? h('span', null, nameOf(agent)) : null].filter(Boolean));
  if (!known) { note(finding, 'Open a session from the list.'); return; }
  const c = card('Turns', h('span', { class: 'card-note' }, 'Newest first'));
  const list = h('ol', { class: 'turns' });
  const foot = h('div', { class: 'card-foot' });
  c.body.append(list);
  c.el.append(foot);
  main.append(c.el);
  busy(list, 4);
  const id = renderId;
  // the turns shown are low to high, of the session's total
  let low = 0;
  let high = 0;
  let total = 0;
  let older = null;
  let newer = null;
  // Turns from seq `from` to `to`, oldest first. Since keeps the oldest, so a cut answer goes on from its last turn.
  // Null when the user has left the screen.
  async function span(from, to) {
    const got = [];
    for (let at = from; at <= to;) {
      const w = await api('v1/window', { agent, session: name, since_seq: at, limit: to - at + 1, max_bytes: EIGHT_MIB });
      if (id !== renderId) return null;
      got.push(...w.turns);
      if (!w.truncated || !w.turns.length) break;
      at = w.turns[w.turns.length - 1].seq + 1;
    }
    return got;
  }
  const paint = () => {
    older = older || pagerFoot(foot, 'Older turns', async () => {
      try {
        const got = await span(Math.max(1, low - 50), low - 1);
        if (!got) return;
        list.append(...got.slice().reverse().map((t) => turnItem(t, want)));
        low = got.length ? got[0].seq : 1;
        paint();
      } catch (err) { if (id === renderId) older.error(err); }
    }, null);
    older.show(low === 1 && high === total ? `Showing all ${count(total, 'turn')}.` : `Showing ${n(high - low + 1)} of ${count(total, 'turn')}.`, low > 1);
    if (high === total && !newer) return;
    if (!newer) {
      const bar = h('div', { class: 'newer' });
      list.before(bar);
      newer = pagerFoot(bar, 'Newer turns', async () => {
        try {
          const got = await span(high + 1, Math.min(total, high + 50));
          if (!got) return;
          list.prepend(...got.slice().reverse().map((t) => turnItem(t, want)));
          high = got.length ? got[got.length - 1].seq : total;
          paint();
        } catch (err) { if (id === renderId) newer.error(err); }
      }, null);
    }
    newer.show(high < total ? `${count(total - high, 'newer turn')} not shown.` : 'These are the newest turns.', high < total);
  };
  try {
    // a window whose newest turn alone passes max_bytes holds no turns, and a turn can pass the 1 MiB default
    const w = await api('v1/window', { agent, session: name, last: 50, max_bytes: EIGHT_MIB });
    if (id !== renderId) return;
    if (!w.turns.length) { empty(list, ['No turns in this session.']); foot.remove(); note(finding, 'No turns from ', nameOf(agent), ' in this session.'); return; }
    const newest = w.turns[w.turns.length - 1];
    total = newest.seq;
    let shown = w.turns;
    // a link to a turn older than these reads a window around it, however far back it is
    if (want !== null && want < shown[0].seq) {
      const at = Math.max(1, want - 25);
      shown = await span(at, Math.min(total, at + 49));
      if (!shown) return;
      if (!shown.length) shown = w.turns;
    }
    put(done(list), ...shown.slice().reverse().map((t) => turnItem(t, want)));
    low = shown[0].seq;
    high = shown[shown.length - 1].seq;
    say(finding, h('span', { class: 'who' }, nameOf(agent)), total === 1
      ? ` saved 1 turn here, ${whenWords(newest.commit_time_ns)}.`
      : ` saved ${count(total, 'turn')} here. The newest came ${whenWords(newest.commit_time_ns)}, ${ago(newest.commit_time_ns)}.`);
    paint();
    list.querySelector('[aria-current="true"]')?.scrollIntoView({ block: 'center' });
  } catch (err) { if (id !== renderId) return; failed(list, err); foot.remove(); say(finding, errorShort(err)); }
}

// 4. Facts: the current value of every key, grouped by prefix
function factRow(f, group) {
  return h('li', { class: 'fact-row linked' },
    h('a', { class: 'key row-link', href: href('fact', { key: f.key }) }, f.key.slice(group.length) || f.key),
    h('p', { class: 'rec-text' }, preview(f.content)),
    h('span', { class: 'rec-agent' }, nameOf(f.agent)),
    h('p', { class: 'rec-meta' }, 'by ', h('span', { class: 'who' }, nameOf(f.agent))),
    h('span', { class: 'rec-when' }, timeCell(f.commit_time_ns), h('span', { class: 'lsn' }, 'LSN ' + f.lsn)));
}
async function factsScreen(main) {
  const id = renderId;
  const finding = titleRow(main, 'Facts');
  const holder = card('Current values');
  main.append(holder.el);
  busy(holder.body, 6);
  let data;
  try { data = await allFacts(); } catch (err) { if (id !== renderId) return; failed(holder.body, err); say(finding, errorShort(err)); return; }
  if (id !== renderId) return;
  const facts = data.facts;
  let next = data.next;
  if (!facts.length && next === null) {
    empty(holder.body, ['No facts yet. An agent saves one with the ', code('fact_set'), ' tool, under ', code('user/'), ' for rules or ', code('repo/<repo>/'), ' for a repo.']);
    note(finding, 'No facts yet.');
    return;
  }
  const rank = (g) => (g === 'user/' ? 0 : g.startsWith('repo/') ? 1 : g ? 2 : 3);
  let cards = [holder.el];
  let more = null;
  let moreCard = null;
  // Until the list is complete the groups show the facts loaded and a last card offers the rest. Only a complete
  // list tells how many keys were deleted.
  const paint = () => {
    const groups = new Map();
    for (const f of facts) {
      const g = groupOf(f.key);
      if (!groups.has(g)) groups.set(g, []);
      groups.get(g).push(f);
    }
    const order = [...groups.keys()].sort((a, b) => rank(a) - rank(b) || (a < b ? -1 : a > b ? 1 : 0));
    const fresh = order.map((g) => {
      const c = card(g ? code(g) : 'Keys with no prefix', h('span', { class: 'card-note' }, count(groups.get(g).length, 'fact')));
      c.body.append(h('ol', { class: 'rows' }, groups.get(g).map((f) => factRow(f, g))));
      return c.el;
    });
    const anchor = cards[0] || moreCard;
    if (anchor) anchor.before(...fresh); else main.append(...fresh);
    for (const el of cards) el.remove();
    cards = fresh;
    if (next !== null) {
      say(finding, `The first ${count(facts.length, 'fact')} with a value, in ${count(order.length, 'group')}. More keys follow.`);
      if (!more) {
        const foot = h('div', { class: 'card-foot' });
        moreCard = h('section', { class: 'card', 'aria-label': 'More facts' }, foot);
        main.append(moreCard);
        more = pagerFoot(foot, 'Load more facts', async () => {
          try {
            const page = await allFacts(next);
            if (id !== renderId) return;
            facts.push(...page.facts);
            next = page.next;
            paint();
          } catch (err) { if (id === renderId) more.error(err); }
        }, null);
      }
      more.show(`Showing ${count(facts.length, 'fact')}. The rest are not loaded.`, true);
      return;
    }
    say(finding, `${count(facts.length, 'fact')} hold a value, in ${count(order.length, 'group')}.`);
    if (more) more.show(`Showing all ${count(facts.length, 'fact')}.`, false);
    deleted();
  };
  // Stats count the keys of every tenant, deleted ones too. The home holds 1 tenant, so only there is the gap this
  // tenant's deleted keys.
  const deleted = async () => {
    try {
      const info = await infoReady;
      if (id !== renderId || !info || info.store !== 'home') return;
      const s = await api('v1/stats');
      if (id !== renderId) return;
      if (s.tenants <= 1 && s.facts > facts.length) finding.append(` ${count(s.facts - facts.length, 'more key')} ${s.facts - facts.length === 1 ? 'was' : 'were'} deleted.`);
    } catch (err) { if (!(err instanceof ApiError)) throw err; }
  };
  paint();
}

// 4b. Fact: the current value and every revision, newest first, deletes marked
async function factScreen(main, p) {
  const key = p.get('key') || '';
  const group = groupOf(key);
  const finding = titleRow(main, key ? key.slice(group.length) || key : 'Fact', [h('a', { href: href('facts') }, 'Facts'), group ? h('span', null, group) : null].filter(Boolean));
  if (!key) { note(finding, 'Open a fact from the list.'); return; }
  const now = card('Current value');
  const revs = card('Revisions', h('span', { class: 'card-note' }, 'Newest first'));
  const near = card(group ? ['Also under ', code(group)] : 'Other keys', h('a', { class: 'link', href: href('facts') }, 'All facts'));
  main.append(now.el, revs.el, near.el);
  siblings(near, key, group);
  busy(now.body, 1);
  busy(revs.body, 3);
  const id = renderId;
  // 1 read, so the current value, a delete and the revisions all come from the same snapshot of the store
  let hist;
  try { hist = await api('v1/facts/history', { key, max_bytes: EIGHT_MIB }); } catch (err) {
    if (id !== renderId) return;
    failed(now.body, err); failed(revs.body, err); say(finding, errorShort(err));
    return;
  }
  if (id !== renderId) return;
  const list = hist.revisions;
  if (!list.length) { empty(now.body, ['No fact has the key ', code(key), '.']); revs.el.remove(); note(finding, 'No such fact.'); return; }
  // the newest revision is the current value, unless it is a delete
  const last = list[0];
  const cur = last.deleted ? null : last;
  // truncated: the store lists only its newest revisions, at most 16 and 8 MiB of them. The log keeps every one.
  const shown = `${count(list.length, 'revision')}${hist.truncated ? ' shown' : ''}`;
  const line = (label, ...v) => h('span', null, label, ' ', h('b', null, v));
  if (cur) {
    put(done(now.body), h('p', { class: 'lead' }, breakable(cur.content)),
      h('p', { class: 'facts-line' }, line('Key', code(key)), line('Set by', nameOf(cur.agent)), line('At', timeCell(cur.commit_time_ns)), line('LSN', fig(String(cur.lsn)))),
      cur.meta === null || cur.meta === undefined ? null : h('p', { class: 'meta-json' }, breakable('meta ' + JSON.stringify(cur.meta))));
    say(finding, `${shown}. Last set by `, h('span', { class: 'who' }, nameOf(cur.agent)), ` ${whenWords(cur.commit_time_ns)}.`);
  } else {
    empty(now.body, ['No current value. ', h('b', null, nameOf(last.agent)), ` deleted this key ${whenWords(last.commit_time_ns)}, at LSN ${last.lsn}.`]);
    say(finding, `${shown}. Deleted ${whenWords(last.commit_time_ns)}.`);
  }
  put(done(revs.body), h('ol', { class: 'rows' }, list.map((r) => h('li', { class: 'rev' },
    h('span', { class: 'rec-lsn' }, String(r.lsn)),
    r.deleted
      ? h('p', { class: 'rec-text is-deleted' }, h('span', { class: 'pill' }, 'Deleted'), 'The key held no value after this.')
      : h('p', { class: 'rec-text' }, r === cur ? h('span', { class: 'pill' }, 'Current') : null, breakable(r.content)),
    h('span', { class: 'rec-agent' }, nameOf(r.agent)),
    h('p', { class: 'rec-meta' }, 'by ', h('span', { class: 'who' }, nameOf(r.agent))),
    h('span', { class: 'rec-when' }, timeCell(r.commit_time_ns), h('span', { class: 'lsn' }, 'LSN ' + r.lsn))))),
    hist.truncated ? h('p', { class: 'chart-note' }, 'Older revisions are not listed here. ', h('a', { class: 'link', href: href('log', { kind: 'fact' }) }, 'The log'), ' keeps every one.') : null);
}

// Up to 6 other keys of the fact's group. Keys with no prefix lie between whole groups of prefixed keys, so for them
// a page that ends inside a group goes on past it. This reads until a 7th turns up or the list ends, at most 20 pages.
async function siblings(near, key, group) {
  const id = renderId;
  busy(near.body, 3);
  try {
    const others = [];
    let after;
    let ended = false;
    for (let i = 0; i < 20 && others.length < 7 && !ended; i++) {
      const page = await api('v1/facts/list', { prefix: group || undefined, after, limit: 8 });
      if (id !== renderId) return;
      others.push(...page.facts.filter((f) => f.key !== key && groupOf(f.key) === group));
      ended = page.next === null;
      after = page.next;
      const slash = group || ended ? -1 : after.indexOf('/');
      if (slash > 0) after = pastGroup(after.slice(0, slash + 1), after) || after;
    }
    const all = h('a', { class: 'link', href: href('facts') }, 'See them all');
    if (!others.length) { empty(near.body, ended ? ['No other fact shares this prefix.'] : ['None turned up in the keys read so far. ', all, '.']); return; }
    put(done(near.body), h('ol', { class: 'rows' }, others.slice(0, 6).map((f) => factRow(f, group))),
      others.length > 6 ? h('p', { class: 'chart-note' }, 'More keys share this prefix. ', all, '.')
        : ended ? null : h('p', { class: 'chart-note' }, 'More keys may share this prefix. ', all, '.'));
  } catch (err) { if (id === renderId) failed(near.body, err); }
}

// 6. Chat start: the block a new chat in a repo starts with
async function chatStartScreen(main, p) {
  const id = renderId;
  const finding = titleRow(main, 'Chat start');
  const info = await infoReady;
  if (id !== renderId) return;
  if (info && info.store !== 'home') {
    note(finding, `The chat-start hook reads only the home store. This studio reads a ${info.store === 'url' ? 'store at a URL' : 'data dir'}, so no block applies here.`);
    return;
  }
  const tabs = h('nav', { class: 'tabs repos', 'aria-label': 'Repo' });
  const actions = h('span', { class: 'card-actions' });
  const view = card('Block', actions, 'chat-card');
  main.append(tabs, view.el);
  blockSkeleton(view.body);
  let facts;
  let sessions;
  let records = [];
  try {
    [facts, sessions] = await Promise.all([factRepos(), allSessions()]);
    if (id !== renderId) return;
    if (!p.get('repo')) records = (await newestRecords(50)) || [];
  } catch (err) { if (id !== renderId) return; tabs.remove(); failed(view.body, err); say(finding, errorShort(err)); return; }
  if (id !== renderId) return;
  const names = facts.names;
  for (const s of sessions.rows) { const r = s.session.split('@')[0]; if (r) names.add(r); }
  const repo = p.get('repo') || newestRepo(records) || [...names].sort()[0] || '';
  if (!repo) {
    tabs.remove();
    empty(view.body, ['A repo shows here once an agent saves a fact under ', code('repo/<repo>/'), ' or a turn in a session named after the repo.']);
    note(finding, 'No repo yet.');
    return;
  }
  // The picker holds the repo whose block shows, or a phone's select would name another. A link can name any repo,
  // and the newest record can be the delete of a repo's last fact.
  names.add(repo);
  const sorted = [...names].sort();
  put(tabs, sorted.map((r) => h('a', { href: href('chat-start', { repo: r }), 'aria-current': r === repo ? 'page' : null }, r)));
  // a phone picks the repo from a select instead, so the chosen one is never scrolled out of view
  const select = h('select', { class: 'input repo-select', 'aria-label': 'Repo' }, sorted.map((r) => h('option', { value: r, selected: r === repo }, r)));
  select.addEventListener('change', () => { location.hash = href('chat-start', { repo: select.value }); });
  tabs.after(select);
  if (facts.next !== null || sessions.next !== null) {
    select.after(h('p', { class: 'toolbar muted' }, 'Some repos may be missing: the store holds more facts or sessions than this list reads at once.'));
  }
  put(view.heading, repo);
  const copy = h('span');
  put(actions, copy);
  const b = await showBlock(view.body, repo, copy);
  if (id !== renderId) return;
  if (!b) { note(finding, 'No block to show.'); return; }
  if (b.block === null) { note(finding, 'A new chat in ', h('b', null, repo), ' starts with no memory block.'); return; }
  const cut = b.block.endsWith('\n(more in zerodb)');
  say(finding, 'A new chat in ', h('b', null, repo), ` starts with this block: ${n(b.chars)} of ${n(b.max)} characters${cut ? ', cut at the cap' : ''}.`);
}

// 7. Health: is the store healthy, and how fast does it ack a write?
function parseHistogram(text, name) {
  const lines = text.split('\n');
  const raw = [];
  for (const line of lines) {
    const m = /^([a-z_]+)_bucket\{le="([^"]+)"\} (\S+)$/.exec(line);
    if (m && m[1] === name) raw.push({ le: m[2] === '+Inf' ? Infinity : Number(m[2]), cum: Number(m[3]) });
  }
  const value = (suffix) => { const line = lines.find((l) => l.startsWith(name + suffix + ' ')); return line ? Number(line.slice(line.indexOf(' ') + 1)) : 0; };
  return { buckets: raw.map((b, i) => ({ ...b, n: b.cum - (i ? raw[i - 1].cum : 0) })), sum: value('_sum'), count: value('_count') };
}
const boundMs = (le) => n(Number((le * 1000).toFixed(3)));
function bucketName(buckets, i) {
  if (buckets[i].le === Infinity) return `over ${boundMs(buckets[i - 1].le)} ms`;
  return i === 0 ? `up to ${boundMs(buckets[0].le)} ms` : `${boundMs(buckets[i - 1].le)} to ${boundMs(buckets[i].le)} ms`;
}
// round, even steps for a count axis: integers, the top tick above the largest bar
function ticks(max) {
  if (max <= 0) return [0, 1, 2, 3, 4];
  const mag = 10 ** Math.floor(Math.log10(max / 4));
  for (const m of [1, 2, 5, 10, 20, 50]) {
    const step = Math.round(m * mag * 1e6) / 1e6;
    if (step < 1) continue;
    const parts = Math.floor(max / step) + 1;
    if (parts <= 5) return Array.from({ length: parts + 1 }, (_, i) => i * step);
  }
  return [0, max * 2];
}
// one column per bucket of the store's fixed bounds, in the one chart hue
function histogram(el, buckets, label) {
  let last = '';
  const draw = () => {
    const W = Math.max(280, Math.floor(el.clientWidth));
    const H = Math.max(216, Math.floor(el.clientHeight));
    if (`${W}x${H}` === last) return;
    last = `${W}x${H}`;
    const left = 40; const top = 22; const foot = 26;
    const plotW = W - left; const plotH = H - top - foot;
    const t = ticks(Math.max(0, ...buckets.map((b) => b.n)));
    const ceil = t[t.length - 1];
    const y = (v) => Math.round(top + plotH - (v / ceil) * plotH) + 0.5;
    const col = plotW / buckets.length;
    const marks = [];
    for (const v of t) {
      marks.push(svg('line', { class: v === 0 ? 'baseline' : 'gridline', x1: left, x2: W, y1: y(v), y2: y(v) }));
      marks.push(svg('text', { x: left - 10, y: y(v) + 4, 'text-anchor': 'end' }, n(v)));
    }
    buckets.forEach((b, i) => {
      const x = left + i * col;
      if (b.n > 0) {
        const bh = (b.n / ceil) * plotH;
        marks.push(svg('rect', { class: 'bar', x: (x + 2).toFixed(1), y: (top + plotH - bh).toFixed(1), width: Math.max(1, col - 4).toFixed(1), height: bh.toFixed(1) },
          svg('title', null, `${bucketName(buckets, i)}: ${count(b.n, 'append')}`)));
        marks.push(svg('text', { class: 'val', x: (x + col / 2).toFixed(1), y: (top + plotH - bh - 6).toFixed(1), 'text-anchor': 'middle' }, n(b.n)));
      }
      if (i % 2 === 0 && b.le !== Infinity) marks.push(svg('text', { x: (x + col / 2).toFixed(1), y: H - 6, 'text-anchor': 'middle' }, boundMs(b.le)));
    });
    put(el, svg('svg', { viewBox: `0 0 ${W} ${H}`, width: W, height: H, role: 'img', 'aria-label': label }, marks));
  };
  draw();
  if ('ResizeObserver' in window) new ResizeObserver(() => requestAnimationFrame(draw)).observe(el);
}
async function healthScreen(main) {
  const finding = titleRow(main, 'Health');
  const kpis = h('dl', { class: 'kpis' });
  const ackNote = h('span', { class: 'card-note' }, 'Buckets by upper bound, in ms');
  const ack = card('Append ack time', ackNote, 'span-7');
  const stages = card('Writer stages', h('span', { class: 'card-note' }, 'Time per group'), 'span-5');
  const index = card('Tape index', h('span', { class: 'card-note' }, 'Resident against budget'), 'span-7');
  const store = card('Store', h('span', { class: 'card-note' }, 'Since the server started'), 'span-5');
  main.append(kpis, h('div', { class: 'grid' }, ack.el, stages.el), h('div', { class: 'grid' }, index.el, store.el));
  const kpi = (label, value, cls) => h('div', { class: 'kpi' }, h('dt', null, label), h('dd', { class: cls || null }, value));
  const labels = ['Health', 'Durable LSN', 'Fact keys', 'Fact revisions', 'Writer groups', 'Busy replies'];
  put(kpis, ...labels.map((l) => kpi(l, h('span', { class: 'sk line', 'aria-hidden': 'true' }))));
  busy(ack.body, 4); busy(stages.body, 4); busy(index.body, 2); busy(store.body, 2);
  const id = renderId;
  let s;
  let text;
  try { [s, text] = await Promise.all([api('v1/stats'), api('metrics')]); } catch (err) {
    if (id !== renderId) return;
    if (statsRefused(err)) { for (const c of [ack, stages, index, store]) empty(c.body, [STATS_REFUSED]); note(finding, STATS_REFUSED); } else {
      for (const c of [ack, stages, index, store]) failed(c.body, err);
      say(finding, errorShort(err));
    }
    put(kpis, ...labels.map((l) => kpi(l, '-', 'zero')));
    return;
  }
  if (id !== renderId) return;
  const num = (v) => [n(v), v === 0 ? 'zero' : null];
  const ok = s.health === 'ok';
  put(kpis, 
    kpi('Health', [h('span', { class: ok ? 'dot' : 'dot bad' }), ok ? 'Healthy' : s.health], 'word'),
    kpi('Durable LSN', ...num(s.durable_lsn)), kpi('Fact keys', ...num(s.facts)), kpi('Fact revisions', ...num(s.fact_revisions)),
    kpi('Writer groups', ...num(s.writer_groups)), kpi('Busy replies', ...num(s.busy_replies)));

  const hist = parseHistogram(text, 'zerodb_append_ack_seconds');
  const total = hist.count;
  const all = hist.buckets.find((b) => total > 0 && b.cum >= total);
  const half = hist.buckets.find((b) => total > 0 && b.cum >= total / 2);
  const chart = h('div', { class: 'chart' });
  put(done(ack.body), chart);
  if (total > 0) ackNote.textContent = `Buckets by upper bound, in ms. Mean ${ms3((hist.sum / total) * 1e9)} ms`;
  histogram(chart, hist.buckets, total ? `${count(total, 'append')}, all ${all.le === Infinity ? 'counted' : 'under ' + boundMs(all.le) + ' ms'}` : 'No appends yet');

  const rows = [['Write', s.writer_write_ns, s.writer_write_max_ns], ['Sync', s.writer_sync_ns, s.writer_sync_max_ns], ['Publish', s.writer_publish_ns, s.writer_publish_max_ns], ['Reply', s.writer_reply_ns, null]];
  const sum = rows.reduce((a, r) => a + r[1], 0);
  const g = s.writer_groups;
  // the group row adds up the figures as shown, so the column sums on screen
  const means = rows.map((r) => (g ? Number(ms3(r[1] / g)) : null));
  // shares in tenths that add up to 100.0%: each rounded down, then the tenths left go to the largest remainders
  const exact = rows.map((r) => (sum ? (r[1] / sum) * 1000 : 0));
  const tenths = exact.map(Math.floor);
  const order = exact.map((_, i) => i).sort((a, b) => (exact[b] - tenths[b]) - (exact[a] - tenths[a]));
  for (let k = 0, left = sum ? 1000 - tenths.reduce((a, b) => a + b, 0) : 0; k < left; k++) tenths[order[k]]++;
  const shares = tenths.map((t) => t / 10);
  const cell = (v) => h('td', { class: v === '-' || v === '0.000' ? 'n zero' : 'n' }, v);
  put(done(stages.body), h('table', null,
    h('thead', null, h('tr', null, h('th', null, 'Stage'), h('th', { class: 'n' }, 'Mean, ms'), h('th', { class: 'n' }, 'Max, ms'), h('th', { class: 'n' }, 'Share'))),
    h('tbody', null, rows.map(([label, , max], i) => h('tr', null, h('td', null, label), cell(g ? means[i].toFixed(3) : '-'), cell(max === null ? '-' : ms3(max)), cell(sum ? `${shares[i].toFixed(1)}%` : '-')))),
    h('tfoot', null, h('tr', null, h('td', null, 'Group'), cell(g ? means.reduce((a, b) => a + b, 0).toFixed(3) : '-'), cell('-'),
      cell(sum ? `${shares.reduce((a, b) => a + b, 0).toFixed(1)}%` : '-')))));

  const used = s.tape_index_resident_bytes;
  const budget = s.tape_index_budget_bytes;
  const fill = h('div', { class: 'meter-fill ink' });
  fill.style.setProperty('width', `${budget ? Math.min(100, (used / budget) * 100).toFixed(2) : 0}%`);
  const kv = (label, v) => [h('dt', null, label), h('dd', { class: v === 0 ? 'zero' : null }, typeof v === 'number' ? n(v) : v)];
  put(done(index.body), 
    h('div', { class: 'meter' }, h('span', null, h('b', null, size(used)), ' resident'), h('span', null, 'budget ', h('b', null, size(budget))),
      h('div', { class: 'meter-track', role: 'meter', 'aria-label': 'Tape index residency', 'aria-valuemin': '0', 'aria-valuemax': String(budget), 'aria-valuenow': String(used) }, fill)),
    h('p', { class: 'chart-note' }, used > 0 && budget > 0 ? `The budget holds about ${n(Math.floor(budget / used))} times today's index.` : 'The index is empty.'),
    h('dl', { class: 'kv' }, kv('Share of budget', budget ? `${((used / budget) * 100).toFixed(1)}%` : '-'), kv('Page evictions', s.tape_index_page_evictions),
      kv('Dirty page evictions', s.tape_index_dirty_page_evictions), kv('Page reloads', s.tape_index_page_reloads)));
  const readOnly = /^zerodb_read_only (\S+)$/m.exec(text);
  put(done(store.body), h('dl', { class: 'kv first' }, kv('Tenants', s.tenants), kv('Read only', readOnly ? (readOnly[1] === '0' ? 'No' : 'Yes') : '-'),
    kv('Window reads', s.window_reads), kv('Checkpoint pending', size(s.checkpoint_pending_bytes)), kv('Checkpoint saves', s.checkpoint_saves),
    kv('Instance', h('span', { title: String(s.instance || '-') }, String(s.instance || '-').slice(0, 8) + (String(s.instance || '').length > 8 ? '\u2026' : '')))));

  const parts = [ok ? 'Healthy.' : `Health is ${s.health}.`];
  if (!total) parts.push('No appends acked since the server started.');
  else if (all.le === Infinity) parts.push(`${count(total, 'append')} acked since the server started; the slowest took over 1 s.`);
  else if (half.le === all.le) parts.push(`All ${count(total, 'append')} acked since the server started took under ${boundMs(all.le)} ms.`);
  else parts.push(`${count(total, 'append')} acked since the server started: half under ${boundMs(half.le)} ms, all under ${boundMs(all.le)} ms.`);
  if (sum) parts.push(`Sync takes ${shares[1].toFixed(1)}% of a writer group's time.`);
  say(finding, parts.join(' '));
}

// ---------- router ----------
const SCREENS = new Map([['', overview], ['log', logScreen], ['sessions', sessionsScreen], ['session', sessionScreen], ['facts', factsScreen],
  ['fact', factScreen], ['search', searchScreen], ['chat-start', chatStartScreen], ['health', healthScreen]]);
const NAV = new Map([['', 'overview'], ['session', 'sessions'], ['fact', 'facts']]);
let renderId = 0;
function render(toTop = true) {
  const raw = location.hash.replace(/^#\/?/, '');
  const qi = raw.indexOf('?');
  const name = qi < 0 ? raw : raw.slice(0, qi);
  const known = SCREENS.has(name);
  const nav = known ? NAV.get(name) || name : 'overview';
  for (const a of document.querySelectorAll('.nav-item')) {
    if (a.dataset.route === nav) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  setMenu(false);
  const id = ++renderId;
  const main = byId('main');
  put(main);
  if (toTop) scrollTo(0, 0);
  (known ? SCREENS.get(name) : overview)(main, new URLSearchParams(qi < 0 ? '' : raw.slice(qi + 1))).catch((err) => {
    console.error(err);
    // a screen the user has left fails into elements that are gone, not into the one on screen now
    if (id === renderId) main.append(h('div', { class: 'state error', role: 'alert' }, h('p', null, 'This screen failed to draw. Reload the page.')));
  });
}
// Focuses the screen's title, unless the screen put focus in its own content, as Search does with its box.
function focusTitle() {
  const main = byId('main');
  if (!main.contains(document.activeElement)) main.querySelector('h1')?.focus({ preventScroll: true });
}
// Draws the screen again in place. The button that asked is gone with the old screen, so the title takes focus.
function redraw() {
  render(false);
  focusTitle();
}
addEventListener('hashchange', () => {
  takeSecret();
  render();
  focusTitle();
});
render();
