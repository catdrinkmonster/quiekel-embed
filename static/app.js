"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  page: "search", kind: "all", scope: null, similar: null,
  foldersKey: "", settingsKey: "", deviceKey: "", folderInfo: null,
  removing: new Map(), // folders on their way out: id -> { path, total, left, sent }
  ready: false, appMode: false, hasFolders: null,
  results: [], selected: -1, terms: [], lightbox: -1,
  update: null, resultShown: false, applying: false, last: null, statusClick: null,
  views: { view_files: "cards", view_images: "grid" }, viewSaved: {},
  sort: { key: "relevance", desc: false }, fetched: null,
};
let searchCtl = null;
let searchTimer = null;
let toastTimer = null;
let pollTimer = null;

// ---------- icons ----------

// Line icons on a 24px grid. Static markup only, never anything from files or the network.
const ICONS = {
  search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="M15.5 15.5 21 21"/>',
  folder: '<path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4.4l2 2.2h8.6A1.5 1.5 0 0 1 21 9.7v8.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>',
  folderPlus: '<path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4.4l2 2.2h8.6A1.5 1.5 0 0 1 21 9.7v8.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/><path d="M12 11v6M9 14h6"/>',
  settings: '<path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/>',
  pause: '<path d="M9 6v12M15 6v12"/>',
  play: '<path d="M8 5.5v13l10.5-6.5z"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
  hourglass: '<path d="M6.5 4h11M6.5 20h11M8 4v2.5c0 2 1.8 3.6 4 5.5 2.2-1.9 4-3.5 4-5.5V4M8 20v-2.5c0-2 1.8-3.6 4-5.5 2.2 1.9 4 3.5 4 5.5V20"/>',
  // Bold, with sand: in the status ring it runs down and turns over (style.css, .timer).
  timer: '<g class="timer"><path class="frame" d="M5.5 3.5h13M5.5 20.5h13M7.5 3.5v2.1c0 2.6 1.8 4.6 4.5 6.4-2.7 1.8-4.5 3.8-4.5 6.4v2.1M16.5 3.5v2.1c0 2.6-1.8 4.6-4.5 6.4 2.7 1.8 4.5 3.8 4.5 6.4v2.1"/>'
    + '<path class="sand solid" d="M9.3 7.4h5.4L12 10.6z"/><path class="sand bottom solid" d="M12 15.3l3.1 3.7H8.9z"/></g>',

  alert: '<path d="M12 4 21 19.5H3z"/><path d="M12 10v4M12 16.8v.2"/>',
  refresh: '<path d="M20 11.5A8 8 0 1 1 17.7 6"/><path d="M20 4.5V9h-4.5"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a1.5 1.5 0 0 0 1.5 1.5h7A1.5 1.5 0 0 0 17 19l1-12M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7"/>',
  eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>',
  eyeOff: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/><path d="M4 4l16 16"/>',
  external: '<path d="M14 4h6v6M20 4l-9 9"/><path d="M18 14v4.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 4 18.5v-11A1.5 1.5 0 0 1 5.5 6H10"/>',
  similar: '<path d="M4.5 9.5c2.5-2.2 5-2.2 7.5 0s5 2.2 7.5 0M4.5 15.5c2.5-2.2 5-2.2 7.5 0s5 2.2 7.5 0"/>',
  close: '<path d="M6 6l12 12M18 6 6 18"/>',
  chevronLeft: '<path d="M15 5l-7 7 7 7"/>',
  chevronRight: '<path d="M9 5l7 7-7 7"/>',
  all: '<path d="M12 3.5 20.5 8 12 12.5 3.5 8z"/><path d="M3.5 12 12 16.5 20.5 12M3.5 16 12 20.5 20.5 16"/>',
  rows: '<rect x="3.5" y="4" width="17" height="7" rx="1.6"/><rect x="3.5" y="13" width="17" height="7" rx="1.6"/><path d="M7 7.5h7M7 16.5h7"/>',
  list: '<path d="M9 6h11.5M9 12h11.5M9 18h11.5M4.3 6h.4M4.3 12h.4M4.3 18h.4"/>',
  grid: '<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>',
  doc: '<path d="M14 3H7.5A1.5 1.5 0 0 0 6 4.5v15A1.5 1.5 0 0 0 7.5 21h9a1.5 1.5 0 0 0 1.5-1.5V7z"/><path d="M14 3v4h4M9 12h6M9 16h6"/>',
  code: '<path d="M8.5 7l-5 5 5 5M15.5 7l5 5-5 5M13.5 5l-3 14"/>',
  image: '<rect x="3.5" y="5" width="17" height="14" rx="2"/><circle cx="9" cy="10" r="1.6"/><path d="M20.5 16l-5-5-8.5 8"/>',
  chip: '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M10 10h4v4h-4zM9.5 3v3M14.5 3v3M9.5 18v3M14.5 18v3M3 9.5h3M3 14.5h3M18 9.5h3M18 14.5h3"/>',
  power: '<path d="M12 3.5v8"/><path d="M7 6.5a7 7 0 1 0 10 0"/>',
  globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.4 2.5 3.5 5.4 3.5 8.5s-1.1 6-3.5 8.5c-2.4-2.5-3.5-5.4-3.5-8.5s1.1-6 3.5-8.5z"/>',
  update: '<circle cx="12" cy="12" r="8.5"/><path d="M12 16V8M8.5 11.5 12 8l3.5 3.5"/>',
  download: '<path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M5 19.5h14"/>',
  arrowRight: '<path d="M5 12h14M13 6l6 6-6 6"/>',
  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
  maximize: '<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>',
  text: '<path d="M5 6h14M5 10h14M5 14h14M5 18h9"/>',
  keyboard: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6.5 10h.01M10 10h.01M13.5 10h.01M17 10h.01M7 14h10"/>',
  arrowLeft: '<path d="M19 12H5M11 6l-6 6 6 6"/>',
  chevronDown: '<path d="M7 10l5 5 5-5"/>',
  chevronUp: '<path d="M7 14l5-5 5 5"/>',
  pulse: '<path d="M3 12h4l2.5-6 4 12 2.5-6H21"/>',
  gift: '<path d="M4 11h16v9H4zM3 7.5h18V11H3zM12 7.5V20"/><path d="M12 7.5C10.5 4.5 7 4 7 6s3 1.5 5 1.5zm0 0c1.5-3 5-3.5 5-1.5s-3 1.5-5 1.5z"/>',
  up: '<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>',
  moon: '<path d="M19.5 14.5A8 8 0 1 1 9.5 4.5a6.5 6.5 0 0 0 10 10z"/>',
  sun: '<circle cx="12" cy="12" r="3.8"/><path d="M12 3v1.8M12 19.2V21M3 12h1.8M19.2 12H21M5.6 5.6l1.3 1.3M17.1 17.1l1.3 1.3M5.6 18.4l1.3-1.3M17.1 6.9l1.3-1.3"/>',
  monitor: '<rect x="3" y="4.5" width="18" height="12" rx="1.8"/><path d="M9 20h6M12 16.5V20"/>',
  contrast: '<circle cx="12" cy="12" r="8.5"/><path class="solid" d="M12 3.5a8.5 8.5 0 0 1 0 17z"/>',
  cube: '<path d="M12 3 20 7.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5 12 12l8-4.5M12 12v9"/>',
  near: '<circle cx="12" cy="12" r="8.5"/><circle cx="12" cy="12" r="4.5"/><path d="M12 12h.01"/>',
  focus: '<circle cx="12" cy="12" r="3"/><path d="M4 9V5.5A1.5 1.5 0 0 1 5.5 4H9M15 4h3.5A1.5 1.5 0 0 1 20 5.5V9M20 15v3.5a1.5 1.5 0 0 1-1.5 1.5H15M9 20H5.5A1.5 1.5 0 0 1 4 18.5V15"/>',
  mouse: '<rect x="6.5" y="3.5" width="11" height="17" rx="5.5"/><path d="M12 7.5v3"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5M12 7.6v.2"/>',
  skip: '<path d="M5 6.5 12 12l-7 5.5zM12 6.5 19 12l-7 5.5z"/>',
  ram: '<rect x="3" y="6.5" width="18" height="10" rx="1.5"/><path d="M7.5 10v3M12 10v3M16.5 10v3M6.5 16.5v2.5M10 16.5v2.5M14 16.5v2.5M17.5 16.5v2.5"/>',
  leaf: '<path d="M5 19.5C4.5 11 9.5 5 19.5 4.5 19.5 14 14 19.5 5 19.5z"/><path d="M5 19.5 13.5 11"/>',
  bolt: '<path d="M13.5 3 5.5 13.5h6l-1 7.5 8-10.5h-6z"/>',
  dots: '<path d="M6 6h1M11.5 6h1M17 6h1M6 11.5h1M11.5 11.5h1M17 11.5h1M6 17h1M11.5 17h1M17 17h1"/>',
  bug: '<path d="M8.5 7.5a3.5 3.5 0 0 1 7 0"/><rect x="7" y="7.5" width="10" height="12.5" rx="5"/><path d="M12 11v9M7 12.5H3.5M20.5 12.5H17M7 16.5l-3 1.5M17 16.5l3 1.5M7.6 9 4.5 7M16.4 9l3.1-2"/>',
  archive: '<rect x="3.5" y="4.5" width="17" height="4.5" rx="1.2"/><path d="M5 9v9.5A1.5 1.5 0 0 0 6.5 20h11a1.5 1.5 0 0 0 1.5-1.5V9M10 13h4"/>',
  lock: '<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8 10.5V7.5a4 4 0 0 1 8 0v3M12 14.5v2"/>',
  film: '<rect x="3.5" y="5" width="17" height="14" rx="2"/><path d="M7.5 5v14M16.5 5v14M3.5 9.5h4M3.5 14.5h4M16.5 9.5h4M16.5 14.5h4"/>',
  app: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M3.5 8.5h17M6.5 6.5h.01M9 6.5h.01"/>',
  mail: '<rect x="3" y="5.5" width="18" height="13" rx="2"/><path d="M3.5 7 12 13l8.5-6"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1.2 1.2"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1.2-1.2"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  bang: '<path d="M12 5.5v8M12 18v.5"/>',
  camera: '<path d="M4 8.5A1.5 1.5 0 0 1 5.5 7h2.3l1.4-2h5.6l1.4 2h2.3A1.5 1.5 0 0 1 20 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 17.5z"/><circle cx="12" cy="12.5" r="3.5"/>',
  help: '<circle cx="12" cy="12" r="8.5"/><path d="M9.6 9.6a2.5 2.5 0 1 1 3.3 2.4c-.6.3-.9.8-.9 1.4v.3M12 16.6v.2"/>',
  sort: '<path d="M7.5 4.5v15M4 16l3.5 3.5L11 16M16.5 19.5v-15M13 8l3.5-3.5L20 8"/>',
  size: '<rect x="4" y="13" width="7" height="7" rx="1.5"/><path d="M14 4h6v6M20 4l-7.5 7.5"/>',
};

// The map's auto-rotate button: a cube on a turntable, with an orbit arrow around its base. While
// the map turns, the cube turns with it (setSpinCube), so the button shows its state.
function turntableCube(deg) {
  const d = ((((deg + 45) % 90) + 90) % 90) - 45; // the view repeats every 90°
  const [right, front, left, back] = [0, 1, 2, 3].map((k) => {
    const a = ((d + 90 * k) * Math.PI) / 180;
    return [12 + 4.6 * Math.cos(a), 8 + 2.48 * Math.sin(a)]; // the top face's corners
  });
  const p = ([x, y], down = 0) => `${+x.toFixed(2)} ${+(y + down).toFixed(2)}`;
  return [
    `M${p(back)}L${p(right)}L${p(right, 5)}L${p(front, 5)}L${p(left, 5)}L${p(left)}Z`,
    `M${p(left)}L${p(front)}L${p(right)}M${p(front)}L${p(front, 5)}`,
  ];
}
{
  const [cube, edges] = turntableCube(0);
  ICONS.turntable = `<path class="tt-cube" d="${cube}"/><path class="tt-edges" d="${edges}"/>`
    + '<path d="M20.84 13.3A9.3 4.2 0 0 1 3.95 16.7"/><path class="solid" d="M1.19 14.54 5.3 14.97 2.59 18.43z"/>';
}

function icon(name) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.classList.add("i");
  svg.innerHTML = ICONS[name] || "";
  return svg;
}

// Elements in the page ask for an icon with data-icon="name".
function fillIcons(root = document) {
  for (const el of root.querySelectorAll("[data-icon]")) {
    if (!el.querySelector(":scope > svg.i")) el.prepend(icon(el.dataset.icon));
  }
}

// ---------- tooltips ----------

// One tooltip for everything with data-tip: "\n" starts a new line and the first line becomes
// the title. It shows on hover and on keyboard focus, and always stays inside the window.
const tipEl = $("tip");
let tipFor = null;
let tipTimer = null;
let tipFollows = false; // the tip of something wide (a row) sits at the cursor and follows it
let pointer = null; // where the pointer was last
const WIDE = 160; // wider than this (in pixels), a thing's tip goes to the cursor

function setTip(el, text) {
  el.dataset.tip = text;
  // Icon-only controls get the tooltip's title as their accessible name.
  if (el.matches("button, a, input, select") && !el.textContent.trim()) {
    el.setAttribute("aria-label", text.split("\n")[0]);
  }
  if (tipFor === el) showTip(el, tipFollows ? pointer : null);
}

// The rail's buttons: beside them. Something wide, pointed at: at the cursor. Anything else: above it.
function showTip(el, at = null) {
  const text = el.dataset.tip;
  if (!text || !el.isConnected) return hideTip();
  tipFor = el;
  const box = renderTip(text);
  const r = el.getBoundingClientRect();
  const wide = r.width > WIDE && !el.closest(".rail");
  tipFollows = wide && !!at;
  if (el.closest(".rail")) placeTip(r.right + 10, r.top + r.height / 2 - box.height / 2, box);
  else if (tipFollows) placeAtCursor(at, box);
  else if (wide) placeTip(r.left + 12, r.top - box.height - 8, box, r.bottom + 8); // (from the keyboard)
  else placeTip(r.left + r.width / 2 - box.width / 2, r.top - box.height - 8, box, r.bottom + 8);
}

const placeAtCursor = (at, box) => placeTip(at.x - box.width / 2, at.y - box.height - 14, box, at.y + 22);

// For things that aren't elements, like a point on the map: the tip sits above the cursor.
function showTipAt(x, y, text) {
  clearTimeout(tipTimer);
  tipFor = "point";
  tipFollows = false;
  const box = renderTip(text);
  placeTip(x - box.width / 2, y - box.height - 14, box, y + 20);
}

function placeTip(x, y, box, below = y) {
  // The visible page, without the scrollbar: a tip slides sideways rather than under it.
  const { clientWidth: w, clientHeight: h } = document.documentElement;
  if (y < 8) y = below;
  x = Math.max(8, Math.min(x, w - box.width - 8));
  y = Math.max(8, Math.min(y, h - box.height - 8));
  tipEl.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
}

function renderTip(text) {
  const [title, ...rest] = text.split("\n");
  // Lines with a tab ("keys\twhat they do") line up as a two-column list.
  const body = rest.some((line) => line.includes("\t"))
    ? [h("span", { class: "tip-grid" }, ...rest.flatMap((line) => {
      const [keys, what = ""] = line.split("\t");
      return [h("span", { class: "tip-keys" }, keys), h("span", {}, what)];
    }))]
    : rest.flatMap((line) => [h("br"), line]);
  tipEl.replaceChildren(rest.length ? h("strong", {}, title) : title, ...body);
  tipEl.hidden = false;
  return tipEl.getBoundingClientRect();
}

function hideTip() {
  clearTimeout(tipTimer);
  tipFor = null;
  tipFollows = false;
  tipEl.hidden = true;
}

// A tooltip that only repeats a name shows while the name is cut off (data-tip-cut: which child).
function tipWanted(el) {
  if (!el || !("tipCut" in el.dataset)) return el;
  const n = (el.dataset.tipCut && el.querySelector(el.dataset.tipCut)) || el;
  return n.scrollWidth > n.clientWidth ? el : null;
}

document.addEventListener("pointerover", (e) => {
  pointer = { x: e.clientX, y: e.clientY };
  const el = tipWanted(e.target.closest?.("[data-tip]") || null);
  if (el === tipFor) return;
  clearTimeout(tipTimer);
  if (!el) return hideTip();
  showTip(el, pointer);
});
let tipFrame = 0;
document.addEventListener("pointermove", (e) => {
  pointer = { x: e.clientX, y: e.clientY };
  if (!tipFollows || tipFrame) return;
  tipFrame = requestAnimationFrame(() => {
    tipFrame = 0;
    if (tipFollows && !tipEl.hidden) placeAtCursor(pointer, tipEl.getBoundingClientRect());
  });
}, { passive: true });
document.documentElement.addEventListener("pointerleave", hideTip);
document.addEventListener("pointerdown", hideTip);
document.addEventListener("focusin", (e) => {
  const el = tipWanted(e.target.closest?.("[data-tip]"));
  if (el && e.target.matches(":focus-visible")) showTip(el);
});
document.addEventListener("focusout", hideTip);
window.addEventListener("scroll", hideTip, true);

// ---------- translations ----------

const i18n = { catalog: { en: {} }, lang: "en", plural: new Intl.PluralRules("en") };

function t(key, params = {}) {
  const text = i18n.catalog[i18n.lang]?.[key] ?? i18n.catalog.en?.[key] ?? key;
  // Numbers (like "GB free" from the backend) are written the language's way: 0,6 in German.
  const value = (v) => (typeof v === "number" ? fmt(v, 1) : v);
  return text.replace(/\{(\w+)\}/g, (m, name) => (name in params ? value(params[name]) : m));
}

// Plural-aware: picks "key.one" or "key.other" for this language.
const tp = (key, n, params = {}) => t(`${key}.${i18n.plural.select(n) === "one" ? "one" : "other"}`, { n: fmt(n), ...params });

function setLanguage(lang) {
  if (!i18n.catalog[lang]) lang = "en";
  i18n.lang = lang;
  i18n.plural = new Intl.PluralRules(lang);
  document.documentElement.lang = lang;
  for (const el of document.querySelectorAll("[data-i18n]")) el.textContent = t(el.dataset.i18n);
  for (const el of document.querySelectorAll("[data-i18n-placeholder]")) el.placeholder = t(el.dataset.i18nPlaceholder);
  for (const el of document.querySelectorAll("[data-i18n-aria]")) el.setAttribute("aria-label", t(el.dataset.i18nAria));
  for (const el of document.querySelectorAll("[data-i18n-tip]")) setTip(el, t(el.dataset.i18nTip));
  for (const b of document.querySelectorAll("#kinds button, #map-kinds button")) {
    const k = b.dataset.kind;
    setTip(b, t(`search.kind.${k}`));
  }
  for (const b of $("views").querySelectorAll("button")) {
    setTip(b, t(`view.${b.dataset.view}`));
  }
  setTip($("nav-map"), t("nav.map"));
  if (map.help) renderExplain();
  renderSort();
  renderKeys();
  renderScopes();
  $("map-q").placeholder = `${t("nav.search")}…`;
  updateSpin();
  rotatePlaceholder();
  document.documentElement.classList.remove("i18n-pending");
}

// The keyboard shortcuts and the map's mouse controls, listed in Settings. A key is a string;
// a mouse action is { mouse: "map.drag" }.
function renderKeys() {
  const ctrl = t("keys.ctrl");
  const groups = [
    ["search", "nav.search", [
      [["/"], [ctrl, "K"], "keys.search"],
      [["↑"], ["↓"], "keys.pick"],
      [["Enter"], "keys.open"],
      [[ctrl, "Enter"], "keys.reveal"],
      [["Shift", "Enter"], [t("keys.space")], "keys.preview"],
      [["Esc"], "keys.clear"],
    ]],
    ["cube", "nav.map", [
      [[{ mouse: "map.drag" }], "map.rotate"],
      [[{ mouse: "map.scroll" }], "map.zoom"],
      [[{ mouse: "map.click" }], "map.inspect"],
      [["←", "→"], "map.back_short"],
      [[{ mouse: "map.dblclick" }], "map.reset_short"],
    ]],
  ];
  const key = (k) => (k.mouse ? h("kbd", { class: "mouse" }, icon("mouse"), t(k.mouse)) : h("kbd", {}, k));
  $("keys").replaceChildren(...groups.map(([name, title, rows]) => h("div", { class: "key-group" },
    h("div", { class: "key-head" }, icon(name), t(title)),
    ...rows.map((row) => {
      const label = row[row.length - 1];
      const combos = row.slice(0, -1).map((combo) => h("span", { class: "combo" }, ...combo.map(key)));
      return h("div", { class: "key-row" }, h("span", { class: "combos" }, ...combos), h("span", {}, t(label)));
    }))));
}

// Translate an error from the API: {key, params, message} or a plain string.
const apiMessage = (detail, fallback) =>
  detail && typeof detail === "object" ? t(detail.key, detail.params) : detail || fallback;

// ---------- helpers ----------

function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (k === "style") el.style.cssText = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

function iconButton(name, tip, onclick, cls = "") {
  const b = h("button", { type: "button", class: `icon-btn small ${cls}`, onclick }, icon(name));
  setTip(b, tip);
  return b;
}

async function api(path, opts = {}) {
  const res = await fetch(path, {
    ...opts,
    headers: { "x-quiekel-embed": "1", "content-type": "application/json", ...(opts.headers || {}) },
  });
  if (!res.ok) {
    let msg = res.statusText;
    try { msg = apiMessage((await res.json()).detail, msg); } catch {}
    throw new Error(msg);
  }
  return res.json();
}

const post = (path, body) => api(path, { method: "POST", body: body ? JSON.stringify(body) : undefined });

function toast(msg, name = null) {
  const el = $("toast");
  el.replaceChildren(...(name ? [icon(name)] : []), h("span", {}, msg));
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 3200);
}

const fmt = (n, digits = 0) => Number(n).toLocaleString(i18n.lang, { maximumFractionDigits: digits });
const pct = (v) => new Intl.NumberFormat(i18n.lang, { style: "percent", maximumFractionDigits: 0 }).format(v / 100);
const baseName = (p) => p.split(/[\\/]/).filter(Boolean).pop() || p;

function ago(ts) {
  if (!ts) return t("time.never");
  const s = Date.now() / 1000 - ts;
  if (s < 60) return t("time.just_now");
  const rtf = new Intl.RelativeTimeFormat(i18n.lang, { numeric: "auto" });
  if (s < 3600) return rtf.format(-Math.round(s / 60), "minute");
  if (s < 86400) return rtf.format(-Math.round(s / 3600), "hour");
  if (s < 86400 * 30) return rtf.format(-Math.round(s / 86400), "day");
  return new Date(ts * 1000).toLocaleDateString(i18n.lang);
}

function bytes(n) {
  if (n < 1024) return `${fmt(n)} B`;
  if (n < 1024 ** 2) return `${fmt(n / 1024)} KB`;
  if (n < 1024 ** 3) return `${fmt(n / 1024 ** 2, 1)} MB`;
  return `${fmt(n / 1024 ** 3, 1)} GB`;
}

function eta(s) {
  if (s == null) return "";
  if (s < 60) return t("eta.soon");
  if (s < 3600) return t("eta.min", { n: fmt(Math.round(s / 60)) });
  return t("eta.hours", { n: fmt(s / 3600, 1) });
}

function escapeRe(s) { return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); }

// Previews are cached by the browser, and file ids can be reused after a folder is
// removed and re-added, so the URL carries the file's version.
// (page: of a document, that page's picture)
const atPage = (page) => (page > 1 ? `&page=${page}` : "");
const thumbUrl = (r, page) => `/api/thumb/${r.file_id}?v=${Math.floor(r.mtime)}-${r.size}${atPage(page)}`;
const previewUrl = (r, page) => `/api/preview/${r.file_id}?v=${Math.floor(r.mtime)}-${r.size}${atPage(page)}`;
const pictureUrl = (r) => r.picture || thumbUrl(r); // (a pasted picture brings its own)

// Wrap query words in <mark>, building DOM nodes (never innerHTML with file content).
function highlighted(text, terms) {
  if (!terms.length || !text) return [text];
  const re = new RegExp(`(?<![\\p{L}\\p{N}])(${terms.map(escapeRe).join("|")})`, "giu");
  const out = [];
  let last = 0;
  for (const m of text.matchAll(re)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(h("mark", {}, m[0]));
    last = m.index + m[0].length;
  }
  out.push(text.slice(last));
  return out;
}

// Little words that would light up every passage ("the", "der", "les", "los"). They still count for
// the search; they just aren't highlighted, unless the query has nothing else.
const STOPWORDS = new Set((
  "a an and are as at be but by for from has have i in is it its me my of on or our so that the their this to "
  + "was we were what when where which who will with you your "
  + "aber als am auch auf aus bei bin bis das dass dem den der des die du ein eine einem einen einer es für hat "
  + "ich im ist mein meine mit nach nicht noch oder sie sind über um und uns von vor wie wir zu zum zur "
  + "au aux avec ce ces dans de du elle en est et il je la le les leur ma mais mes mon ne nous ou par pas pour "
  + "qu que qui sa se ses son sur ta te tu un une vos votre vous "
  + "al como con del el las lo los mi mis no para pero por su sus una uno y"
).split(" "));

function queryTerms(q) {
  const words = [...new Set((q.toLowerCase().match(/[\p{L}\p{N}]+/gu) || []).filter((w) => w.length > 1))];
  const content = words.filter((w) => !STOPWORDS.has(w));
  return content.length ? content : words;
}

// Release notes are Markdown: show them as tidy plain text (never as HTML).
function plainNotes(md) {
  return md
    .replace(/\r\n/g, "\n")
    .replace(/^#{1,6}\s*/gm, "")
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/__(.+?)__/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/^\s*[-*]\s+/gm, "• ")
    .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, "$1 ($2)")
    .trim();
}

// The search box shows example searches instead of an explanation.
let exampleAt = 0;
function rotatePlaceholder() {
  const examples = t("search.examples").split("|");
  $("q").placeholder = `${examples[exampleAt++ % examples.length]}…`;
}
setInterval(() => { if (!$("q").value && !state.similar) rotatePlaceholder(); }, 4500);

// ---------- pages ----------

function route() {
  const page = (location.hash.match(/^#\/(\w+)/) || [])[1];
  const pages = ["search", "map", "folders", "settings", "diagnostics"];
  state.page = pages.includes(page) && (page !== "diagnostics" || state.diagnostics) ? page : "search";
  for (const p of pages) $(`page-${p}`).hidden = p !== state.page;
  for (const a of document.querySelectorAll(".nav a")) {
    a.classList.toggle("active", a.dataset.page === state.page);
  }
  hideTip();
  closeScopeMenu();
  closeSortMenu();
  if (state.page === "search") $("q").focus();
  if (state.page === "map") openMap();
  if (state.page === "diagnostics") refreshDiagnostics();
  if (state.page === "folders" && state.last) {
    showProblemsBadge(state.last.folders); // seen: the badge goes
    renderGlance(state.last.folders);
  }
}

window.addEventListener("hashchange", route);

// A click on Quiekel: a little hop (and to Search, where the link goes).
$("brand").addEventListener("click", () => {
  const brand = $("brand");
  brand.classList.remove("hop");
  void brand.offsetWidth; // (from the start again when clicked mid-hop)
  brand.classList.add("hop");
});
$("brand").addEventListener("animationend", () => $("brand").classList.remove("hop"));

const goTo = (page) => { location.hash = `#/${page}`; };

// ---------- status polling ----------

async function poll() {
  let delay = 1000;
  try {
    const s = await api("/api/status");
    render(s);
    const p = s.progress;
    if (s.model.status === "ready" && p.state === "idle" && !p.queued) delay = 2000;
  } catch {
    setStatus({ kind: "bad", icon: "alert", lines: [t("activity.unreachable"), t("activity.unreachable_sub")] });
    delay = 3000;
  }
  if (document.hidden) delay = 10000; // window hidden in the tray: barely poll
  clearTimeout(pollTimer);
  pollTimer = setTimeout(poll, delay);
}
document.addEventListener("visibilitychange", () => { if (!document.hidden) poll(); });

const refresh = () => api("/api/status").then(render).catch(() => {});

function render(s) {
  // (a copy serving its page from a folder: when a file there changes, the page loads itself again)
  if (s.page) {
    if (state.pageStamp && s.page !== state.pageStamp) return location.reload();
    state.pageStamp = s.page;
  }
  state.last = s;
  state.appMode = !!s.app_mode;
  if (s.settings.language_resolved !== i18n.lang) {
    // The language changed: translate everything again.
    setLanguage(s.settings.language_resolved);
    state.foldersKey = state.settingsKey = state.deviceKey = "";
    if ($("q").value.trim() || state.similar) runSearch();
    else renderEmpty();
  }
  const phase = workPhase(s);
  renderActivity(s, phase);
  updateNotice();
  renderFolders(s.folders, s.progress, phase, s.removing || []);
  renderSettings(s);
  renderUpdate(s);
}

// ---------- status (bottom of the rail) ----------

// What the indexer is doing with a folder right now. One answer for the status ring, the
// folders list and the tray, so they never contradict each other.
function workPhase({ progress: p, resources }) {
  if (p.state !== "indexing" && p.state !== "scanning") return null;
  if (p.paused) return "paused"; // you pressed pause
  if (resources.decision.duty === 0 || p.note) return "waiting"; // memory running out
  return p.state;
}

const RING = 2 * Math.PI * 16;

// The ring: a colour for the state, an icon (or the percentage) inside, details in its tooltip.
// The ring in the rail: what indexing is doing, and (when there is something to pause or continue)
// the button for it. Pointing at it shows what a click does.
function setStatus({ kind = "", icon: name = null, label = null, frac = null, spin = false, lines, onclick = null, action = null }) {
  const el = $("status");
  el.className = `status ${kind}${spin ? " spin" : ""}${onclick ? " clickable" : ""}`;
  const shown = spin ? 0.28 : frac ?? 0;
  const arc = $("status-arc");
  arc.style.strokeDasharray = `${(shown * RING).toFixed(1)} ${RING.toFixed(1)}`;
  arc.style.opacity = shown > 0 ? 1 : 0; // a round cap would draw a dot at 0 %
  const inner = $("status-inner");
  const key = `${label ?? `icon:${name}`}|${action ?? ""}`;
  if (inner.dataset.key !== key) {
    inner.dataset.key = key;
    inner.replaceChildren(h("span", { class: "status-now" }, label ?? icon(name)),
      ...(action ? [h("span", { class: "status-do" }, icon(action))] : []));
  }
  setTip(el, lines.filter(Boolean).join("\n"));
  state.statusClick = onclick;
}

$("status").addEventListener("click", () => state.statusClick?.());

function renderActivity({ model, progress: p, folders, resources, settings }, phase) {
  const wasReady = state.ready;
  state.ready = model.status === "ready";
  const resting = model.status === "asleep";
  const d = resources.decision;
  const total = folders.reduce((a, f) => a + f.indexed, 0);
  let s;

  if (model.status === "downloading") {
    s = { kind: "busy", icon: "download", spin: true, lines: [t("activity.downloading"), t("activity.downloading_sub", { gb: model.download_gb ?? 1.5 })] };
  } else if (model.status !== "ready" && model.status !== "error" && !resting) {
    s = { kind: "busy", icon: "timer", lines: [t("activity.waking"), t("activity.waking_sub")] };
  } else if (model.status === "error" || p.state === "model-error") {
    s = {
      kind: "bad", icon: "alert", lines: [t("activity.model_error"), model.error || p.message, t("activity.click_retry")],
      onclick: () => post("/api/retry-model").catch((e) => toast(e.message, "alert")),
    };
  } else if (phase) {
    const lines = [t(`activity.${phase}`), baseName(p.folder || "")];
    let frac = null;
    if (p.state === "scanning") {
      lines.push(tp("activity.found", p.found || 0));
    } else {
      frac = p.total ? p.done / p.total : 0;
      const parts = [t("activity.progress", { done: fmt(p.done), total: fmt(p.total) })];
      if (phase === "indexing" && p.files_per_min) parts.push(t("activity.rate", { n: fmt(Math.round(p.files_per_min)) }));
      if (phase === "indexing" && p.eta_s != null) parts.push(eta(p.eta_s));
      lines.push(parts.join(" · "));
    }
    s = {
      kind: { paused: "paused", waiting: "warn", scanning: "busy", indexing: "busy" }[phase],
      icon: { paused: "pause", waiting: "timer", scanning: "search" }[phase],
      label: phase === "indexing" ? pct(Math.floor(frac * 100)) : null,
      frac, spin: phase === "scanning", lines,
    };
  } else if (p.state === "error") {
    s = { kind: "warn", icon: "alert", lines: [t("activity.error"), p.message] };
  } else if (!folders.length) {
    s = { icon: "folderPlus", lines: [t("activity.no_folders"), t("activity.no_folders_sub")], onclick: () => goTo("folders") };
  } else if (p.paused) {
    s = { kind: "paused", icon: "pause", lines: [t("activity.paused"), tp("activity.ready", total)] };
  } else {
    s = { kind: "ok", icon: "check", frac: 1, lines: [t("activity.idle"), tp("activity.ready", total)] };
  }
  if (resting) s.lines.push(t("activity.model_resting"));
  const busy = state.ready && (phase || p.paused || p.queued);
  if (busy && !p.paused) { // paused by you: "Paused" says it all
    const why = p.note ? t(p.note.key, p.note.params) : d.level !== "full" ? t(`gov.${d.code}`, d.params) : "";
    if (why) s.lines.push(why);
  }
  if (busy) { // the ring itself pauses and continues
    s.action = p.paused ? "play" : "pause";
    s.onclick = () => post(p.paused ? "/api/resume" : "/api/pause").then(refresh).catch((e) => toast(e.message, "alert"));
    s.lines.push(t(p.paused ? "activity.click_resume" : "activity.click_pause"));
  }

  setStatus(s);

  if (!wasReady && state.ready && $("q").value.trim()) runSearch();
}


// ---------- folders ----------

// The folders badge says only that something new needs a look (files that couldn't be read), and
// goes away once the folders were looked at. How many problems were seen is a setting: it stays
// seen after a restart.
let problemsSeen = null; // (from the settings; ahead of them right after looking)

function showProblemsBadge(folders) {
  const problems = folders.reduce((a, f) => a + f.errors, 0);
  const saved = state.last?.settings.problems_seen;
  if (problemsSeen === null && saved != null) problemsSeen = saved;
  if ((state.page === "folders" || problems < (problemsSeen ?? 0)) && problems !== problemsSeen) {
    problemsSeen = problems;
    post("/api/settings", { problems_seen: problems }).catch(() => {});
  }
  const badge = $("nav-badge");
  const show = problems > (problemsSeen ?? 0);
  if (show === !badge.hidden) return;
  badge.hidden = !show;
  badge.replaceChildren(show ? icon("bang") : "");
  setTip(badge.parentElement, show ? `${t("nav.folders")}: ${t("nav.problems")}` : t("nav.folders"));
}

const PHASE_STAT = {
  paused: ["pause", "activity.paused"],
  waiting: ["hourglass", "activity.waiting"],
  scanning: ["search", "activity.scanning"],
  indexing: ["refresh", "folders.indexing"],
};

function stat(name, text, tip, cls = "", onclick = null) {
  const el = h("span", { class: `stat ${cls}`, onclick }, icon(name), text);
  setTip(el, tip);
  return el;
}

// The folder being worked on: a small ring that fills up as it's indexed (and turns while files
// are still being looked for); paused or waiting, its icon.
function phaseStat(phase, p) {
  if (phase !== "indexing" && phase !== "scanning") {
    return stat(PHASE_STAT[phase][0], "", t(PHASE_STAT[phase][1]), `phase ${phase}`);
  }
  const share = phase === "indexing" && p.total ? Math.min(1, (p.done || 0) / p.total) : null;
  const ring = h("span", { class: `ring-mini${share == null ? " busy" : ""}` });
  ring.innerHTML = '<svg viewBox="0 0 20 20" aria-hidden="true"><circle class="t" cx="10" cy="10" r="7.5"/>'
    + `<circle class="a" cx="10" cy="10" r="7.5" pathLength="100" stroke-dasharray="${share == null ? 28 : Math.max(2, share * 100)} 100"/></svg>`;
  const el = h("span", { class: "stat phase" }, ring, share == null ? "" : pct(share * 100));
  setTip(el, t(PHASE_STAT[phase][1]));
  return el;
}

function renderFolders(folders, progress, phase, removing = []) {
  // Folders on their way out (removed here, or still going after a restart): their cards stay,
  // emptying as their files leave the index, and go once the folder is gone.
  const going = state.removing;
  const listed = new Set(removing.map((r) => r.id));
  for (const r of removing) {
    const e = going.get(r.id) || { path: r.path, total: Math.max(1, r.left), sent: true };
    e.left = r.left;
    going.set(r.id, e);
  }
  for (const [id, e] of going) if (e.sent && !listed.has(id)) going.delete(id); // (gone)
  folders = folders.filter((f) => !going.has(f.id));
  const active = phase ? progress.folder : null;
  const key = JSON.stringify([folders, [...going].map(([id, e]) => [id, e.left]), active, phase, progress.done,
    progress.total, Math.floor(Date.now() / 60000)]);
  const hadFolders = state.hasFolders;
  state.hasFolders = folders.length > 0;
  showProblemsBadge(folders);
  if (hadFolders !== state.hasFolders && !$("q").value.trim() && !state.similar) renderEmpty();
  if (key === state.foldersKey) return;
  state.foldersKey = key;
  const open = !$("folder-modal").hidden && folders.find((f) => f.id === state.folderInfo);
  if (open) renderFolderSummary(open); // (its numbers follow, as files come and go)

  // The rows stay the same elements while their numbers change (a click that lands while one is
  // drawn anew would be lost); only what changed is drawn again.
  const list = $("folders");
  if (list.dataset.lang !== i18n.lang) {
    list.replaceChildren();
    list.dataset.lang = i18n.lang;
  }
  const rows = new Map([...list.querySelectorAll(":scope > .folder:not(.gone)")].map((el) => [el.dataset.id, el]));
  // (in the order of their paths, so a card being removed keeps its place)
  const cards = [...folders.map((f) => ({ path: f.path, id: f.id, f })), ...[...going].map(([id, e]) => ({ path: e.path, id, e }))]
    .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  const wanted = cards.map((c) => {
    const row = rows.get(String(c.id)) || folderRow();
    rows.delete(String(c.id));
    if (c.f) fillFolderRow(row, c.f, active, phase, progress);
    else fillRemovingRow(row, c.id, c.e);
    return row;
  });
  for (const gone of rows.values()) leaveRow(gone);
  list.querySelector(":scope > .empty")?.remove();
  // Where the next folder would go: a click picks one. With none yet, it stands alone in the middle
  // of the page, the one sentence under it.
  let slot = list.querySelector(":scope > .add-slot");
  if (!slot) {
    slot = h("button", { type: "button", class: "add-slot", onclick: () => pickFolder() }, icon("folderPlus"));
    setTip(slot, t("folders.add"));
  }
  const none = !cards.length;
  list.classList.toggle("none", none);
  wanted.push(slot);
  if (none) wanted.push(h("div", { class: "empty compact" }, waitingText(false)));
  wanted.forEach((el, i) => { if (list.children[i] !== el) list.insertBefore(el, list.children[i] || null); });
  renderGlance(folders);
  // A folder that was searched in is gone: search everywhere again.
  if (state.scope && !folders.some((f) => state.scope.prefix.startsWith(`${f.path.toLowerCase().replace(/[\\/]+$/, "")}\\`))) {
    setScope(null);
  }
}

// A folder, as a card: the ring of its kinds, its name and place; its numbers and buttons under them.
// A click on it shows what's in it: its numbers, what's left out, what couldn't be read.
function folderRow() {
  const row = h("div", { class: "folder" });
  const main = h("button", { type: "button", class: "folder-main", onclick: () => openFolderInfo(row.folder) },
    h("span", { class: "folder-ring" }),
    h("span", { class: "folder-text" }, h("span", { class: "folder-name" }), h("span", { class: "folder-path" })));
  setTip(main, t("folder.details"));
  row.append(main, h("div", { class: "folder-side" }, h("div", { class: "stats" }), h("div", { class: "folder-actions" })));
  return row;
}

function fillFolderRow(row, f, active, phase, progress) {
  row.folder = f;
  row.dataset.id = f.id;
  row.querySelector(".folder-main").disabled = false;
  const here = active === f.path;
  const working = here && phase !== "paused"; // (everything paused: the rail turns it back on)
  row.className = `folder${here ? ` active ${phase}` : ""}${f.missing ? " missing" : ""}`;
  const name = row.querySelector(".folder-name");
  const path = row.querySelector(".folder-path");
  if (name.textContent !== baseName(f.path)) name.textContent = baseName(f.path);
  if (path.textContent !== f.path) path.textContent = f.path;
  const parts = kindParts((key) => f[key] || 0);
  const ring = row.querySelector(".folder-ring");
  const ringKey = JSON.stringify([parts.map((p) => p.n), i18n.lang]);
  if (ring.dataset.key !== ringKey) {
    ring.dataset.key = ringKey;
    ring.replaceChildren(donut(parts, 44));
  }
  row.querySelector(".stats").replaceChildren(...[
    f.missing ? stat("alert", t("folder.missing"), t("folder.missing_d"), "bad", () => locateFolder(row.folder)) : null,
    stat("doc", fmt(f.indexed), t("folder.ready_long")),
    stat("archive", bytes(f.bytes || 0), t("folder.size")),
    f.errors ? stat("alert", fmt(f.errors), tp("folders.unreadable", f.errors), "bad", () => openFolderInfo(row.folder)) : null,
    f.paused ? stat("pause", "", t("activity.paused"), "phase paused")
      : here ? phaseStat(phase, progress) : stat("clock", ago(f.last_scan_at), t("folders.col.scanned")),
  ].filter(Boolean));
  // Its buttons are drawn again only when they change.
  const actions = row.querySelector(".folder-actions");
  const key = JSON.stringify([!!f.missing, !!f.paused, working]);
  if (actions.dataset.key === key) return;
  actions.dataset.key = key;
  actions.replaceChildren(...[
    f.missing ? iconButton("folder", t("folder.locate"), () => locateFolder(row.folder), "primary") : null,
    // Pausing a folder here holds just this one; the others go on (the rail pauses everything).
    f.paused || working ? iconButton(f.paused ? "play" : "pause", t(f.paused ? "activity.resume" : "activity.pause"),
      () => post(`/api/folders/${f.id}/${f.paused ? "resume" : "pause"}`).then(refresh).catch((e) => toast(e.message, "alert")),
      f.paused ? "primary" : "") : null,
    iconButton("trash", t("folders.remove_title"), () => removeFolder(row.folder), "danger"),
  ].filter(Boolean));
}

// A folder on its way out: dimmed, without buttons, its ring the share of its files still in the
// index (emptying as they leave), their number beside the bin.
function fillRemovingRow(row, id, e) {
  row.folder = null;
  row.dataset.id = id;
  row.className = "folder removing";
  const name = row.querySelector(".folder-name");
  const path = row.querySelector(".folder-path");
  if (name.textContent !== baseName(e.path)) name.textContent = baseName(e.path);
  if (path.textContent !== e.path) path.textContent = e.path;
  row.querySelector(".folder-main").disabled = true;
  const ring = row.querySelector(".folder-ring");
  if (ring.dataset.key !== "removing") {
    ring.dataset.key = "removing";
    ring.replaceChildren(drainRing(44));
  }
  const share = Math.max(0, Math.min(100, (e.left / Math.max(e.total, e.left, 1)) * 100));
  ring.querySelector(".drain").style.strokeDasharray = `${share.toFixed(2)} ${(100 - share).toFixed(2)}`;
  row.querySelector(".stats").replaceChildren(stat("trash", fmt(e.left), t("folders.removing")));
  const actions = row.querySelector(".folder-actions");
  if (actions.dataset.key !== "removing") {
    actions.dataset.key = "removing";
    actions.replaceChildren();
  }
}

// A ring of one share that can change smoothly (a folder's files still to remove).
function drainRing(size) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 100 100");
  svg.classList.add("donut-svg");
  for (const cls of ["track", "part drain"]) {
    const c = document.createElementNS(ns, "circle");
    c.setAttribute("cx", 50); c.setAttribute("cy", 50); c.setAttribute("r", 42); c.setAttribute("pathLength", 100);
    c.setAttribute("class", cls);
    svg.append(c);
  }
  return h("span", { class: "donut", style: `--size: ${size}px` }, svg);
}

// A card whose folder is gone: a removed one fades out where it was, any other goes at once.
function leaveRow(row) {
  if (!row.classList.contains("removing")) return row.remove();
  if (row.classList.contains("gone")) return;
  row.classList.add("gone");
  row.addEventListener("animationend", () => row.remove(), { once: true });
  setTimeout(() => row.remove(), 800); // (also without animations)
}

// Reasons the indexer knows ("!damaged_image") come as keys; anything else as it was reported.
const problemText = (error) => (error?.startsWith("!") ? t(`problem.${error.slice(1)}`) : error);

// ---------- a folder's details ----------
//
// Its numbers, the files that couldn't be read, and everything that isn't searched, by reason. A
// new unreadable file shows as a red mark on the folder once: looking here counts as seen. "Try
// again" reads them anew (a rescan does too).

const WHY_ICON = {
  left_out: "eyeOff", type: "help", off: "eyeOff", media: "film", program: "app", mailbox: "mail", hidden: "eyeOff",
  office_temp: "hourglass", backup: "clock", program_folder: "folder", link: "link", too_many: "archive", encrypted: "lock",
  split_archive: "archive", needs_windows: "monitor", nested_deep: "archive", nested_off: "archive",
  nested_large: "archive", odd_name: "alert", inline_image: "image", empty: "doc", too_large: "maximize",
  tiny_image: "image", binary: "code", no_text: "text", password: "lock", not_image: "archive",
};
// What the scan couldn't open goes with the files that couldn't be read, said of each one.
const SCAN_FAILED = { no_access_dir: "problem.no_access", unreadable_dir: "problem.unreadable_dir",
  damaged_archive: "problem.damaged_archive" };
// A file the scan left out, by where its details name it (the server knows the path).
const revealExample = (reason, index) => post(`/api/folders/${state.folderInfo}/reveal`, { reason, index })
  .catch((e) => toast(e.message, "alert"));

function fileRow(path, why, reveal, isNew = false) {
  const name = h("b", {}, baseName(path));
  setTip(name, path);
  return h("div", { class: `fm-file${isNew ? " new" : ""}` },
    h("span", { class: "fm-file-text" }, name, why ? h("span", { class: "fm-why" }, why) : null),
    iconButton("folder", t("result.reveal"), reveal, "small"));
}

// Reasons that Settings → File types can change: their row has a way there.
const CHANGEABLE = new Set(["off", "hidden", "program_folder", "too_many", "nested_off"]);
const known = (key) => key in (i18n.catalog.en || {});

// One reason: what it is and how many; opened, which types and a few of the files.
function reasonRow(r) {
  const label = r.key ? t(`why.${r.key}`) : r.raw;
  const more = r.n - r.files.length;
  const exts = Object.entries(r.exts || {}).sort((a, b) => b[1] - a[1]).slice(0, 40)
    .map(([ext, n]) => h("span", { class: "type-chip static" }, ext || t("why.no_ext"), h("small", {}, fmt(n))));
  const summary = h("summary", {}, icon(WHY_ICON[r.key] || "skip"),
    h("span", { class: "fm-file-text" }, h("b", {}, label)),
    CHANGEABLE.has(r.key) ? iconButton("settings", t("settings.types"), (e) => {
      e.preventDefault();
      closeFolderInfo();
      openTypes();
    }) : null,
    h("span", { class: "fm-count" }, fmt(r.n)), icon("chevronDown"));
  if (r.key && known(`why.${r.key}_d`)) setTip(summary, t(`why.${r.key}_d`));
  return h("details", { class: "fm-reason" }, summary,
    h("div", { class: "fm-reason-body" },
      exts.length ? h("div", { class: "tm-chips" }, ...exts) : null,
      ...r.files.map((file, i) => fileRow(file.path, "",
        () => (file.id ? revealFile({ file_id: file.id }) : revealExample(r.key, i)))),
      more > 0 ? h("p", { class: "help-note" }, t("why.more", { n: fmt(more) })) : null));
}

// The folder at a glance: how many of its files can be searched, one bar for what's what, and a
// few numbers.
function renderFolderSummary(f) {
  const parts = [["ok", f.indexed, "folder.ready"], ["wait", f.pending, "folder.waiting"],
    ["skip", f.unsearched, "folder.unsearched"], ["bad", f.unreadable, "folder.unreadable"]].filter(([, n]) => n > 0);
  const when = h("span", { class: "fm-when" }, icon("clock"), ago(f.last_scan_at));
  setTip(when, t("folders.col.scanned"));
  $("fm-summary").replaceChildren(
    h("div", { class: "fm-headline" }, h("div", {}, h("b", {}, fmt(f.indexed)), h("span", {}, t("folder.ready_long"))), when),
    h("div", { class: "fm-bar" }, ...parts.map(([cls, n]) => h("i", { class: cls, style: `flex-grow: ${n}` }))),
    h("div", { class: "fm-legend" }, ...parts.map(([cls, n, key]) => h("span", { class: cls }, h("i"), h("b", {}, fmt(n)), " ", t(key)))),
  );
  const cell = (ic, key, value) => h("div", { class: "fm-stat" }, h("span", {}, icon(ic), t(key)), h("b", {}, value));
  $("fm-numbers").replaceChildren(
    cell("doc", "folder.docs", fmt(Math.max(0, f.indexed - f.images))),
    cell("image", "folders.col.images", fmt(f.images)),
    cell("archive", "folder.size", bytes(f.bytes || 0)),
  );
}

async function openFolderInfo(f) {
  hideTip();
  state.folderInfo = f.id;
  $("fm-name").textContent = baseName(f.path);
  $("fm-path").textContent = f.path;
  renderFolderSummary(f);
  $("fm-actions").replaceChildren(...[
    f.missing ? iconButton("folder", t("folder.locate"), () => locateFolder(f), "primary") : null,
    iconButton(f.watch ? "eye" : "eyeOff", t("folders.watch_title"),
      () => post(`/api/folders/${f.id}/watch`, { watch: !f.watch }).then(() => { f.watch = !f.watch; openFolderInfo(f); refresh(); }),
      f.watch ? "on" : ""),
    iconButton("refresh", t("folders.rescan_title"),
      () => post(`/api/folders/${f.id}/rescan`).then(() => { toast(t("folders.rescan_queued"), "refresh"); refresh(); })),
  ].filter(Boolean));
  $("fm-problems").hidden = $("fm-skips").hidden = $("fm-inside").hidden = true;
  $("fm-list").replaceChildren();
  $("fm-reasons").replaceChildren();
  $("fm-subfolders").replaceChildren();
  $("folder-modal").hidden = false;
  let d;
  try {
    d = await api(`/api/folders/${f.id}/details`);
  } catch (e) {
    toast(e.message, "alert");
    return;
  }
  if (state.folderInfo !== f.id) return;
  renderSubfolders(f, d.left_out || []);
  const failed = [
    ...d.errors.map((r) => fileRow(r.path, problemText(r.error), () => revealFile({ file_id: r.id }), !r.seen)),
    ...Object.entries(d.report).filter(([key]) => key in SCAN_FAILED)
      .flatMap(([key, r]) => r.examples.map((path, i) => fileRow(path, t(SCAN_FAILED[key]), () => revealExample(key, i)))),
  ];
  $("fm-problems").hidden = !failed.length;
  $("fm-list").replaceChildren(...failed);
  const reasons = [
    ...Object.entries(d.report).filter(([key]) => !(key in SCAN_FAILED))
      .map(([key, r]) => ({ key, n: r.n, exts: r.exts, files: r.examples.map((path) => ({ path })) })),
    ...d.skipped.map((s) => ({ key: s.reason?.startsWith("!") ? s.reason.slice(1) : null, raw: s.reason, n: s.n,
      files: s.examples })),
  ].sort((a, b) => b.n - a.n);
  $("fm-skips").hidden = !reasons.length;
  $("fm-reasons").replaceChildren(...reasons.map(reasonRow));
  if (f.errors) post(`/api/folders/${f.id}/errors/seen`).then(refresh).catch(() => {}); // the red mark has done its job
}

// The folders inside one, each with a switch: on, it's searched; off, you left it out (its files
// leave the index, scans pass it by). Folders left out deeper inside are listed too.
async function renderSubfolders(f, leftOut) {
  let dirs = [];
  try {
    dirs = (await api(`/api/dirs?${new URLSearchParams({ path: f.path })}`)).dirs;
  } catch { /* none to show */ }
  if (state.folderInfo !== f.id) return;
  const out = new Set(leftOut.map((p) => p.toLowerCase()));
  const shown = new Set(dirs.map((d) => d.path.toLowerCase()));
  const rows = [...dirs.map((d) => ({ path: d.path, name: d.name, count: d.count, kind: d.kind })),
    ...leftOut.filter((p) => !shown.has(p.toLowerCase())).map((p) => ({ path: p, name: p.slice(f.path.replace(/[\\/]+$/, "").length + 1), count: null }))]
    .sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true }));
  const toggle = async (path, isOut) => {
    try {
      await post(`/api/folders/${f.id}/leave-out`, { path, out: !isOut });
      const d = await api(`/api/folders/${f.id}/details`);
      renderSubfolders(f, d.left_out || []);
      refresh();
    } catch (e) { toast(e.message, "alert"); }
  };
  $("fm-subfolders").replaceChildren(...rows.map((r) => {
    const isOut = out.has(r.path.toLowerCase());
    const name = h("b", {}, r.name);
    setTip(name, r.path);
    const sw = h("input", { type: "checkbox", class: "switch", "aria-label": r.name });
    sw.checked = !isOut;
    sw.addEventListener("change", () => toggle(r.path, isOut));
    setTip(sw, t(isOut ? "folder.take_back" : "folder.leave_out"));
    return h("div", { class: `fm-file fm-sub${isOut ? " left-out" : ""}` },
      icon(DIR_ICON[r.kind] || "folder"), h("span", { class: "fm-sub-name" }, name),
      r.count != null && !isOut ? h("span", { class: "fm-count" }, fmt(r.count)) : null, sw);
  }));
  $("fm-inside").hidden = !rows.length;
}

function closeFolderInfo() {
  $("folder-modal").hidden = true;
  state.folderInfo = null;
}

$("fm-close").addEventListener("click", closeFolderInfo);
$("folder-modal").addEventListener("click", (e) => { if (e.target.id === "folder-modal") closeFolderInfo(); });
$("fm-retry").addEventListener("click", () => {
  const id = state.folderInfo;
  post(`/api/folders/${id}/rescan`).then(() => { toast(t("folder.retrying"), "refresh"); closeFolderInfo(); refresh(); })
    .catch((e) => toast(e.message, "alert"));
});

// ---------- Settings → File types ----------
//
// What's searched, by kind of file: a switch for each. And the rules for hidden files and program
// folders.

const TYPE_KINDS = [["documents", "doc"], ["pictures", "image"], ["text", "text"], ["mail", "mail"], ["archives", "archive"]];
const TYPE_RULES = [["hidden_files", "eyeOff"], ["program_folders", "folder"]];

async function openTypes() {
  hideTip();
  state.typesSeen = {};
  try {
    state.types = await api("/api/filetypes");
  } catch (e) {
    toast(e.message, "alert");
    return;
  }
  renderTypes();
  $("types-modal").hidden = false;
}

const closeTypes = () => { $("types-modal").hidden = true; };

function renderTypes() {
  const m = state.types;
  const off = m.categories.filter((c) => !c.on).map((c) => c.key);
  const formatsOff = m.categories.flatMap((c) => (c.formats || []).filter((f) => !f.on).map((f) => f.ext));
  const row = (ic, label, tip, checked, change, more = null) => {
    const sw = h("input", { type: "checkbox", class: "switch", "aria-label": label });
    sw.checked = checked;
    sw.addEventListener("change", () => change(sw.checked));
    const r = h("label", { class: "tm-rule" }, icon(ic), h("span", { class: "fm-file-text" }, h("b", {}, label)), more, sw);
    if (tip) setTip(r, tip);
    return r;
  };
  // A kind's formats, for the precise: folded away behind its ›; each one switches on its own. A
  // format stays where it is while the dialog is open, even before its files are back.
  const formatsOf = (c) => {
    const seen = new Map((state.typesSeen?.[c.key] || []).map((f) => [f.ext, { ...f, n: 0, on: !formatsOff.includes(f.ext) }]));
    for (const f of c.formats || []) seen.set(f.ext, f);
    const list = [...seen.values()].sort((a, b) => a.ext.localeCompare(b.ext));
    if (state.typesSeen) state.typesSeen[c.key] = list;
    const chips = list.map((f) => {
      const chip = h("button", { type: "button", class: `type-chip${f.on ? "" : " off"}`,
        onclick: () => saveTypes({ formats_off: f.on ? [...formatsOff, f.ext] : formatsOff.filter((x) => x !== f.ext) }) },
        f.ext.slice(1), f.on && f.n ? h("small", {}, fmt(f.n)) : null);
      setTip(chip, t(f.on ? "folder.leave_out" : "folder.take_back"));
      return chip;
    });
    return h("div", { class: "tm-formats", hidden: !state.typesOpen?.has(c.key) }, ...chips);
  };
  const kinds = TYPE_KINDS.flatMap(([key, ic]) => {
    const c = m.categories.find((x) => x.key === key) || { formats: [] };
    const formats = formatsOf(c);
    const open = (c.formats || []).length
      ? iconButton(state.typesOpen?.has(key) ? "chevronUp" : "chevronDown", t("types.formats"), (e) => {
        e.preventDefault();
        state.typesOpen = state.typesOpen || new Set();
        state.typesOpen.has(key) ? state.typesOpen.delete(key) : state.typesOpen.add(key);
        renderTypes();
      }, "tm-more")
      : null;
    return [row(ic, t(`types.cat.${key}`), t(`types.cat.${key}_d`), !off.includes(key),
      (on) => saveTypes({ kinds_off: on ? off.filter((k) => k !== key) : [...off, key] }), open), formats];
  });
  const rules = TYPE_RULES.map(([key, ic]) => row(ic, t(`types.rule.${key}`),
    known(`types.rule.${key}_d`) ? t(`types.rule.${key}_d`) : "", !!m.rules[key], (on) => saveTypes({ [key]: on })));
  $("tm-body").replaceChildren(
    h("section", { class: "tm-group" }, h("div", { class: "tm-rules" }, ...kinds)),
    h("section", { class: "tm-group" }, h("div", { class: "tm-head" }, icon("settings"), h("b", {}, t("types.rules_title"))),
      h("div", { class: "tm-rules" }, ...rules)));
  $("tm-reset").disabled = !off.length && !formatsOff.length && TYPE_RULES.every(([key]) => m.rules[key] === m.defaults[key]);
}

async function saveTypes(change) {
  try {
    await post("/api/settings", change);
    state.types = await api("/api/filetypes");
    renderTypes();
    state.settingsKey = "";
    refresh();
  } catch (e) { toast(e.message, "alert"); }
}

$("types-open").addEventListener("click", openTypes);
$("tm-close").addEventListener("click", closeTypes);
$("types-modal").addEventListener("click", (e) => { if (e.target.id === "types-modal") closeTypes(); });
$("tm-reset").addEventListener("click", () => saveTypes({ kinds_off: [], formats_off: [], ...state.types.defaults }));

async function removeFolder(f) {
  const yes = await confirmDialog({
    title: t("folders.confirm_title", { name: baseName(f.path) }), text: t("folders.confirm_text"), ok: t("folders.forget"),
  });
  if (!yes || state.removing.has(f.id)) return;
  // Its card turns into the removal at once (nothing left to click twice) and stays until the last of
  // its files has left the index.
  const going = { path: f.path, total: Math.max(1, f.files || 0), left: f.files || 0, sent: false };
  state.removing.set(f.id, going);
  redrawFolders();
  try {
    await api(`/api/folders/${f.id}`, { method: "DELETE" });
    going.sent = true;
  } catch (e) {
    state.removing.delete(f.id);
    toast(e.message, "alert");
  }
  refresh();
}

// The folders drawn again from the last status (after something changed here).
function redrawFolders() {
  const s = state.last;
  if (!s) return;
  state.foldersKey = "";
  renderFolders(s.folders, s.progress, workPhase(s), s.removing || []);
}

async function addFolder(path) {
  $("folder-error").hidden = true;
  try {
    const f = await post("/api/folders", { path });
    const inside = f.took_over?.length || 0;
    toast(inside ? tp("folders.took_over", inside, { name: baseName(f.path) }) : t("folders.added", { name: baseName(f.path) }), "folderPlus");
    refresh();
  } catch (e) {
    goTo("folders");
    $("folder-error").textContent = e.message;
    $("folder-error").hidden = false;
  }
}

// A folder that moved (renamed, another drive letter): pointed at its new place, its index stays.
async function locateFolder(f) {
  try {
    const { path } = await post("/api/folders/pick");
    if (!path) return;
    await post(`/api/folders/${f.id}/locate`, { path });
    toast(t("folders.located", { name: baseName(path) }), "folder");
    closeFolderInfo();
    refresh();
  } catch (e) {
    toast(e.message, "alert");
  }
}

// What kind the files are, as File types counts them, in the colours of the kinds: a folder's (or
// all folders') numbers as the parts of a ring, what's still waiting in grey.
const KIND_PARTS = [["docs", "doc", "--kind-docs", "documents"], ["texts", "text", "--kind-text", "text"],
  ["mails", "mail", "--kind-mail", "mail"], ["images", "image", "--kind-images", "pictures"]];

function kindParts(count) {
  const parts = KIND_PARTS.map(([key, ic, color, cat]) => ({ ic, n: count(key), color: `var(${color})`, name: t(`types.cat.${cat}`) }));
  parts.push({ ic: "hourglass", n: count("pending"), color: "var(--border-strong)", name: t("folder.waiting") });
  return parts;
}

// A ring of parts ({ n, color, name }), clockwise from the top with a little gap between them, and
// what's in its middle, if anything. Each part says what it is when pointed at.
function donut(parts, size, mid = null) {
  const shown = parts.filter((p) => p.n > 0);
  const total = shown.reduce((s, p) => s + p.n, 0);
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 100 100");
  svg.classList.add("donut-svg");
  const circle = (cls) => {
    const c = document.createElementNS(ns, "circle");
    c.setAttribute("cx", 50); c.setAttribute("cy", 50); c.setAttribute("r", 42); c.setAttribute("pathLength", 100);
    c.setAttribute("class", cls);
    return c;
  };
  svg.append(circle("track"));
  const gap = shown.length > 1 ? 1.5 : 0;
  let at = 0;
  for (const p of shown) {
    const share = (p.n / total) * 100;
    const len = Math.max(0.8, share - gap);
    const c = circle("part");
    c.setAttribute("stroke-dasharray", `${len.toFixed(2)} ${(100 - len).toFixed(2)}`);
    c.setAttribute("stroke-dashoffset", (-at).toFixed(2));
    c.style.stroke = p.color;
    setTip(c, `${p.name}\n${fmt(p.n)} · ${pct(Math.round(share))}`);
    svg.append(c);
    at += share;
  }
  return h("span", { class: "donut", style: `--size: ${size}px` }, svg, mid);
}

// The index at a glance: the ring of all folders with the files ready in its middle; beside it,
// each kind's number and a few facts.
function renderGlance(folders) {
  const box = $("glance");
  if (!folders.length) return box.replaceChildren();
  const sum = (key) => folders.reduce((n, f) => n + (f[key] || 0), 0);
  const parts = kindParts(sum);
  const key = JSON.stringify([parts.map((p) => p.n), sum("bytes"), Math.max(...folders.map((f) => f.last_scan_at || 0)), i18n.lang,
    Math.floor(Date.now() / 60000)]);
  if (box.dataset.key === key) return; // (unchanged: the ring stays as it is)
  box.dataset.key = key;
  const legend = h("div", { class: "glance-legend" }, ...parts.filter((p) => p.n > 0).map((p) => {
    const item = h("span", {}, h("i", { style: `background: ${p.color}` }), icon(p.ic), h("b", {}, fmt(p.n)));
    setTip(item, p.name);
    return item;
  }));
  const last = Math.max(...folders.map((f) => f.last_scan_at || 0));
  box.replaceChildren(
    donut(parts, 132, h("span", { class: "donut-mid" }, h("b", {}, fmt(sum("indexed"))))),
    h("div", { class: "glance-text" },
      h("div", { class: "glance-title" }, t("folder.ready_long")), legend,
      h("div", { class: "glance-facts" }, stat("archive", bytes(sum("bytes")), t("folder.size")),
        last ? stat("clock", ago(last), t("folders.col.scanned")) : null)));
}

async function pickFolder() {
  // In the browser, the picker is a separate native window that can open behind the browser.
  if (!state.appMode) toast(t("folders.picker_behind"), "folder");
  try {
    const { path } = await post("/api/folders/pick");
    if (path) await addFolder(path);
  } catch (e) {
    toast(e.message, "alert");
  }
}


// ---------- settings ----------

const shortGpu = (name) => (name || "GPU").replace(/^NVIDIA\s+/i, "").replace(/^GeForce\s+/i, "");

function renderSettings({ settings, resources, model }) {
  const key = JSON.stringify([settings, i18n.lang]);
  if (key !== state.settingsKey) {
    state.settingsKey = key;
    sideWidth(settings.map_side);
    $("autostart-row").hidden = settings.autostart == null;
    $("autostart").checked = !!settings.autostart;
    $("autostart").setAttribute("aria-label", t("settings.autostart"));
    $("auto-update").checked = !!settings.auto_update;
    setTip($("auto-update"), t("settings.auto_update"));
    $("diagnostics-on").checked = !!settings.diagnostics;
    setTip($("diagnostics-row"), t("settings.diagnostics_tip"));
    const off = (settings.kinds_off || []).length + (settings.formats_off || []).length; // (kinds and formats)
    $("types-summary").textContent = off ? tp("types.summary_off", off) : t("types.all_on");
    if (state.diagnostics !== !!settings.diagnostics) {
      state.diagnostics = !!settings.diagnostics;
      $("nav-diagnostics").hidden = !state.diagnostics;
      if (!state.diagnostics && state.page === "diagnostics") goTo("settings");
      if (state.diagnostics && location.hash.startsWith("#/diagnostics")) route(); // (opened before the settings came)
    }
    const langs = settings.languages;
    const sel = $("language");
    sel.replaceChildren(
      h("option", { value: "auto" }, t("lang.auto", { name: langs[settings.language_system] || "English" })),
      ...Object.entries(langs).map(([code, name]) => h("option", { value: code }, name)),
    );
    sel.value = settings.language || "auto";
    for (const key of ["view_files", "view_images"]) {
      if (settings[key] && settings[key] !== state.viewSaved[key]) { // saved here, or in another window
        state.viewSaved[key] = settings[key];
        if (settings[key] !== state.views[key]) {
          state.views[key] = settings[key];
          updateViews();
          if (key === viewKey() && state.results.length) renderResults(state.results, state.likely);
          else if (key === viewKey() && !$("q").value.trim() && !state.similar) renderEmpty(); // (the example)
        }
      }
    }
    if (settings.theme !== theme.saved) { // saved here, or in another window
      theme.saved = settings.theme;
      if (settings.theme !== theme.mode) applyTheme(settings.theme);
    }
  }

  const m = resources.metrics;
  const free = (gb) => t("settings.gb_free", { gb: fmt(gb, 1) });
  meter("cpu", m.cpu_others, t("settings.cpu"));
  meter("ram", m.ram_total_gb ? (1 - m.ram_free_gb / m.ram_total_gb) * 100 : null,
    `${t("settings.memory")}${m.ram_total_gb ? `\n${free(m.ram_free_gb)}` : ""}`);
  // Only NVIDIA cards report how busy they are: other cards, or none, show a dash.
  const nvidia = m.vram_total_gb != null;
  meter("gpu", m.gpu_others, nvidia ? t("settings.gpu") : t("settings.gpu_unmeasured"), nvidia ? t("settings.na") : "–");
  meter("vram", nvidia ? (1 - m.vram_free_gb / m.vram_total_gb) * 100 : null,
    nvidia ? `${t("settings.vram")}\n${free(m.vram_free_gb)}` : t("settings.gpu_unmeasured"), nvidia ? t("settings.na") : "–");
  renderDevice(model);
}

function meter(name, used, tip, unknown = t("settings.na")) {
  const known = used != null;
  const frac = known ? Math.max(0, Math.min(1, used / 100)) : 0;
  $(`m-${name}`).textContent = known ? pct(used) : unknown;
  const bar = $(`mb-${name}`);
  bar.style.width = `${Math.max(frac * 100, 2)}%`;
  bar.className = frac > 0.85 ? "high" : frac > 0.6 ? "mid" : "";
  setTip($(`meter-${name}`), tip);
}

// Where the model runs, as small chips.
function renderDevice(model) {
  const threads = model.setup ? model.setup.threads : 0;
  const key = JSON.stringify([model.status, model.on_gpu, model.gpu_name, threads, model.cpu_because, i18n.lang]);
  if (key === state.deviceKey) return;
  state.deviceKey = key;
  const chips = [];
  if (model.status === "ready") {
    const where = model.on_gpu ? `${model.gpu_name} (DirectML)` : tp("device.cpu", threads);
    chips.push(stat("chip", model.on_gpu ? shortGpu(model.gpu_name) : "CPU", t("settings.model_on", { device: where })));
    if (!model.on_gpu) { // why indexing is slow
      const why = t(`device.why_${model.cpu_because || "busy"}`);
      chips.push(stat("info", t("settings.no_gpu"), `${why}\n${t("settings.cpu_only")}`));
    }
  } else {
    const k = { loading: "model.loading", downloading: "model.downloading", error: "model.error" }[model.status] || "model.not_loaded";
    chips.push(stat("hourglass", "", t("settings.model_state", { state: t(k) })));
  }
  $("device").replaceChildren(...chips);
}

$("autostart").addEventListener("change", (e) => saveSettings({ autostart: e.target.checked }));
$("auto-update").addEventListener("change", (e) => saveSettings({ auto_update: e.target.checked }));
$("diagnostics-on").addEventListener("change", (e) => saveSettings({ diagnostics: e.target.checked }));
$("language").addEventListener("change", (e) => saveSettings({ language: e.target.value }));

async function saveSettings(change) {
  try {
    await post("/api/settings", change);
    state.settingsKey = "";
    refresh();
  } catch (e) { toast(e.message, "alert"); }
}

// ---------- theme ----------

// Light, dark, or the system's ("auto"). theme.js picked one before the page was drawn; this
// switches it live. The map's canvas can't use CSS variables, so it keeps its colours here.
const darkQuery = matchMedia("(prefers-color-scheme: dark)");
const theme = { mode: document.documentElement.dataset.themeMode || "auto", saved: undefined, dark: null };
const hexRgb = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));

function applyTheme(mode = theme.mode) {
  const root = document.documentElement;
  const dark = mode === "dark" || (mode === "auto" && darkQuery.matches);
  theme.mode = root.dataset.themeMode = mode;
  root.dataset.theme = dark ? "dark" : "light";
  const css = getComputedStyle(root);
  const read = (name) => css.getPropertyValue(name).trim();
  Object.assign(theme, {
    ink: read("--ink-rgb"), card: read("--card-rgb").split(",").map(Number), well: read("--well"),
    neutral: read("--neutral-rgb").split(",").map(Number), pos: hexRgb(read("--pink")), neg: hexRgb(read("--blue")),
  });
  for (const k of Object.keys(KIND_COLOR)) KIND_COLOR[k] = read(`--kind-${k}`);
  for (const b of $("themes").querySelectorAll("button")) {
    const on = b.dataset.themeMode === mode;
    b.classList.toggle("active", on);
    b.setAttribute("aria-checked", on);
  }
  if (dark !== theme.dark) {
    theme.dark = dark;
    if (state.appMode) post("/api/window/theme", { dark }).catch(() => {}); // the title bar follows
  }
  drawFingerprint(map.vector, map.dim);
  startMapLoop();
}

darkQuery.addEventListener("change", () => { if (theme.mode === "auto") applyTheme(); });
for (const b of $("themes").querySelectorAll("button")) {
  b.addEventListener("click", () => {
    applyTheme(b.dataset.themeMode);
    saveSettings({ theme: b.dataset.themeMode });
  });
}

// ---------- updates ----------

// A newer version: the badge, and what's new in every version since this one (then Update). After
// an update the same badge shows what it brought, until that has been looked at.
function renderUpdate({ update: u, settings }) {
  state.update = u;
  $("version").textContent = `v${u.current}`;
  setTip($("brand"), `Quiekel Embed v${u.current}`);

  // What the last check found goes with the button that checks.
  const found = u.latest ? t("update.available", { version: u.latest.version })
    : u.error ? t(u.error.key, u.error.params)
    : u.last_check ? t("update.up_to_date", { ago: ago(u.last_check) }) : "";
  setTip($("check-now"), found ? `${t("update.check")}\n${found}` : t("update.check"));

  const news = newsReleases(u).length > 0;
  const unseen = news && settings.seen_version !== u.current;
  const badge = $("update-badge");
  badge.hidden = !u.latest && !unseen;
  badge.classList.toggle("news", !u.latest && unseen);
  const name = u.latest ? "update" : "gift";
  if (badge.dataset.icon !== name || !badge.querySelector(":scope > svg.i")) {
    badge.dataset.icon = name;
    badge.querySelector(":scope > svg.i")?.remove();
    badge.prepend(icon(name));
  }
  setTip(badge, u.latest ? t("update.sticker", { version: u.latest.version }) : t("update.whats_new_in", { version: u.current }));
  const version = $("version");
  version.classList.toggle("link", news);
  setTip(version, news ? t("update.whats_new_in", { version: u.current }) : "");

  if (u.last_result && !u.last_result.ok && !state.resultShown) {
    state.resultShown = true;
    toast(t("update.failed", { error: u.last_result.error }), "alert");
    saveSettings({ seen_version: u.current });
  }
  if (!$("update-modal").hidden && state.updateMode === "update") renderStage(u);
}

// The releases this version brought (written down by the version before it).
function newsReleases(u) {
  const w = u.whats_new;
  if (!w || !w.releases) return [];
  return w.releases.filter((r) => !isNewer(r.version, u.current) && isNewer(r.version, w.from));
}

function isNewer(a, b) {
  const pa = String(a).split(".").map(Number);
  const pb = String(b || "0.0.0").split(".").map(Number);
  for (let i = 0; i < 3; i++) if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) > (pb[i] || 0);
  return false;
}

// "update": a newer version, what's new in it, and Update. "news": what this version brought.
function openUpdateModal(mode) {
  const u = state.update;
  if (!u) return;
  const releases = mode === "update" ? (u.releases && u.releases.length ? u.releases : [u.latest]).filter(Boolean) : newsReleases(u);
  if (!releases.length) return;
  state.updateMode = mode;
  $("um-title").textContent = mode === "update" ? t("update.modal_title", { version: u.latest.version }) : t("update.whats_new_in", { version: u.current });
  const date = releases[0].published ? new Date(releases[0].published).toLocaleDateString(i18n.lang) : "";
  $("um-sub").textContent = date;
  $("um-sub").hidden = !date;
  $("um-notes").replaceChildren(...releases.map((r) => h("div", { class: "um-release" },
    releases.length > 1 ? h("h4", {}, `v${r.version}`, r.published ? h("span", { class: "muted" }, new Date(r.published).toLocaleDateString(i18n.lang)) : "") : "",
    h("pre", { class: "notes" }, plainNotes(r.notes || "")))));
  $("um-error").hidden = true;
  const apply = $("um-apply");
  apply.hidden = mode !== "update" || !u.can_apply || !u.latest.asset;
  const hint = $("um-hint");
  hint.hidden = mode !== "update" || (u.can_apply && !!u.latest.asset);
  hint.textContent = t(state.appMode ? "update.not_git" : "update.app_only");
  if (mode === "update") renderStage(u);
  $("update-modal").hidden = false;
  if (mode === "news") saveSettings({ seen_version: u.current }); // (looked at: the badge goes)
}

// Installing: the download's progress, then "closes and opens again".
function renderStage(u) {
  const stage = u.stage || { state: "idle" };
  const apply = $("um-apply");
  const busy = state.applying || stage.state === "downloading" || stage.state === "installing" || stage.state === "ready";
  apply.disabled = busy;
  apply.classList.toggle("working", busy);
  const hint = $("um-hint");
  if (stage.state === "downloading" && state.applying) {
    hint.hidden = false;
    hint.textContent = t("update.downloading", { pct: pct(stage.total ? Math.floor((stage.done / stage.total) * 100) : 0) });
  } else if ((stage.state === "ready" || stage.state === "installing") && state.applying) {
    hint.hidden = false;
    hint.textContent = t("update.restarting");
  } else if (stage.state === "failed" && state.applying) {
    state.applying = false;
    apply.disabled = false;
    apply.classList.remove("working");
    $("um-error").textContent = t("update.failed", { error: stage.error || "" });
    $("um-error").hidden = false;
  }
}

const closeUpdateModal = () => { $("update-modal").hidden = true; };

$("update-badge").addEventListener("click", () => openUpdateModal(state.update && state.update.latest ? "update" : "news"));
$("version").addEventListener("click", () => openUpdateModal("news"));
$("um-later").addEventListener("click", closeUpdateModal);
$("um-github").addEventListener("click", () => post("/api/update/open").catch((e) => toast(e.message, "alert")));
$("update-modal").addEventListener("click", (e) => { if (e.target.id === "update-modal") closeUpdateModal(); });
$("um-apply").addEventListener("click", async () => {
  state.applying = true;
  $("um-error").hidden = true;
  renderStage(state.update);
  try {
    await post("/api/update/apply");
    refresh();
  } catch (e) {
    state.applying = false;
    renderStage(state.update);
    $("um-error").textContent = e.message;
    $("um-error").hidden = false;
  }
});

$("report-bug").addEventListener("click", () => post("/api/report-bug").catch((e) => toast(e.message, "alert")));

$("check-now").addEventListener("click", async () => {
  const btn = $("check-now");
  btn.disabled = true;
  btn.classList.add("working");
  try {
    const u = await post("/api/update/check");
    state.update = u;
    if (u.latest) openUpdateModal("update");
    else if (u.error) toast(t(u.error.key, u.error.params), "alert");
    else toast(t("update.latest", { version: u.current }), "check");
    refresh();
  } catch (e) {
    toast(e.message, "alert");
  } finally {
    btn.disabled = false;
    btn.classList.remove("working");
  }
});

// ---------- diagnostics ----------

// What the app uses: a strip of facts (where the model runs and why, the index, how long searches
// take, what's waiting), small line charts of the last ten minutes that fill up from when it was
// switched on, and where indexing time went. Fetched every two seconds while the tab is open.
let diagTimer = null;

async function refreshDiagnostics() {
  clearTimeout(diagTimer);
  if (state.page !== "diagnostics") return;
  try {
    renderDiagnostics(await api("/api/diagnostics"));
  } catch { /* the next try */ }
  diagTimer = setTimeout(refreshDiagnostics, 2000);
}

const DIAG_COLORS = { app: "var(--orange)", model: "var(--blue)", others: "var(--faint)", files: "var(--green)" };
// (the kinds a file is read as, under the names the filters give them: code is text there too)
const DIAG_KINDS = [[["doc"], "docs", "--kind-docs"], [["text", "code"], "text", "--kind-text"], [["image"], "images", "--kind-images"]];

// A round top for a chart, a little above its highest value: 1, 2, 2.5 or 5 times a power of ten.
function niceTop(v) {
  if (!(v > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(v * 1.1));
  return [1, 2, 2.5, 5, 10].map((m) => m * p).find((x) => x >= v * 1.1);
}

const megabytes = (v) => (v >= 1000 ? `${fmt(v / 1000, v >= 10000 ? 0 : 1)} GB` : `${fmt(Math.round(v) || 0)} MB`); // (never "-0")
const median = (xs) => { const s = [...xs].sort((a, b) => a - b); return s.length ? s[Math.floor(s.length / 2)] : null; };

// One chart: the lines from t0 to t1 (seconds), its top value beside the title, each line's last
// value in the legend.
function diagChart(title, lines, { show, top = null, t0, t1, more = null }) {
  const W = 300, H = 80;
  const max = top ?? niceTop(Math.max(0, ...lines.flatMap((l) => l.points.map((p) => p[1]))));
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.classList.add("dg-svg");
  for (const y of [1, H / 2]) {
    const grid = document.createElementNS(ns, "line");
    grid.setAttribute("x1", 0); grid.setAttribute("x2", W); grid.setAttribute("y1", y); grid.setAttribute("y2", y);
    grid.classList.add("dg-grid");
    svg.append(grid);
  }
  const x = (tt) => ((tt - t0) / Math.max(1, t1 - t0)) * W;
  const y = (v) => H - (Math.min(v, max) / max) * (H - 3) - 1.5;
  for (const l of lines) {
    const pts = l.points.filter(([tt]) => tt >= t0);
    if (pts.length < 2) continue;
    const pl = document.createElementNS(ns, "polyline");
    pl.setAttribute("points", pts.map(([tt, v]) => `${x(tt).toFixed(1)},${y(v).toFixed(1)}`).join(" "));
    pl.style.stroke = l.color;
    svg.append(pl);
  }
  const last = (l) => (l.points.length ? l.points[l.points.length - 1][1] : null);
  const legend = h("div", { class: "dg-legend" }, ...lines.map((l) => h("span", {},
    h("i", { style: `background: ${l.color}` }), l.name, h("b", {}, last(l) == null ? "–" : show(last(l))))));
  return h("div", { class: "card dg-chart" }, h("div", { class: "dg-title" }, h("span", {}, title), h("span", { class: "dg-top" }, show(max))),
    svg, legend, more);
}

// What indexing did, second by second: one band over the charts' time in the colour of what it was
// doing; the legend says how long each. (Nothing to do: the band's own colour.)
const ACTIVITY = [["indexing", "var(--orange)"], ["scanning", "var(--blue)"], ["waiting", "var(--yellow)"],
  ["paused", "var(--faint)"], ["idle", "var(--well)"]];

// (a burst shorter than a second still counts: files were read since the sample before)
function activityOf(x, before) {
  if (x.paused) return "paused";
  if (x.state === "loading-model" || ((x.state === "indexing" || x.state === "scanning") && x.waiting)) return "waiting";
  if (x.state === "indexing" || x.state === "scanning") return x.state;
  return before?.pipeline && x.pipeline && x.pipeline.files > before.pipeline.files ? "indexing" : "idle";
}

const duration = (sec) => (sec >= 60 ? t("diag.minutes", { n: fmt(Math.round(sec / 60)) }) : t("diag.seconds", { n: fmt(Math.round(sec)) }));

function renderActivityBand(s, t0, t1) {
  const box = $("dg-activity");
  const runs = [];
  for (const [i, x] of s.entries()) {
    if (x.t < t0) continue;
    const a = activityOf(x, s[i - 1]);
    if (runs.length && runs[runs.length - 1].a === a) continue;
    runs.push({ a, from: x.t });
  }
  runs.forEach((r, i) => { r.to = i + 1 < runs.length ? runs[i + 1].from : t1; });
  box.hidden = runs.length === 0;
  if (!runs.length) return;
  const span = Math.max(1, t1 - t0);
  const color = Object.fromEntries(ACTIVITY);
  const band = h("div", { class: "dg-band" }, ...runs.filter((r) => r.a !== "idle").map((r) => {
    const seg = h("i", { style: `left: ${((r.from - t0) / span) * 100}%; width: ${Math.max(0.4, ((r.to - r.from) / span) * 100)}%; background: ${color[r.a]}` });
    setTip(seg, `${t(`diag.state.${r.a}`)}\n${duration(r.to - r.from)}`);
    return seg;
  }));
  const total = new Map();
  for (const r of runs) total.set(r.a, (total.get(r.a) || 0) + (r.to - r.from));
  const legend = h("div", { class: "dg-legend" }, ...ACTIVITY.filter(([a]) => total.get(a) >= 1).map(([a, c]) => h("span", {},
    h("i", { style: `background: ${c}` }), t(`diag.state.${a}`), h("b", {}, duration(total.get(a))))));
  box.replaceChildren(h("div", { class: "dg-title" }, h("span", {}, t("diag.activity"))), band, legend);
}

// Searches as bars at the time they ran: the part the model took to read the words at the bottom,
// finding the files on top.
function searchChart(searches, t0, t1) {
  const W = 300, H = 80;
  const shown = searches.filter((x) => x.t >= t0);
  const max = niceTop(Math.max(1, ...shown.map((x) => x.ms)));
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.classList.add("dg-svg");
  const rect = (x, y, h, color) => {
    const r = document.createElementNS(ns, "rect");
    r.setAttribute("x", x.toFixed(1)); r.setAttribute("y", y.toFixed(1)); r.setAttribute("width", 3); r.setAttribute("height", Math.max(0.5, h).toFixed(1));
    r.style.fill = color;
    svg.append(r);
  };
  for (const g of [1, H / 2]) {
    const line = document.createElementNS(ns, "line");
    line.setAttribute("x1", 0); line.setAttribute("x2", W); line.setAttribute("y1", g); line.setAttribute("y2", g);
    line.classList.add("dg-grid");
    svg.append(line);
  }
  const scale = (v) => (Math.min(v, max) / max) * (H - 3);
  for (const x of shown) {
    const px = ((x.t - t0) / Math.max(1, t1 - t0)) * (W - 3);
    const model = scale(x.model_ms), all = scale(x.ms);
    rect(px, H - all, all - model, "var(--orange)");
    rect(px, H - model, model, "var(--blue)");
  }
  const mid = (key) => (shown.length ? t("diag.ms", { n: fmt(Math.round(median(shown.map((x) => x[key])))) }) : "–");
  const legend = h("div", { class: "dg-legend" },
    h("span", {}, h("i", { style: "background: var(--blue)" }), t("diag.search_words"), h("b", {}, mid("model_ms"))),
    h("span", {}, h("i", { style: "background: var(--orange)" }), t("diag.search_rest"),
      h("b", {}, shown.length ? t("diag.ms", { n: fmt(Math.round(median(shown.map((x) => x.ms - x.model_ms)))) }) : "–")));
  return h("div", { class: "card dg-chart" },
    h("div", { class: "dg-title" }, h("span", {}, t("diag.searches")), h("span", { class: "dg-top" }, t("diag.ms", { n: fmt(max) }))), svg, legend);
}

// Where the model runs, and why there.
function setupChip(d) {
  const s = d.setup;
  if (!s) return null;
  const why = s.gpu
    ? (s.shared ? t("diag.setup_shared", { gb: fmt(d.pc.ram_gb, 0) }) : t("diag.setup_graphics", { gb: fmt(d.pc.card_gb, 0) }))
    : s.name === "light" ? t("diag.setup_light", { gb: fmt(d.pc.ram_gb, 0), cores: d.pc.cores })
      : t(`device.why_${d.cpu_because || (d.pc.card ? "shared" : "none")}`);
  const on = t("settings.model_on", { device: s.gpu ? `${s.card} (DirectML)` : tp("device.cpu", s.threads) });
  return stat("chip", s.gpu ? shortGpu(s.card) : t("diag.cpu"), `${on}\n${why}`);
}

function renderDiagnostics(d) {
  const s = d.samples || [];
  const at = (f) => s.map((x) => { try { const v = f(x); return v == null ? null : [x.t, v]; } catch { return null; } }).filter(Boolean);
  // Per second, from the indexer's counts (smoothed over 5 seconds): files read, passages and
  // pictures through the model.
  const rate = (key) => {
    const out = [];
    for (let i = 5; i < s.length; i++) {
      const a = s[i - 5].pipeline, b = s[i].pipeline;
      if (a && b && s[i].t > s[i - 5].t) out.push([s[i].t, Math.max(0, (b[key] - a[key]) / (s[i].t - s[i - 5].t))]);
    }
    return out;
  };
  const files = rate("files");
  // The charts start when Diagnostics was switched on and fill up to ten minutes.
  const t1 = s.length ? s[s.length - 1].t : Date.now() / 1000;
  const t0 = Math.min(t1 - 30, Math.max(s.length ? s[0].t : t1, t1 - 600));
  // What's waiting, and how long it takes at the last minute's pace.
  const minute = s.filter((x) => x.t >= t1 - 60 && x.pipeline);
  const pace = minute.length > 1 && minute[minute.length - 1].t > minute[0].t
    ? (minute[minute.length - 1].pipeline.files - minute[0].pipeline.files) / (minute[minute.length - 1].t - minute[0].t) : 0;
  const waiting = (state.last?.folders || []).reduce((n, f) => n + (f.pending || 0), 0);
  const ix = d.index || {};
  const searches = (d.searches || []).map((x) => x.ms);
  const strip = [
    setupChip(d),
    stat("doc", fmt(ix.files || 0), [t("diag.index", { files: fmt(ix.files || 0), passages: fmt(ix.passages || 0) }),
      t("diag.on_disk", { size: megabytes(ix.disk_mb || 0) }),
      ix.memory_mb > 0 ? t("diag.in_memory", { size: megabytes(ix.memory_mb) }) : t("diag.searched_on_disk")].join("\n")),
    searches.length ? stat("search", t("diag.ms", { n: fmt(Math.round(median(searches))) }), t("diag.search_tip", {
      n: fmt(searches.length), model: fmt(Math.round(median(d.searches.map((x) => x.model_ms)))) })) : null,
    waiting ? stat("hourglass", pace > 0 ? `${fmt(waiting)} · ${eta(waiting / pace)}` : fmt(waiting), t("diag.waiting_tip")) : null,
    // How far back the charts go (the same for all of them).
    stat("clock", t1 - t0 >= 60 ? t("diag.minutes", { n: fmt(Math.round((t1 - t0) / 60)) }) : t("diag.seconds", { n: fmt(Math.round(t1 - t0)) }),
      t("diag.span_tip")),
  ];
  const now = files.length ? files[files.length - 1][1] : 0;
  $("dg-strip").replaceChildren(...[
    s.length && s[s.length - 1].state === "indexing"
      ? h("div", { class: "dg-headline" }, h("b", {}, fmt(Math.round(now * 10) / 10)), h("span", {}, t("diag.files"))) : null,
    ...strip].filter(Boolean));
  // Which kinds of files were read in the charts' time.
  const ps = s.map((x) => x.pipeline).filter(Boolean);
  const first = ps[0], lastP = ps[ps.length - 1];
  const kinds = first?.kinds && lastP?.kinds
    ? DIAG_KINDS.map(([keys, name, color]) => ({ name, color, n: keys.reduce((n, k) => n + (lastP.kinds[k] || 0) - (first.kinds[k] || 0), 0) }))
      .filter((k) => k.n > 0) : [];
  const byKind = kinds.length ? h("div", { class: "dg-legend dg-kinds" }, ...kinds.map((k) => h("span", {},
    h("i", { style: `background: var(${k.color})` }), t(`search.kind.${k.name}`), h("b", {}, fmt(k.n))))) : null;
  const pctShow = (v) => pct(Math.round(v));
  const perSecond = (v) => `${fmt(Math.round(v * 10) / 10)}/s`;
  const vramShared = d.setup?.shared;
  renderActivityBand(s, t0, t1);
  $("dg-charts").replaceChildren(
    diagChart(t("diag.files"), [{ name: t("diag.files_short"), color: DIAG_COLORS.files, points: files }],
      { show: perSecond, t0, t1, more: byKind }),
    diagChart(t("diag.model"), [
      { name: t("diag.passages"), color: "var(--blue)", points: rate("texts") },
      { name: t("diag.pictures"), color: "var(--pink)", points: rate("pictures") }], { show: perSecond, t0, t1 }),
    diagChart(t("diag.queue"), [{ name: t("folders.col.files"), color: "var(--yellow)", points: at((x) => x.pending) }],
      { show: (v) => fmt(Math.round(v)), t0, t1 }),
    diagChart(t("diag.disk"), [{ name: t("diag.app"), color: DIAG_COLORS.app, points: at((x) => x.disk.read_mbs) }],
      { show: (v) => (v >= 1 ? `${fmt(Math.round(v * 10) / 10)} MB/s` : `${fmt(Math.round(v * 1000))} KB/s`), t0, t1 }),
    diagChart(t("diag.cpu"), [
      { name: t("diag.app"), color: DIAG_COLORS.app, points: at((x) => x.app.cpu) },
      { name: t("diag.model"), color: DIAG_COLORS.model, points: at((x) => x.model.cpu) },
      { name: t("diag.pc"), color: DIAG_COLORS.others, points: at((x) => x.pc.cpu) }], { show: pctShow, top: 100, t0, t1 }),
    diagChart(t("diag.memory"), [
      { name: t("diag.app"), color: DIAG_COLORS.app, points: at((x) => x.app.mb) },
      { name: t("diag.model"), color: DIAG_COLORS.model, points: at((x) => x.model.mb) }], { show: megabytes, t0, t1 }),
    diagChart(t("diag.gpu"), [
      { name: t("diag.model"), color: DIAG_COLORS.model, points: at((x) => x.model.gpu) },
      { name: t("diag.others"), color: DIAG_COLORS.others, points: at((x) => x.pc.gpu_others) }], { show: pctShow, top: 100, t0, t1 }),
    diagChart(vramShared ? t("diag.shared_memory") : t("diag.vram"), [{ name: t("diag.model"), color: DIAG_COLORS.model,
      points: at((x) => (vramShared ? x.model.shared_mb : x.model.vram_mb)) }], { show: megabytes, t0, t1 }),
    searchChart(d.searches || [], t0, t1),
  );
  // Where indexing time went in these minutes: one bar.
  const box = $("dg-time");
  if (ps.length < 2) { box.hidden = true; return; }
  const parts = [["diag.reading", lastP.read_ms - first.read_ms, "var(--orange)"], ["diag.waiting", lastP.wait_ms - first.wait_ms, "var(--pink)"],
    ["diag.model", lastP.embed_ms - first.embed_ms, "var(--blue)"], ["diag.saving", lastP.save_ms - first.save_ms, "var(--green)"]];
  const total = parts.reduce((sum, p) => sum + p[1], 0);
  box.hidden = total <= 0;
  if (total <= 0) return;
  box.replaceChildren(h("div", { class: "dg-title" }, t("diag.time")),
    h("div", { class: "dg-bar" }, ...parts.map(([, v, c]) => h("i", { style: `width: ${(v / total) * 100}%; background: ${c}` }))),
    h("div", { class: "dg-legend" }, ...parts.map(([k, v, c]) => h("span", {}, h("i", { style: `background: ${c}` }), t(k),
      h("b", {}, `${fmt(Math.round(v / 1000))} s`)))));
}

// ---------- confirm ----------

// A small dialog for things that can't be undone. Resolves to true when you confirm.
let confirmDone = null;

function confirmDialog({ title, text, ok, okIcon = "trash" }) {
  confirmDone?.(false);
  hideTip();
  $("cm-title").textContent = title;
  $("cm-text").textContent = text;
  const btn = $("cm-ok");
  btn.replaceChildren(icon(okIcon));
  setTip(btn, ok);
  $("confirm-modal").hidden = false;
  btn.focus(); // Enter confirms, Esc cancels
  return new Promise((resolve) => {
    confirmDone = (yes) => {
      confirmDone = null;
      $("confirm-modal").hidden = true;
      hideTip();
      resolve(yes);
    };
  });
}

$("cm-ok").addEventListener("click", () => confirmDone?.(true));
$("cm-cancel").addEventListener("click", () => confirmDone?.(false));
$("confirm-modal").addEventListener("click", (e) => { if (e.target.id === "confirm-modal") confirmDone?.(false); });

// ---------- search ----------

function filtersQuery() {
  const qs = new URLSearchParams({ kind: state.kind, limit: currentView() === "cards" ? 30 : 60 });
  if (state.scope) qs.set("under", state.scope.path);
  return qs;
}

async function runSearch() {
  const q = $("q").value.trim();
  if (state.similar) return state.similar.file ? runExample(state.similar) : runSimilar(state.similar);
  if (!q) return renderResults(null);
  const qs = filtersQuery();
  qs.set("q", q);
  state.terms = queryTerms(q);
  await fetchResults(`/api/search?${qs}`);
}

// Files like one file; words typed meanwhile make it "like this, and like these words".
async function runSimilar(target) {
  state.similar = target;
  const q = $("q").value.trim();
  state.terms = q ? queryTerms(q) : [];
  $("q").placeholder = `${t("search.add_words")}…`;
  const qs = filtersQuery();
  if (q) qs.set("q", q);
  await fetchResults(`/api/similar/${target.id}?${qs}`);
}

async function fetchResults(url, options = {}) {
  searchCtl?.abort();
  searchCtl = new AbortController();
  try {
    const res = await fetch(url, { ...options, signal: searchCtl.signal });
    if (!res.ok) throw new Error(apiMessage((await res.json().catch(() => ({}))).detail, res.statusText));
    const data = await res.json();
    state.semantic = data.semantic;
    state.expanded = false; // each search shows its likely results first
    state.fetched = { items: data.results, likely: data.likely };
    state.reference = data.reference || null;
    if (state.reference && state.similar?.picture) state.reference.picture = state.similar.picture;
    updateNotice();
    renderResults(data.results, data.likely);
  } catch (e) {
    if (e.name !== "AbortError") renderMessage(e.message);
  }
}

// The hourglass next to the results: matches by meaning aren't there yet (the model is waking up),
// or indexing is still running, so files it hasn't reached are found by name only for now.
function updateNotice() {
  const notice = $("notice");
  const p = state.last?.progress;
  const indexing = p && (p.state === "indexing" || p.state === "scanning");
  const searching = !!($("q").value.trim() || state.similar);
  notice.hidden = !searching || !(state.semantic === false || indexing);
  if (notice.hidden) return;
  if (state.semantic === false) return setTip(notice, t("search.keyword_only"));
  const progress = p.state === "indexing" && p.total ? `${t("activity.progress", { done: fmt(p.done), total: fmt(p.total) })} · ` : "";
  setTip(notice, `${t("search.still_indexing")}\n${progress}${t("search.by_name_only")}`);
}

// No folders yet: one friendly sentence, its "add a folder" part a link to the folder picker.
function waitingText(link = true) {
  const [before, after = ""] = t("search.waiting").split("{folder}");
  // A link, not a button: buttons don't flow with text, so the comma after them could wrap. (On the
  // Index the + card above it does that: there it's just the words.)
  const add = link
    ? h("a", { href: "#/folders", class: "link", onclick: (e) => { e.preventDefault(); pickFolder(); } }, t("search.waiting_folder"))
    : h("b", {}, t("search.waiting_folder"));
  return h("p", { class: "waiting" }, before, add, after);
}

// Nothing searched yet: with no folders, the sentence that adds one; else "Search anything", in the
// middle of the page. (Quiekel stays the logo at the top of the rail, nowhere else.)
function renderEmpty() {
  const empty = $("empty");
  empty.classList.remove("anything");
  if (state.hasFolders === false) {
    empty.hidden = false;
    return empty.replaceChildren(waitingText());
  }
  if (state.hasFolders && !$("q").value.trim() && !state.similar) {
    empty.classList.add("anything");
    empty.hidden = false;
    return empty.replaceChildren(h("p", {}, t("search.anything")));
  }
  empty.hidden = true;
  empty.replaceChildren();
}

function renderMessage(msg) {
  state.results = [];
  $("results").replaceChildren();
  $("empty").hidden = false;
  $("empty").classList.remove("anything");
  $("empty").replaceChildren(h("p", {}, msg));
}

// The likely results first (the server puts them on top); the rest behind a divider that says how
// many more there are, until it's clicked.
function renderResults(items, likely) {
  const box = $("results");
  box.replaceChildren();
  hideTip();
  state.selected = -1;
  state.likely = Math.min(likely ?? (items || []).length, (items || []).length);
  // Similar files: the nearly identical ones come first (the server puts them there).
  const near = state.similar && items ? items.filter((r) => r.near).length : 0;
  // Sorted by something else than relevance: each part (likely and the rest, or nearly the same
  // and related) in that order.
  const cut = state.similar ? near : state.likely;
  if (items) items = [...sortPart(items.slice(0, cut)), ...sortPart(items.slice(cut))];
  state.results = items || [];
  const view = currentView();
  box.className = `results ${view}`;
  if (items === null) {
    $("notice").hidden = true;
    return renderEmpty();
  }
  if (!items.length) return renderMessage(t("search.nothing"));
  $("empty").hidden = true;
  const make = { cards: row, list: line, grid: tile }[view];
  // In the list, the lines share a card: the likely ones one, the rest (once shown) another, with
  // the divider between the two, as between the other views' cards.
  const put = (part, from, rest = false) => {
    const els = part.map((r, k) => make(r, from + k));
    if (view === "list") return box.append(h("div", { class: `list-card${rest ? " rest" : ""}` }, els));
    for (const el of els) {
      el.classList.toggle("rest", rest);
      box.append(el);
    }
  };
  const label = (key) => h("div", { class: "more-divider static" }, h("span", { class: "more-label" }, t(key)));
  if (state.similar) {
    // The file they are like, on top; then the nearly identical ones, if any, apart from the related.
    if (state.reference) box.append(referenceCard(state.reference));
    if (!near) return put(items, 0);
    box.append(label("search.near"));
    put(items.slice(0, near), 0);
    if (near < items.length) {
      box.append(label("search.related"));
      put(items.slice(near), near);
    }
    return;
  }
  if (!state.likely) {
    // Nothing clears the bar: say so, and show what there is.
    state.expanded = true; // (the keyboard steps through all of them)
    box.append(label("search.no_clear_match"));
    return put(items, 0);
  }
  put(items.slice(0, state.likely), 0);
  if (state.likely < items.length) {
    const more = items.length - state.likely;
    const label = h("span", { class: "more-label" });
    const divider = h("button", { type: "button", class: "more-divider", "aria-expanded": "false" }, label);
    const show = (open) => {
      state.expanded = open;
      divider.setAttribute("aria-expanded", open);
      label.replaceChildren(open ? "" : tp("search.less_likely", more), icon(open ? "chevronUp" : "chevronDown"));
      setTip(divider, open ? t("search.fewer") : "");
      for (const el of box.querySelectorAll(".rest")) el.remove();
      if (open) put(items.slice(state.likely), state.likely, true);
      if (!open && state.selected >= state.likely) select(-1);
    };
    divider.addEventListener("click", () => show(!state.expanded));
    box.append(divider);
    show(!!state.expanded);
  }
}

// The results the keyboard steps through: the likely ones, or all once the rest is shown.
const visibleResults = () => (state.expanded ? state.results.length : state.likely);

// ---------- result views ----------

// Details (with the passage that matched), List (one line per file) and Grid (thumbnails).
// Photos and everything else each remember their own view, in the settings.
const viewKey = () => (state.kind === "images" ? "view_images" : "view_files");
const currentView = () => state.views[viewKey()];

function updateViews() {
  for (const b of $("views").querySelectorAll("button")) {
    const on = b.dataset.view === currentView();
    b.classList.toggle("active", on);
    b.setAttribute("aria-checked", on);
  }
}

for (const b of $("views").querySelectorAll("button")) {
  b.addEventListener("click", () => {
    const key = viewKey();
    if (state.views[key] === b.dataset.view) return;
    state.views[key] = b.dataset.view;
    updateViews();
    saveSettings({ [key]: b.dataset.view });
    if ($("q").value.trim() || state.similar) runSearch(); // the list and grid show more at once
    else renderEmpty(); // (the example, in the new view)
  });
}

// ---------- sorting ----------

// By relevance (the server's order), or by date, size, name or type: each with the direction most
// people want first; choosing the one that's on again turns it round. For this session only.
const byName = (a, b) => a.name.localeCompare(b.name, i18n.lang, { numeric: true, sensitivity: "base" });
const extOf = (r) => (r.name.includes(".") ? r.name.split(".").pop().toLowerCase() : "");
const KIND_ORDER = { doc: 0, text: 1, code: 2, image: 3 };
const SORTS = {
  date: { icon: "clock", desc: true, dirs: ["sort.oldest", "sort.newest"], cmp: (a, b) => a.mtime - b.mtime },
  size: { icon: "size", desc: true, dirs: ["sort.smallest", "sort.largest"], cmp: (a, b) => a.size - b.size },
  name: { icon: "text", desc: false, dirs: ["sort.az", "sort.za"], cmp: byName },
  type: {
    icon: "doc", desc: false, dirs: ["sort.az", "sort.za"],
    cmp: (a, b) => (KIND_ORDER[a.kind] ?? 9) - (KIND_ORDER[b.kind] ?? 9) || extOf(a).localeCompare(extOf(b)) || byName(a, b),
  },
};

function sortPart(part) {
  const s = SORTS[state.sort.key];
  if (!s) return part;
  const dir = state.sort.desc ? -1 : 1;
  return [...part].sort((a, b) => s.cmp(a, b) * dir); // (stable: ties keep their relevance)
}

function renderSort() {
  const b = $("sort-btn");
  const s = SORTS[state.sort.key];
  b.classList.toggle("on", !!s);
  setTip(b, s ? `${t(`sort.${state.sort.key}`)}\n${t(s.dirs[state.sort.desc ? 1 : 0])}` : t("sort.title"));
}

function setSort(key) {
  const s = SORTS[key];
  state.sort = key === state.sort.key && s ? { key, desc: !state.sort.desc } : { key, desc: s ? s.desc : false };
  renderSort();
  if (state.fetched) renderResults(state.fetched.items, state.fetched.likely);
}

// A menu's chosen row says so at its end: a check (or which way it sorts). No frame: a menu is the
// only frame around its rows.
function menuMark(name) {
  const mark = icon(name);
  mark.classList.add("menu-mark");
  return mark;
}

function openSortMenu() {
  const menu = $("sort-menu");
  const row = (key, name) => {
    const on = state.sort.key === key;
    const s = SORTS[key];
    const el = h("button", { type: "button", class: `menu-row${on ? " on" : ""}`, onclick: () => { setSort(key); openSortMenu(); } },
      icon(name), h("span", { class: "menu-name" }, t(`sort.${key}`)),
      on ? menuMark(s ? (state.sort.desc ? "chevronDown" : "chevronUp") : "check") : null);
    if (on && s) setTip(el, t(s.dirs[state.sort.desc ? 1 : 0]));
    return el;
  };
  menu.replaceChildren(row("relevance", "similar"), h("div", { class: "menu-sep" }),
    ...Object.entries(SORTS).map(([key, s]) => row(key, s.icon)));
  menu.hidden = false;
  const r = $("sort-btn").getBoundingClientRect();
  menu.style.transform = `translate(${Math.round(Math.max(8, r.right - menu.offsetWidth))}px, ${Math.round(r.bottom + 6)}px)`;
}

const closeSortMenu = () => { $("sort-menu").hidden = true; };
$("sort-btn").addEventListener("click", () => ($("sort-menu").hidden ? openSortMenu() : closeSortMenu()));
document.addEventListener("pointerdown", (e) => {
  if (!$("sort-menu").hidden && !e.target.closest("#sort-menu, #sort-btn")) closeSortMenu();
}, true);

const openFile = (r) => post(`/api/open/${r.file_id}`).catch((e) => toast(e.message, "alert"));
const revealFile = (r) => post(`/api/reveal/${r.file_id}`).catch((e) => toast(e.message, "alert"));
const similarTo = (r) => {
  goTo("search");
  window.scrollTo(0, 0);
  closeLightbox();
  $("q").value = ""; // (words typed now are added to it)
  runSimilar({ id: r.file_id, name: r.name });
};

// The file the similar ones are like: on top of them, with the way back to searching.
function referenceCard(r) {
  let visual;
  if (r.picture || r.preview) {
    visual = h("img", { class: "mini", src: r.picture || thumbUrl(r), alt: "" });
    visual.addEventListener("error", () => visual.replaceWith(extBadge(r)), { once: true });
  } else {
    visual = extBadge(r);
  }
  // (a dropped or pasted file isn't in the index: nothing to open, no folder)
  const name = h("span", { class: "name", onclick: r.file_id ? () => openFile(r) : null }, r.name);
  if (r.path) setTip(name, r.path);
  const back = h("button", { type: "button", class: "chip-x", onclick: () => clearSimilar() }, icon("close"));
  setTip(back, t("search.back"));
  return h("div", { class: "reference" }, icon("similar"), visual,
    h("div", { class: "ref-body" }, name, r.dir ? dirButton(r) : null), back);
}

// ---------- search by example ----------
//
// A file dropped on the window, or a picture pasted (Ctrl+V): the files like it, whether it's
// indexed or not. It's read and compared on this PC like any other; nothing is kept.

const MAX_EXAMPLE = 64 * 1024 * 1024;

function searchByExample(file) {
  if (!file) return;
  if (file.size > MAX_EXAMPLE) return toast(t("search.too_big"), "alert");
  goTo("search");
  window.scrollTo(0, 0);
  closeLightbox();
  if (state.similar?.picture) URL.revokeObjectURL(state.similar.picture);
  $("q").value = ""; // (words typed now are added to it)
  const picture = file.type.startsWith("image/") ? URL.createObjectURL(file) : null;
  runExample({ file, name: file.name || t("search.pasted"), picture });
}

async function runExample(target) {
  state.similar = target;
  const q = $("q").value.trim();
  state.terms = q ? queryTerms(q) : [];
  $("q").placeholder = `${t("search.add_words")}…`;
  const qs = filtersQuery();
  qs.set("name", target.file.name || "pasted.png");
  if (q) qs.set("q", q);
  await fetchResults(`/api/similar-to?${qs}`, {
    method: "POST", body: target.file, headers: { "x-quiekel-embed": "1", "Content-Type": "application/octet-stream" },
  });
}

{
  const veil = $("drop-veil");
  const hasFiles = (e) => [...(e.dataTransfer?.types || [])].includes("Files");
  let depth = 0; // (enter and leave come for every element passed over)
  window.addEventListener("dragenter", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth += 1;
    veil.hidden = false;
  });
  window.addEventListener("dragover", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault(); // (else the window would open the file)
    e.dataTransfer.dropEffect = "copy";
  });
  window.addEventListener("dragleave", (e) => {
    if (!hasFiles(e)) return;
    depth = Math.max(0, depth - 1);
    if (!depth) veil.hidden = true;
  });
  window.addEventListener("drop", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth = 0;
    veil.hidden = true;
    searchByExample(e.dataTransfer.files[0]);
  });
  // A picture pasted anywhere (also into the search box: text goes in as text).
  document.addEventListener("paste", (e) => {
    const file = [...(e.clipboardData?.files || [])].find((f) => f.type.startsWith("image/"));
    if (!file || e.target.closest?.("#scope-menu, .modal")) return;
    e.preventDefault();
    searchByExample(file);
  });
}

function actionButtons(r) {
  return h("span", { class: "actions" },
    iconButton("external", t("result.open"), () => openFile(r)),
    iconButton("folder", t("result.reveal"), () => revealFile(r)),
    iconButton("similar", t("result.similar"), () => similarTo(r)),
  );
}

// A file without a picture: its kind's icon and its extension, muted (no box, no colour).
const KIND_ICON = { doc: "doc", text: "text", code: "code", image: "image" };
const MAIL_EXTS = new Set(["eml", "msg", "mht", "mhtml"]);

function extBadge(r) {
  const ext = r.name.includes(".") ? r.name.split(".").pop().toLowerCase() : "";
  const name = MAIL_EXTS.has(ext) ? "mail" : KIND_ICON[r.kind] || "doc";
  return h("div", { class: "ext" }, icon(name), ext ? h("span", {}, ext) : null);
}

// How well a result matches its meaning, from 0 to 1. Scores between a search and a file run from
// about 0.55 (unrelated) to 0.82 (as close as they get), so that's the scale.
const matchShare = (similarity) => Math.max(0, Math.min(1, (similarity - 0.55) / 0.27));

// A small ring for how well something matches (0 to 1): filled that far, red through yellow to green.
function matchRing(share) {
  const ring = h("span", { class: "match-ring", style: `--hue: ${Math.round(share * 120)}` });
  ring.innerHTML = '<svg viewBox="0 0 20 20" aria-hidden="true"><circle class="t" cx="10" cy="10" r="7.5"/>'
    + `<circle class="a" cx="10" cy="10" r="7.5" pathLength="100" stroke-dasharray="${Math.max(4, Math.round(share * 100))} 100"/></svg>`;
  return ring;
}

// How sure it is that a result is what you searched for, as one ring: a file with all your words
// counts as sure (those come first), any other by its meaning, so the rings fall as the list goes
// down. The tooltip says what matched. Before the model is ready there's no meaning to go by:
// "Aa" marks a file found by your words.
function matchBadge(r) {
  const meaning = r.similarity != null ? matchShare(r.similarity) : null;
  const lines = [];
  let mark;
  if (r.exact) {
    mark = matchRing(1);
    lines.push(t("match.all_words_title"));
    if (meaning != null) lines.push(t("match.by_meaning", { pct: pct(Math.round(meaning * 100)) }));
  } else if (meaning != null) {
    mark = matchRing(meaning);
    lines.push(t("match.share", { pct: pct(Math.round(meaning * 100)) }));
    if (r.keyword) lines.push(t("match.some_words_title"));
  } else {
    mark = h("span", { class: "aa" }, "Aa");
    lines.push(t("match.some_words_title"));
  }
  const el = h("span", { class: "match" }, mark);
  setTip(el, lines.join("\n"));
  return el;
}

// A folder as people think of it: from the indexed folder it's in ("Downloads › Projekte").
function shortDir(dir) {
  const lower = dir.toLowerCase();
  const root = (state.last?.folders || []).map((f) => f.path.replace(/[\\/]+$/, ""))
    .filter((p) => lower === p.toLowerCase() || lower.startsWith(`${p.toLowerCase()}\\`))
    .sort((a, b) => b.length - a.length)[0];
  if (!root) return dir;
  return [baseName(root), ...dir.slice(root.length).split(/[\\/]/).filter(Boolean)].join(" › ");
}

// The folder a result is in, short; its whole path in the tooltip. A click searches only there.
function dirButton(r) {
  const b = h("button", { type: "button", class: "dir", onclick: () => setScope(r.dir) }, shortDir(r.dir));
  setTip(b, `${r.dir}\n${t("scope.here")}`);
  return b;
}

// Details: the name and the ring, the date and size on the right (the buttons there while you point
// at it), the folder, and the passage that matched as two lines of running text.
function row(r, i) {
  let visual;
  if (r.preview) {
    // (a document: the page its passage is on)
    visual = h("img", { class: "thumb", src: r.picture || thumbUrl(r, r.page), loading: "lazy", alt: "", onclick: () => openLightbox(i) });
    visual.addEventListener("error", () => visual.replaceWith(extBadge(r)), { once: true });
  } else {
    visual = extBadge(r);
  }
  const name = h("span", { class: "name", onclick: () => openFile(r) }, r.name);
  setTip(name, r.name);
  name.dataset.tipCut = "";
  const passage = (r.snippet || "").replace(/\s+/g, " ").trim();
  const el = h("div", { class: "result", "data-i": i },
    visual,
    h("div", { class: "body" },
      h("div", { class: "title-row" }, name, matchBadge(r)),
      h("div", { class: "side" }, h("span", { class: "info" }, `${ago(r.mtime)} · ${bytes(r.size)}`), actionButtons(r)),
      dirButton(r),
      passage ? h("p", { class: "snippet", onclick: () => el.classList.toggle("expanded") },
        r.page ? h("span", { class: "page-no" }, t("result.page", { n: fmt(r.page) })) : null,
        highlighted(passage, state.terms)) : null,
    ),
  );
  el.addEventListener("mouseenter", () => select(i, false));
  return el;
}

// List view: one line per file. The passage that matched shows when you point at the name.
function line(r, i) {
  let visual;
  if (r.preview) {
    visual = h("img", { class: "mini", src: pictureUrl(r), loading: "lazy", alt: "", onclick: () => openLightbox(i) });
    visual.addEventListener("error", () => visual.replaceWith(extBadge(r)), { once: true });
  } else {
    visual = extBadge(r);
  }
  const name = h("span", { class: "name", onclick: () => openFile(r) }, r.name);
  const passage = (r.snippet || "").replace(/\s+/g, " ").trim();
  setTip(name, passage ? (passage.length > 280 ? `${passage.slice(0, 280)}…` : passage) : r.path);
  const el = h("div", { class: "line", "data-i": i },
    visual, name, dirButton(r),
    h("span", { class: "info" }, ago(r.mtime)), h("span", { class: "info size" }, bytes(r.size)),
    matchBadge(r), actionButtons(r));
  el.addEventListener("mouseenter", () => select(i, false));
  return el;
}

function tile(r, i) {
  const name = h("span", { class: "name", onclick: () => openFile(r) }, r.name);
  setTip(name, r.path);
  const picture = r.preview
    ? h("img", { src: pictureUrl(r), loading: "lazy", alt: r.name, onclick: () => openLightbox(i) })
    : h("div", { class: "tile-ext", onclick: () => openFile(r) }, extBadge(r));
  const el = h("div", { class: "tile", "data-i": i },
    picture,
    actionButtons(r),
    h("div", { class: "tile-body" }, name, matchBadge(r)),
  );
  el.addEventListener("mouseenter", () => select(i, false));
  return el;
}

function select(i, scroll = true) {
  const at = (k) => $("results").querySelector(`[data-i="${k}"]`);
  if (state.selected >= 0) at(state.selected)?.classList.remove("selected");
  state.selected = i;
  const el = i >= 0 ? at(i) : null;
  if (el) {
    el.classList.add("selected");
    if (scroll) el.scrollIntoView({ block: "nearest" });
  }
}

// (in the similar files of one file, the words typed are added to it)
$("search-form").addEventListener("submit", (e) => {
  e.preventDefault();
  clearTimeout(searchTimer);
  runSearch();
});

$("q").addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(runSearch, 300);
});

for (const b of $("kinds").querySelectorAll("button")) b.addEventListener("click", () => setKind(b.dataset.kind));

function clearSimilar(rerun = true) {
  if (!state.similar) return;
  if (state.similar.picture) URL.revokeObjectURL(state.similar.picture);
  state.similar = null;
  state.reference = null;
  rotatePlaceholder();
  if (rerun) runSearch();
}

// ---------- quick look ----------
//
// Any result, large: a picture, a document at the page its passage is on, or for everything else
// the passage itself, your words marked. ← → go through the results shown; Enter opens the file.

function openLightbox(i) {
  const r = state.results[i];
  if (!r) return;
  hideTip();
  state.lightbox = i;
  const img = $("lb-img");
  const text = $("lb-text");
  img.hidden = !r.preview;
  text.hidden = !!r.preview;
  if (r.preview) {
    img.src = r.picture || previewUrl(r, r.page);
  } else {
    img.removeAttribute("src");
    text.className = `lb-text${r.kind === "code" ? " mono" : ""}`;
    text.replaceChildren(...highlighted(r.snippet || "", state.terms));
    text.scrollTop = 0;
  }
  $("lb-name").textContent = r.name;
  setTip($("lb-name"), r.path);
  $("lb-page").textContent = r.page ? t("result.page", { n: fmt(r.page) }) : "";
  $("lb-actions").replaceChildren(...actionButtons(r).childNodes);
  $("lb-prev").style.visibility = i > 0 ? "visible" : "hidden";
  $("lb-next").style.visibility = i < visibleResults() - 1 ? "visible" : "hidden";
  $("lightbox").hidden = false;
}

function stepLightbox(dir) {
  const next = state.lightbox + dir;
  if (next < 0 || next >= visibleResults()) return;
  select(next);
  openLightbox(next);
}

function closeLightbox() {
  $("lightbox").hidden = true;
  state.lightbox = -1;
  hideTip();
}

$("lb-close").addEventListener("click", closeLightbox);
$("lb-prev").addEventListener("click", () => stepLightbox(-1));
$("lb-next").addEventListener("click", () => stepLightbox(1));
$("lightbox").addEventListener("click", (e) => { if (e.target.id === "lightbox") closeLightbox(); });

// ---------- map ----------

// Every file is a dot in a cube; files with similar meaning sit close together. Clicking a dot
// flies there: the other dots fade by how related they are, and you can walk from file to file.
const KIND_GROUP = { doc: "docs", text: "text", code: "text", image: "images" }; // (code is text, as in File types)
const KIND_COLOR = { docs: "#1d4aff", text: "#7048e8", images: "#f2547f" };
const CUBE = [...Array(8)].map((_, i) => [i & 1 ? 1.15 : -1.15, i & 2 ? 1.15 : -1.15, i & 4 ? 1.15 : -1.15]);
const CUBE_EDGES = [[0, 1], [1, 3], [3, 2], [2, 0], [4, 5], [5, 7], [7, 6], [6, 4], [0, 4], [1, 5], [2, 6], [3, 7]];
const OVERVIEW = { tx: 0, ty: 0, tz: 0, zoom: 1 };
const TAU = Math.PI * 2;
const FOCUS_ZOOM = 2.1;
const CAMERA_D = 4.6; // camera distance: smaller means more perspective

const map = {
  points: [], byId: new Map(), total: 0,
  yaw: 0.7, pitch: 0.35, spin: true, spinBefore: null,
  cam: { ...OVERVIEW }, fly: null,
  hover: null, rowHover: null, selected: null, history: [], future: [], related: null, closest: [], vector: null,
  info: null, tab: "preview",
  highlight: null, dim: null, dimValues: null, dimCache: new Map(),
  drag: null, animStart: 0, last: 0, raf: 0, timer: 0, spinTimer: 0, screen: [],
  // the views (see "map views"): which one, Neighbours' layout, How it works (the tour: its step)
  view: "map", near: null, nearZoom: 1, help: false, steps: null,
};
let mapSearchTimer = null;

// (the kind and the folder chosen, as Search has them)
const visible = (p) => !!p && (state.kind === "all" || p.g === state.kind) && (!state.scope || p.lp.startsWith(state.scope.prefix));

function openMap() {
  loadMap();
  startMapLoop();
}

async function loadMap() {
  clearTimeout(map.timer);
  if (state.page !== "map") return;
  let m;
  try {
    // An unchanged map isn't sent again (the server says "not modified"), and isn't read again.
    const res = await fetch("/api/map");
    if (!res.ok) throw new Error(res.statusText);
    const tag = res.headers.get("ETag");
    m = tag && tag === map.etag ? map.reply : await res.json();
    map.etag = tag;
    map.reply = m;
  } catch (e) {
    toast(e.message, "alert");
    return;
  }
  $("map-busy").hidden = m.status !== "computing";
  if (m.status !== "computing" && m.rev !== map.rev) {
    map.rev = m.rev;
    setPoints(m.points, m.total);
  }
  // Still computing, or the index changed since: look again in a moment.
  if (m.status !== "ready") map.timer = setTimeout(loadMap, m.status === "computing" ? 1500 : 8000);
}

function setPoints(points, total) {
  const before = map.byId;
  const now = performance.now();
  map.byId = new Map();
  map.points = points.map((p) => {
    const old = before.get(p.id);
    const name = baseName(p.p);
    // The first time, the dots fly out from the middle; later, new ones fade in where they belong
    // and the rest stay put (or glide, after a fresh layout).
    const from = old || (before.size ? p : { x: 0, y: 0, z: 0 });
    const q = {
      ...p, g: KIND_GROUP[p.k] || "text", name, lp: p.p.toLowerCase(),
      dir: p.p.slice(0, Math.max(0, p.p.length - name.length - 1)),
      fx: from.x, fy: from.y, fz: from.z, born: old || !before.size ? 0 : now,
    };
    map.byId.set(p.id, q);
    return q;
  });
  map.total = total;
  map.animStart = performance.now();
  map.dimCache.clear();
  map.history = map.history.filter((id) => map.byId.has(id));
  map.future = map.future.filter((id) => map.byId.has(id));
  if (map.selected) {
    const again = map.byId.get(map.selected.id);
    if (again) selectPoint(again, { remember: false, fly: false });
    else clearSelection();
  }
  const count = $("map-count");
  count.hidden = total <= points.length;
  count.textContent = `${fmt(points.length)} / ${fmt(total)}`;
  setTip(count, t("map.sampled", { shown: fmt(points.length), total: fmt(total) }));
  startMapLoop();
}

function startMapLoop() {
  if (!map.raf) map.raf = requestAnimationFrame(drawMap);
}

function flyTo(to, ms = 650) {
  map.fly = { from: { ...map.cam }, to: { ...map.cam, ...to }, start: performance.now(), ms };
  startMapLoop();
}

// Pink above zero, blue below, grey around it.
function diverging(v) {
  const a = Math.min(1, Math.abs(v)) ** 0.8;
  const to = v >= 0 ? theme.pos : theme.neg;
  return `rgb(${theme.neutral.map((from, i) => Math.round(from + (to[i] - from) * a)).join(", ")})`;
}

function drawMap(now) {
  map.raf = 0;
  if (state.page !== "map" || document.hidden) return;
  const canvas = $("map-canvas");
  const dpr = devicePixelRatio || 1;
  const W = Math.round(canvas.clientWidth * dpr);
  const H = Math.round(canvas.clientHeight * dpr);
  if (!W || !H) return;
  if (canvas.width !== W || canvas.height !== H) { canvas.width = W; canvas.height = H; }
  const dt = map.last ? Math.min(now - map.last, 50) : 16;
  map.last = now;
  const near = map.view === "near" && map.near;
  // It turns while you look at it: not while another window has the focus (a game, say), and not in
  // Neighbours, whose directions come from it.
  const turning = map.spin && document.hasFocus() && !near;
  if (turning && !map.drag && !map.hover) map.yaw += dt * 0.00012;
  if (map.spin) setSpinCube((map.yaw * 180) / Math.PI);
  if (map.fly) {
    const k = Math.min(1, (now - map.fly.start) / map.fly.ms);
    const e = 1 - (1 - k) ** 3;
    for (const key of ["tx", "ty", "tz", "zoom"]) map.cam[key] = map.fly.from[key] + (map.fly.to[key] - map.fly.from[key]) * e;
    if (k >= 1) map.fly = null;
  }
  const appear = Math.min(1, (now - map.animStart) / 900);
  const ease = 1 - (1 - appear) ** 3;

  const { tx, ty, tz, zoom } = map.cam;
  const cy = Math.cos(map.yaw), sy = Math.sin(map.yaw), cp = Math.cos(map.pitch), sp = Math.sin(map.pitch);
  const scale = Math.min(W, H) * 0.23 * zoom;
  const project = (x, y, z) => {
    x -= tx; y -= ty; z -= tz;
    const x1 = x * cy - z * sy, z1 = x * sy + z * cy;
    const y2 = y * cp - z1 * sp, z2 = y * sp + z1 * cp;
    const s = CAMERA_D / Math.max(0.6, CAMERA_D - z2);
    return [W / 2 + x1 * s * scale, H / 2 - y2 * s * scale, z2, s];
  };

  const ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, W, H);
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  const ink = (a) => `rgba(${theme.ink}, ${a.toFixed(3)})`;

  let NX = 0, NY = 0, NR = 1; // Neighbours: the middle and the 0 % ring, in pixels
  if (near) {
    ({ x: NX, y: NY, r: NR } = nearFrame(W, H, dpr));
    NR *= map.nearZoom;
    // The rings: how alike, on the card's scale (the middle is 100 %).
    ctx.font = `600 ${11 * dpr}px ${getComputedStyle(document.body).fontFamily}`;
    ctx.textBaseline = "bottom";
    for (const share of [0.75, 0.5, 0.25, 0]) {
      const r = (1 - share) * NR;
      ctx.setLineDash([3 * dpr, 5 * dpr]);
      ctx.lineWidth = dpr;
      ctx.strokeStyle = ink(0.14);
      ctx.beginPath();
      ctx.arc(NX, NY, r, 0, TAU);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.fillStyle = ink(0.35);
      ctx.fillText(pct(Math.round(share * 100)), NX + 5 * dpr, NY - r - 3 * dpr);
    }
  } else {
    // A faint grid on the cube's floor, then its edges: nearer ones a little stronger.
    ctx.lineWidth = dpr;
    ctx.strokeStyle = ink(0.05);
    ctx.beginPath();
    for (let i = 1; i < 5; i++) {
      const v = -1.15 + i * 0.46;
      for (const [a, b] of [[[v, -1.15, -1.15], [v, -1.15, 1.15]], [[-1.15, -1.15, v], [1.15, -1.15, v]]]) {
        const pa = project(...a), pb = project(...b);
        ctx.moveTo(pa[0], pa[1]);
        ctx.lineTo(pb[0], pb[1]);
      }
    }
    ctx.stroke();
    const c = CUBE.map(([x, y, z]) => project(x, y, z));
    ctx.lineWidth = 1.2 * dpr;
    for (const [a, b] of CUBE_EDGES) {
      const nearer = Math.max(0, Math.min(1, ((c[a][2] + c[b][2]) / 2 + 1.6) / 3.2));
      ctx.strokeStyle = ink(0.08 + 0.1 * nearer);
      ctx.beginPath();
      ctx.moveTo(c[a][0], c[a][1]);
      ctx.lineTo(c[b][0], c[b][1]);
      ctx.stroke();
    }
  }

  // The dots, far ones first (in Neighbours: the least alike first). Each keeps its screen record
  // between frames (no garbage).
  const list = [];
  const frame = (map.frame = (map.frame || 0) + 1);
  let fading = false, moving = false;
  const glide = near ? 1 - (1 - Math.min(1, (now - map.near.start) / 700)) ** 3 : 1;
  for (const p of map.points) {
    if (!visible(p)) continue;
    let px, py, pz, s;
    if (near) {
      const to = map.near.pos.get(p.id);
      if (!to) continue;
      const gx = NX + to[0] * NR, gy = NY - to[1] * NR;
      const from = map.near.from.get(p.id) || [NX, NY];
      px = from[0] + (gx - from[0]) * glide;
      py = from[1] + (gy - from[1]) * glide;
      pz = -Math.hypot(to[0], to[1]);
      s = 1;
    } else {
      [px, py, pz, s] = project(p.fx + (p.x - p.fx) * ease, p.fy + (p.y - p.fy) * ease, p.fz + (p.z - p.fz) * ease);
      if (pz > CAMERA_D - 0.5) continue; // right in front of the camera
    }
    const q = p.q || (p.q = { p });
    q.px = px; q.py = py; q.pz = pz; q.s = s; q.r = 0; q.frame = frame;
    q.fade = p.born ? Math.min(1, (now - p.born) / 700) : 1;
    if (q.fade < 1) fading = true;
    list.push(q);
  }
  if (near && glide < 1) moving = true;
  list.sort((a, b) => a.pz - b.pz);
  const at = { get: (id) => { const q = map.byId.get(id)?.q; return q && q.frame === frame ? q : undefined; } };

  // How it works shows its example over the map: the map's own files wait faintly behind it.
  const tour = map.help && map.steps ? map.steps : null;

  const sel = map.selected && at.get(map.selected.id);
  const dims = map.dimValues;
  const lines = (from, to, i, k = 1) => {
    if (k <= 0) return;
    ctx.strokeStyle = ink(0.5 - i * 0.06);
    ctx.lineWidth = 1.3 * dpr;
    ctx.beginPath();
    ctx.moveTo(from[0], from[1]);
    ctx.lineTo(from[0] + (to.px - from[0]) * k, from[1] + (to.py - from[1]) * k);
    ctx.stroke();
  };
  ctx.setLineDash([4 * dpr, 4 * dpr]);
  if (sel && map.closest.length && !dims && !tour) { // dashed lines to the closest files
    map.closest.forEach((cl, i) => { const q = at.get(cl.id); if (q) lines([sel.px, sel.py], q, i); });
  }
  ctx.setLineDash([]);

  const base = Math.max(2.2, Math.min(5.5, 7.5 - 1.6 * Math.log10(Math.max(10, map.points.length))));
  const hl = map.highlight;
  const rel = map.related;
  for (const q of list) {
    const p = q.p;
    const depth = near ? 1 : Math.min(1, Math.max(0, (q.s - 0.75) / 0.6));
    let color = KIND_COLOR[p.g];
    let alpha = 0.4 + 0.6 * depth;
    let size = 1;
    if (dims) {
      color = diverging((dims.values.get(p.id) ?? 0) / dims.scale);
      alpha = 0.95;
    } else if (tour) {
      alpha = 0.07; // (behind the example)
    } else if (rel && p !== map.selected) {
      const w = rel.get(p.id)?.w ?? 0; // 1 = the most related file, 0 = the least
      alpha = (0.05 + 0.95 * w ** 6) * (0.7 + 0.3 * depth);
      size = 0.75 + 0.6 * w ** 4;
      if (hl && hl.has(p.id)) { alpha = Math.max(alpha, 0.85); size = Math.max(size, 1.2); }
    } else if (hl) {
      const lit = hl.has(p.id);
      alpha = lit ? 1 : 0.1;
      size = lit ? 1.4 : 1;
    }
    q.r = base * (0.5 + 0.5 * q.s) * dpr * Math.sqrt(near ? 1 : zoom) * size * (near && p === map.selected ? 1.5 : 1);
    q.color = color;
    // One fill per dot: a canvas draws 6,000 single circles faster than a few paths holding
    // them all (measured: 12 ms against 19 ms a frame).
    ctx.globalAlpha = alpha * q.fade;
    ctx.beginPath();
    ctx.arc(q.px, q.py, q.r, 0, TAU);
    ctx.fillStyle = color;
    ctx.fill();
    if (hl && hl.has(p.id) && !dims && !tour) {
      ctx.lineWidth = 1.5 * dpr;
      ctx.strokeStyle = ink(0.8);
      ctx.stroke();
    }
  }
  ctx.globalAlpha = 1;
  // The file pointed at and the selected one go on top, at full strength, with their rings: a dot
  // in front of them (a copy, a close neighbour) would cover them, and the ring would look off.
  const marked = [[map.hover || map.rowHover, ink(0.8)], [tour ? null : map.selected, "#f54e00"]];
  for (const [p, color] of marked) {
    const q = p && at.get(p.id);
    if (!q) continue;
    ctx.globalAlpha = q.fade;
    ctx.fillStyle = q.color;
    ctx.beginPath();
    ctx.arc(q.px, q.py, q.r, 0, TAU);
    ctx.fill();
    ctx.globalAlpha = 1;
    ctx.lineWidth = 2.5 * dpr;
    ctx.strokeStyle = color;
    ctx.beginPath();
    ctx.arc(q.px, q.py, q.r + 4 * dpr, 0, TAU);
    ctx.stroke();
  }
  if (tour) {
    drawTour(ctx, project, now - tour.start, tour.at, base * dpr * Math.sqrt(zoom), dpr, ink, W, H);
  } else {
    ripple($("map-ripple"), null);
    ripple($("map-ripple-spot"), null);
  }
  map.screen = list;
  if (map.drag || map.fly || appear < 1 || fading || moving || (tour && now - tour.start < 2600)) {
    startMapLoop();
  } else if (turning) {
    // The slow turn looks the same at 30 frames a second as at a 144 Hz screen's rate,
    // for a fraction of the work.
    clearTimeout(map.spinTimer);
    map.spinTimer = setTimeout(startMapLoop, 33);
  }
}

// Rings going out from a point on the map (CSS pixels): x, y and the radius they start from.
function ripple(el, at) {
  el.hidden = !at;
  if (!at) return;
  const [x, y, r] = at;
  el.style.transform = `translate(${x.toFixed(1)}px, ${y.toFixed(1)}px)`;
  el.style.setProperty("--r", `${(r + 34).toFixed(1)}px`);
  el.style.setProperty("--from", (r / (r + 34)).toFixed(3));
}

function mapHit(e) {
  const rect = $("map-canvas").getBoundingClientRect();
  const dpr = devicePixelRatio || 1;
  const x = (e.clientX - rect.left) * dpr;
  const y = (e.clientY - rect.top) * dpr;
  for (let i = map.screen.length - 1; i >= 0; i--) { // the one nearest to you first
    const q = map.screen[i];
    const r = q.r + 3 * dpr;
    if ((q.px - x) ** 2 + (q.py - y) ** 2 <= r * r) return q.p;
  }
  return null;
}

// Select a dot: fly there, stop turning, and fill the card. The card switches at once and shows
// placeholders until the file's details arrive; the map keeps its old fading until then, so
// nothing flashes. remember: Back can return to the file before.
async function selectPoint(p, { remember = true, fly = true } = {}) {
  if (!p) return clearSelection();
  if (!map.selected && map.spinBefore === null) map.spinBefore = map.spin;
  map.spin = false;
  updateSpin();
  const same = map.selected?.id === p.id; // the same file again, e.g. the map was refreshed
  if (remember && map.selected && !same) {
    map.history.push(map.selected.id);
    map.future = [];
  }
  map.selected = p;
  if (fly) flyTo({ tx: p.x, ty: p.y, tz: p.z, zoom: Math.max(map.cam.zoom, FOCUS_ZOOM) });
  if (same) {
    map.closest = map.closest.filter((cl) => map.byId.has(cl.id));
  } else {
    map.closest = [];
    map.vector = map.info = null;
    showCard(p);
  }
  const [info, rel] = await Promise.all([
    api(`/api/map/vector/${p.id}`).catch(() => null),
    api(`/api/map/related/${p.id}`).catch(() => null),
  ]);
  if (map.selected !== p) return; // another file was picked meanwhile
  map.vector = info ? info.vector : null;
  map.info = info ? info.file : null;
  showPreview(p, map.info);
  $("mc-print").classList.remove("loading");
  drawFingerprint(map.vector, map.dim);
  if (rel) setRelated(p, rel);
  else $("mc-related").replaceChildren();
  startMapLoop();
}

function setRelated(p, rel) {
  const pairs = [];
  rel.ids.forEach((id, i) => { if (id !== p.id && map.byId.has(id)) pairs.push([id, rel.sims[i]]); });
  pairs.sort((a, b) => b[1] - a[1]);
  const n = pairs.length;
  map.related = new Map(pairs.map(([id, s], i) => [id, { w: n > 1 ? 1 - i / (n - 1) : 1, s }]));
  setClosest();
  layoutNear(); // (in Neighbours: this file goes in the middle)
}

// The files the lines go to: the most similar of the kind and folder shown (map.related is in order).
function setClosest() {
  map.closest = [...map.related].filter(([id]) => visible(map.byId.get(id))).slice(0, 6).map(([id, r]) => ({ id, s: r.s }));
  renderRelatedList();
}

function clearSelection() {
  map.selected = null;
  map.history = [];
  map.future = [];
  map.info = null;
  map.related = null;
  map.closest = [];
  map.vector = null;
  $("map-card").hidden = true;
  flyTo(OVERVIEW);
  if (map.spinBefore !== null) {
    map.spin = map.spinBefore;
    map.spinBefore = null;
    updateSpin();
  }
  if (map.view === "near") setMapView("map"); // (Neighbours needs a file in the middle)
}

// Back to the file you looked at before, and forward again.
function step(from, to) {
  const p = map.byId.get(from.pop());
  if (!p) return;
  if (map.selected) to.push(map.selected.id);
  selectPoint(p, { remember: false });
}
const stepBack = () => step(map.history, map.future);
const stepForward = () => step(map.future, map.history);

function updateSteps() {
  $("mc-back").disabled = !map.history.length;
  $("mc-forward").disabled = !map.future.length;
}

function showCard(p) {
  $("map-card").hidden = map.help; // (one thing at a time beside the map: How it works has it)
  const name = $("mc-name");
  name.textContent = p.name;
  setTip(name, p.name);
  name.dataset.tipCut = "";
  const dir = $("mc-dir");
  dir.textContent = p.dir;
  setTip(dir, t("scope.here"));
  dir.onclick = () => setScope(p.dir);
  $("mc-actions").replaceChildren(...actionButtons({ file_id: p.id, name: p.name }).childNodes);
  updateSteps();
  // Placeholders until the details arrive: the card keeps its size, so nothing jumps.
  const box = $("mc-preview");
  box.className = "mc-preview loading";
  box.dataset.key = "";
  box.replaceChildren();
  $("mc-print").classList.add("loading");
  drawFingerprint(null);
  $("mc-related").replaceChildren(...[86, 64, 75, 58, 70].map((w) => h("div", { class: "mc-rel placeholder" },
    h("i", { class: "skeleton sk-dot" }), h("span", { class: "skeleton sk-line", style: `width: ${w}%` }))));
}

// The card's preview, the whole file to look through: a picture whole, a PDF's pages (each loaded as
// it scrolls near), any other document's, email's, text or code file's whole text (scrolling,
// selectable). A file that can't be read so shows the start of its text, or its kind.
const docCache = new Map(); // (a file's text or page count, by file and version)

function showPreview(p, info) {
  const box = $("mc-preview");
  const key = info ? `${p.id}:${info.mtime}:${info.size}` : "";
  if (key && box.dataset.key === key) return; // already showing it
  box.dataset.key = key;
  box.scrollTop = 0;
  if (!info) {
    box.className = "mc-preview";
    return box.replaceChildren();
  }
  const r = { file_id: p.id, mtime: info.mtime, size: info.size };
  const open = () => openFile({ file_id: p.id });
  const fallback = () => {
    box.className = "mc-preview doc";
    box.replaceChildren(info.passage
      ? h("p", { class: `mc-doc${info.kind === "code" ? " mono" : ""}` }, info.passage)
      : h("span", { class: "mc-none" }, icon(info.kind in ICONS ? info.kind : "doc")));
  };
  if (info.kind === "image") {
    const img = h("img", { src: previewUrl(r), alt: "", onclick: open });
    // (a picture still loading for a file you've since left must not touch the card)
    img.addEventListener("load", () => { if (img.isConnected) box.classList.remove("loading"); }, { once: true });
    img.addEventListener("error", () => { if (img.isConnected) fallback(); }, { once: true });
    box.className = "mc-preview picture loading";
    return box.replaceChildren(img);
  }
  box.className = "mc-preview loading";
  box.replaceChildren();
  const show = (d) => {
    if (box.dataset.key !== key) return; // (another file since)
    box.className = "mc-preview doc";
    if (d.pdf_pages) {
      return box.replaceChildren(...Array.from({ length: Math.min(d.pdf_pages, 300) }, (_, i) => {
        const img = h("img", { class: "mc-page", alt: "", loading: "lazy", src: previewUrl(r, i + 1), onclick: open });
        setTip(img, t("result.page", { n: fmt(i + 1) }));
        return img;
      }));
    }
    const pages = d.pages || [];
    if (!pages.length) return fallback();
    box.replaceChildren(...[...pages.flatMap((text, i) => [
      pages.length > 1 ? h("div", { class: "mc-page-label" }, t("result.page", { n: fmt(i + 1) })) : null,
      h("p", { class: `mc-doc${info.kind === "code" ? " mono" : ""}` }, text.trim()),
    ]), d.cut ? h("div", { class: "mc-cut" }, "…") : null].filter(Boolean));
  };
  if (docCache.has(key)) return show(docCache.get(key));
  api(`/api/text/${p.id}`).then((d) => {
    docCache.set(key, d);
    show(d);
  }).catch(() => { if (box.dataset.key === key) fallback(); });
}

// Preview or fingerprint: one place in the card, two ways to look at the file.
function setCardTab(tab) {
  map.tab = tab;
  for (const b of $("mc-tabs").querySelectorAll("button")) {
    b.classList.toggle("active", b.dataset.tab === tab);
    b.setAttribute("aria-selected", b.dataset.tab === tab);
  }
  $("mc-preview").hidden = tab !== "preview";
  $("mc-print").hidden = tab !== "print";
}

// The closest files, as rows you can hover (rings the dot) and click (goes there), with how
// similar they are as the match ring (two files score from about 0.55, unrelated, to 0.95, alike).
function renderRelatedList() {
  $("mc-related").replaceChildren(...map.closest.slice(0, 5).map((cl) => {
    const p = map.byId.get(cl.id);
    const share = Math.max(0, Math.min(1, (cl.s - 0.55) / 0.4));
    const ring = matchRing(share);
    setTip(ring, t("match.share", { pct: pct(Math.round(share * 100)) }));
    const row = h("button", { type: "button", class: "mc-rel", onclick: () => selectPoint(p) },
      h("i", { class: "mc-dot", style: `background: ${KIND_COLOR[p.g]}` }),
      h("span", { class: "mc-rel-name" }, p.name), ring);
    row.addEventListener("pointerenter", () => { map.rowHover = p; startMapLoop(); });
    row.addEventListener("pointerleave", () => { map.rowHover = null; startMapLoop(); });
    setTip(row, p.name);
    row.dataset.tipCut = ".mc-rel-name";
    return row;
  }));
  updateSteps();
}

for (const b of $("mc-tabs").querySelectorAll("button")) b.addEventListener("click", () => setCardTab(b.dataset.tab));
setCardTab("preview");

// The file's 256 numbers as a 16 × 16 grid: pink above zero, blue below.
function drawFingerprint(v, active = null, canvas = $("mc-print"), reveal = 1) {
  const ctx = canvas.getContext("2d");
  const n = 16;
  const cell = canvas.width / n;
  ctx.clearRect(0, 0, canvas.width, canvas.height); // without numbers, the well (or a placeholder) shows
  if (!v) return;
  ctx.fillStyle = theme.well;
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  // Scale to the 90th percentile, so a few big numbers don't wash out the rest.
  const sorted = v.map(Math.abs).sort((a, b) => a - b);
  const top = sorted[Math.floor(sorted.length * 0.9)] || 1;
  v.forEach((x, i) => {
    // (while it's revealed: from the top left to the bottom right, each square fading in)
    const shown = reveal >= 1 ? 1 : Math.max(0, Math.min(1, (reveal * 1.3 - ((i % n) + Math.floor(i / n)) / (2 * n - 2)) / 0.3));
    if (shown <= 0) return;
    const a = (Math.min(1, Math.abs(x) / top) ** 0.7) * shown;
    const to = x >= 0 ? theme.pos : theme.neg;
    ctx.fillStyle = `rgb(${theme.card.map((from, k) => Math.round(from + (to[k] - from) * a)).join(", ")})`;
    ctx.fillRect((i % n) * cell + 0.5, Math.floor(i / n) * cell + 0.5, cell - 1, cell - 1);
  });
  if (active !== null) {
    ctx.strokeStyle = `rgb(${theme.ink})`;
    ctx.lineWidth = 2;
    ctx.strokeRect((active % n) * cell + 1, Math.floor(active / n) * cell + 1, cell - 2, cell - 2);
  }
}

// Pointing at a square colors the whole map by that one number.
async function showDimension(d) {
  let entry = map.dimCache.get(d);
  if (!entry) {
    try {
      const r = await api(`/api/map/dimension/${d}`);
      const sorted = r.values.map(Math.abs).sort((a, b) => a - b);
      entry = { values: new Map(r.ids.map((id, i) => [id, r.values[i]])), scale: sorted[Math.floor(sorted.length * 0.95)] || 1 };
      map.dimCache.set(d, entry);
    } catch { return; }
  }
  if (map.dim === d) {
    map.dimValues = entry;
    startMapLoop();
  }
}

function updateSpin() {
  $("map-spin").classList.toggle("on", map.spin);
  setTip($("map-spin"), t("map.spin"));
  if (!map.spin) setSpinCube(0);
}

// The cube in the auto-rotate button turns along with the map.
function setSpinCube(deg) {
  const svg = $("map-spin").querySelector("svg.i");
  const turn = String(Math.round(deg * 2) / 2); // half degrees: no needless redraws
  if (!svg || svg.dataset.turn === turn) return;
  svg.dataset.turn = turn;
  const [cube, edges] = turntableCube(deg);
  svg.querySelector(".tt-cube").setAttribute("d", cube);
  svg.querySelector(".tt-edges").setAttribute("d", edges);
}

function resetMapView() {
  Object.assign(map, { yaw: 0.7, pitch: 0.35, nearZoom: 1 });
  layoutNear();
  flyTo(map.selected ? { zoom: FOCUS_ZOOM } : OVERVIEW);
}

async function mapSearch() {
  const q = $("map-q").value.trim();
  if (!q) {
    map.highlight = null;
  } else {
    try {
      const qs = new URLSearchParams({ q, kind: state.kind, limit: 40 });
      if (state.scope) qs.set("under", state.scope.path);
      const data = await api(`/api/search?${qs}`);
      // Meaning search always returns *something*: light up only the strong matches.
      const best = Math.max(0, ...data.results.map((r) => r.similarity ?? 0));
      const strong = data.results.filter((r) => r.exact || (r.similarity != null && r.similarity >= best - 0.03));
      map.highlight = new Set(strong.slice(0, 12).map((r) => r.file_id));
    } catch (e) { toast(e.message, "alert"); }
  }
  startMapLoop();
}

{
  const canvas = $("map-canvas");
  canvas.addEventListener("pointerdown", (e) => {
    map.drag = { x: e.clientX, y: e.clientY, moved: 0 };
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (map.drag) {
      const dx = e.clientX - map.drag.x, dy = e.clientY - map.drag.y;
      Object.assign(map.drag, { x: e.clientX, y: e.clientY, moved: map.drag.moved + Math.abs(dx) + Math.abs(dy) });
      map.yaw += dx * 0.008;
      map.pitch = Math.max(-1.45, Math.min(1.45, map.pitch + dy * 0.008));
      if (map.view === "near" && map.near) {
        layoutNear();
        map.near.start = 0; // (straight there while it's being turned)
      }
      hideTip();
      startMapLoop();
      return;
    }
    const p = mapHit(e);
    if (p !== map.hover) { map.hover = p; startMapLoop(); }
    canvas.style.cursor = p ? "pointer" : "grab";
    if (p) {
      const s = map.view === "near" && p !== map.selected ? map.related?.get(p.id)?.s : null;
      const how = s != null ? `\n${t("match.share", { pct: pct(Math.round(Math.max(0, Math.min(1, (s - NEAR_BOTTOM) / (NEAR_TOP - NEAR_BOTTOM))) * 100)) })}` : "";
      showTipAt(e.clientX, e.clientY, `${p.name}\n${p.dir.split(/[\\/]/).slice(-2).join("\\")}${how}`);
    } else if (tipFor === "point") {
      hideTip();
    }
  });
  canvas.addEventListener("pointerup", (e) => {
    const click = map.drag && map.drag.moved < 5;
    map.drag = null;
    if (!click) return;
    const p = mapHit(e);
    if (p && map.help) { // (a file of yours: How it works makes way for it)
      toggleHelp(false);
      return selectPoint(p);
    }
    if (p) selectPoint(p);
    else if (map.selected && map.view === "map") clearSelection();
  });
  canvas.addEventListener("pointerleave", () => { map.hover = null; if (tipFor === "point") hideTip(); });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const by = Math.exp(-e.deltaY * 0.0012);
    if (map.view === "near") {
      map.nearZoom = Math.max(0.5, Math.min(6, map.nearZoom * by)); // (Neighbours zooms on its own)
    } else {
      map.fly = null;
      map.cam.zoom = Math.max(0.5, Math.min(6, map.cam.zoom * by));
    }
    startMapLoop();
  }, { passive: false });
  canvas.addEventListener("dblclick", resetMapView);

  const print = $("mc-print");
  print.addEventListener("pointermove", (e) => {
    if (!map.vector) return;
    const r = print.getBoundingClientRect();
    const col = Math.min(15, Math.max(0, Math.floor(((e.clientX - r.left) / r.width) * 16)));
    const row = Math.min(15, Math.max(0, Math.floor(((e.clientY - r.top) / r.height) * 16)));
    const d = row * 16 + col;
    if (d !== map.dim) {
      map.dim = d;
      drawFingerprint(map.vector, d);
      showDimension(d);
    }
    const value = map.vector[d];
    showTipAt(e.clientX, e.clientY, t("map.dimension", { n: d + 1, value: `${value >= 0 ? "+" : ""}${fmt(value, 3)}` }));
  });
  print.addEventListener("pointerleave", () => {
    map.dim = null;
    map.dimValues = null;
    drawFingerprint(map.vector);
    hideTip();
    startMapLoop();
  });
}
$("mc-close").addEventListener("click", clearSelection);
$("mc-back").addEventListener("click", stepBack);
$("mc-forward").addEventListener("click", stepForward);
$("map-spin").addEventListener("click", () => { map.spin = !map.spin; map.spinBefore = null; updateSpin(); startMapLoop(); });
$("map-reset").addEventListener("click", resetMapView);
// A new size (the window, the bar above) draws the map again right away, also while it stands still.
new ResizeObserver(() => startMapLoop()).observe($("map-canvas"));
$("map-search").addEventListener("submit", (e) => { e.preventDefault(); clearTimeout(mapSearchTimer); mapSearch(); });
$("map-q").addEventListener("input", () => { clearTimeout(mapSearchTimer); mapSearchTimer = setTimeout(mapSearch, 300); });
for (const b of $("map-kinds").querySelectorAll("button")) b.addEventListener("click", () => setKind(b.dataset.kind));
document.addEventListener("visibilitychange", () => { if (!document.hidden && state.page === "map") startMapLoop(); });
window.addEventListener("focus", () => { if (state.page === "map") startMapLoop(); });

// ---------- map views ----------
//
// Two views of the same files: the map itself, and Neighbours: one file in the middle and the others
// at their true distance from it. How it works (the ? on the map) is a tour beside the map: how a
// file comes to its dot, then how a search finds files, step by step, shown on the map as it goes.

const NEAR_TOP = 0.95, NEAR_BOTTOM = 0.55; // the card's scale: 100 % and 0 % alike
const NEAR_EDGE = 1.2; // files less alike than 0 % wait just outside the outer ring

// The tour's steps: a file is read, cut into passages, turned into numbers and given its place;
// then your words are turned into numbers, compared with every file, their words looked for, and
// the results put in order. (Each has a title, map.tN, and a sentence, map.tN_d.)
const TOUR = ["map.t1", "map.t2", "map.t3", "map.t4", "map.t5", "map.t6", "map.t7", "map.t8"];
const TOUR_SEARCH = 4; // (the search's first step)

// The example, the same for everyone (its texts in the page's language): a letter about the rent,
// read and placed among nine other files; then a search for words in it. Each file: how close its
// meaning is to the words, its kind, whether it holds the words, and where it sits (around the
// middle of the map, the closest to the words nearest).
const TOUR_FILES = [
  { s: 0.80, kind: "doc", words: true, x: 0.08, y: 0.06, z: 0.04 }, // (the letter)
  { s: 0.76, kind: "doc", words: true, x: -0.18, y: 0.22, z: 0.10 },
  { s: 0.71, kind: "doc", words: true, x: 0.24, y: -0.16, z: -0.10 },
  { s: 0.66, kind: "text", words: false, x: -0.30, y: -0.12, z: 0.22 },
  { s: 0.63, kind: "image", words: false, x: 0.34, y: 0.28, z: -0.18 },
  { s: 0.61, kind: "doc", words: false, x: -0.08, y: 0.40, z: -0.28 },
  { s: 0.59, kind: "doc", words: false, x: 0.50, y: -0.30, z: 0.26 },
  { s: 0.58, kind: "text", words: false, x: -0.52, y: 0.30, z: -0.24 },
  { s: 0.57, kind: "image", words: false, x: 0.28, y: 0.54, z: 0.40 },
  { s: 0.56, kind: "text", words: false, x: -0.40, y: -0.50, z: -0.38 },
];
const TOUR_PASSAGE_SPOTS = [[0.32, 0.24, 0.12], [-0.24, 0.30, -0.10], [0.10, -0.32, 0.24]]; // (where each passage's numbers would sit)
const TOUR_NEAR = [1, 2, 5, 3, 4, 6]; // the letter's closest files
const TOUR_LIKELY = 3; // the files with the words: the likely results
const TOUR_VIEW = { tx: 0, ty: 0, tz: 0, zoom: 2.4 };

// The example's numbers: made up, the same every time.
function tourVector(seed) {
  let s = seed;
  return Array.from({ length: 256 }, () => {
    s = (s * 16807) % 2147483647;
    const u = s / 2147483647;
    return (u - 0.5) * (u > 0.85 || u < 0.15 ? 3 : 1);
  });
}
const TOUR_PRINTS = [tourVector(11), tourVector(23), tourVector(37)];
const TOUR_MEAN = TOUR_PRINTS[0].map((_, i) => (TOUR_PRINTS[0][i] + TOUR_PRINTS[1][i] + TOUR_PRINTS[2][i]) / 3);
const TOUR_WORDS = tourVector(53);
const tourNames = () => t("tour.files").split("|");

function setMapView(view) {
  if (map.view === view) return;
  if (map.help) toggleHelp(false); // (the tour shows the map)
  const was = map.view;
  map.view = view;
  for (const b of $("map-views").querySelectorAll("button")) {
    b.classList.toggle("active", b.dataset.view === view);
    b.setAttribute("aria-checked", b.dataset.view === view);
  }
  if (view === "near") {
    if (map.spinBefore === null) map.spinBefore = map.spin;
    map.spin = false;
    updateSpin();
    if (!map.selected) {
      const c = centralPoint();
      if (c) selectPoint(c, { fly: false });
    } else {
      layoutNear();
    }
  } else if (was === "near") {
    map.near = null;
  }
  renderSide();
  startMapLoop();
}

// The file nearest the middle of the map: where Neighbours and the walks start when none is picked.
function centralPoint() {
  let best = null, dist = Infinity;
  for (const p of map.points) {
    if (!visible(p)) continue;
    const d = p.x * p.x + p.y * p.y + p.z * p.z;
    if (d < dist) { dist = d; best = p; }
  }
  return best;
}

// Neighbours: its middle and the radius of the 0 % ring, in pixels: as big as the map allows.
function nearFrame(W, H, dpr) {
  return { x: W / 2, y: H / 2, r: Math.max(60 * dpr, Math.min(W / 2 - 24 * dpr, H / 2 - 30 * dpr) / NEAR_EDGE) };
}

// Where each file goes around the one in the middle: as far out as it's unlike it (the middle is
// 100 % alike, the outer ring 0 %), in the direction it has on the map as you looked at it last.
// The dots glide there from where they are now.
function layoutNear() {
  const c = map.selected;
  if (map.view !== "near" || !c || !map.related) return;
  const cy = Math.cos(map.yaw), sy = Math.sin(map.yaw), cp = Math.cos(map.pitch), sp = Math.sin(map.pitch);
  const flat = (p) => { const x1 = p.x * cy - p.z * sy, z1 = p.x * sy + p.z * cy; return [x1, p.y * cp - z1 * sp]; };
  const [cx, cyy] = flat(c);
  const pos = new Map([[c.id, [0, 0]]]);
  for (const p of map.points) {
    if (p === c) continue;
    const s = map.related.get(p.id)?.s;
    if (s == null) continue;
    const r = Math.max(0.02, Math.min(NEAR_EDGE, (NEAR_TOP - s) / (NEAR_TOP - NEAR_BOTTOM)));
    const [x, y] = flat(p);
    const a = Math.atan2(y - cyy, x - cx) || (p.id * 2.399) % TAU; // (the same spot: a direction all its own)
    pos.set(p.id, [Math.cos(a) * r, Math.sin(a) * r]);
  }
  const from = new Map();
  for (const id of pos.keys()) {
    const q = map.byId.get(id)?.q;
    if (q && q.frame === map.frame) from.set(id, [q.px, q.py]); // (drawn last frame: on the map, or here)
  }
  map.near = { center: c.id, pos, from, start: performance.now() };
  startMapLoop();
}

// How it works: the tour, in the column beside the map instead of the file's card (which comes back
// when it closes). It shows on the map, so it leaves Neighbours.
function toggleHelp(open = !map.help) {
  if (open === map.help) return;
  if (open && map.view !== "map") setMapView("map");
  map.help = open;
  map.steps = open ? { at: 0, start: performance.now() } : null;
  if (open) { // (the map holds still for the example)
    if (map.spinBefore === null) map.spinBefore = map.spin;
    map.spin = false;
    updateSpin();
    flyTo(TOUR_VIEW);
  } else {
    const p = map.selected;
    if (!p && map.spinBefore !== null) {
      map.spin = map.spinBefore;
      map.spinBefore = null;
      updateSpin();
    }
    flyTo(p ? { tx: p.x, ty: p.y, tz: p.z, zoom: FOCUS_ZOOM } : OVERVIEW);
  }
  renderSide();
  startMapLoop();
}

// Beside the map, one thing at a time: How it works while it's open, else the file picked.
function renderSide() {
  $("map-card").hidden = !map.selected || map.help;
  renderExplain();
}

// How it works, a step at a time: which part (a file, a search), its title and one sentence, what it
// shows in the room left, and ‹ dots › to go through.
function renderExplain() {
  const box = $("map-explain");
  $("map-help").classList.toggle("on", map.help);
  box.hidden = !map.help;
  if (box.hidden) return;
  const at = map.steps.at;
  const head = h("div", { class: "mx-head" }, icon("help"), h("b", {}, t("map.help")),
    iconButton("close", t("common.close"), () => toggleHelp(false), "chip-x"));
  const dots = h("span", { class: "mx-dots" }, ...TOUR.map((_, i) => {
    const dot = h("button", { type: "button", class: i === at ? "on" : "", onclick: () => goStep(i) });
    setTip(dot, t("map.step", { n: i + 1, total: TOUR.length }));
    return dot;
  }));
  const back = iconButton("chevronLeft", t("common.previous"), () => goStep(at - 1));
  back.disabled = at === 0;
  const next = iconButton("chevronRight", t("common.next"), () => goStep(at + 1));
  next.disabled = at === TOUR.length - 1;
  box.replaceChildren(head,
    h("div", { class: "mx-body" },
      h("div", { class: "mx-part" }, t(at < TOUR_SEARCH ? "map.part.file" : "map.part.search")),
      h("div", { class: "mx-title" }, t(TOUR[at])),
      h("p", { class: "mx-text" }, t(`${TOUR[at]}_d`)),
      h("div", { class: "mx-stage" }, walkVisual())),
    h("div", { class: "mx-nav" }, back, dots, next));
  requestAnimationFrame(fitStep);
}

// Once a step is laid out: each passage beside its numbers shows as many lines as its row has room
// for, a list loses the rows from its end that don't fit, and the passages' numbers know where the
// average is that they fly into.
function fitStep() {
  const stage = document.querySelector("#map-explain .mx-stage");
  if (!stage) return;
  const rows = stage.querySelector(".mx-rows");
  if (rows?.firstElementChild) rows.style.setProperty("--lines", Math.max(1, Math.floor(rows.firstElementChild.clientHeight / 20.8)));
  const visual = stage.firstElementChild;
  const trim = [...stage.querySelectorAll(".mx-trim")];
  while (trim.length && visual.offsetHeight > stage.clientHeight) trim.pop().remove();
  const mean = stage.querySelector(".mx-mean");
  if (mean) {
    const m = mean.getBoundingClientRect();
    for (const c of stage.querySelectorAll(".mx-prints.merge > canvas")) {
      const r = c.getBoundingClientRect();
      c.style.setProperty("--dx", `${(m.left + m.width / 2 - r.left - r.width / 2).toFixed(1)}px`);
      c.style.setProperty("--dy", `${(m.top + m.height / 2 - r.top - r.height / 2).toFixed(1)}px`);
    }
  }
}

function goStep(i) {
  if (!map.help || i < 0 || i >= TOUR.length) return;
  map.steps = { at: i, start: performance.now() };
  renderExplain();
  startMapLoop();
}

// How it works on the map: the example's files as dots of their own around the middle, doing what
// the step says. The letter is read (its dot sends out rings, its name beside it); its passages
// appear where their numbers would sit and fly into it; its lines go to its closest files. Then
// where the words would sit (rings too, the words beside it), a ring sweeping out as the files
// light up, the closest first; rings on the files with the words; lines to the results, each
// answering with a ring.
function drawTour(ctx, project, age, step, r0, dpr, ink, W, H) {
  const clamp = (k) => Math.max(0, Math.min(1, k));
  const grow = (delay, i, gap) => clamp((age - delay - i * gap) / 420);
  const pos = TOUR_FILES.map((f) => project(f.x, f.y, f.z));
  const color = (i) => KIND_COLOR[KIND_GROUP[TOUR_FILES[i].kind]];
  const close = (i) => (TOUR_FILES[i].s - 0.55) / 0.27; // 1: the closest to the words (the match ring's scale)
  const names = tourNames();
  const search = step >= TOUR_SEARCH, ws = step - TOUR_SEARCH;
  const letter = pos[0];
  const dot = (x, y, r, fill, alpha) => {
    ctx.globalAlpha = alpha;
    ctx.fillStyle = fill;
    ctx.beginPath();
    ctx.arc(x, y, r, 0, TAU);
    ctx.fill();
    ctx.globalAlpha = 1;
  };
  const ring = (x, y, r, stroke, width = 2) => {
    ctx.lineWidth = width * dpr;
    ctx.strokeStyle = stroke;
    ctx.beginPath();
    ctx.arc(x, y, r, 0, TAU);
    ctx.stroke();
  };
  const dashed = (from, to, k, i) => {
    if (k <= 0) return;
    ctx.setLineDash([4 * dpr, 4 * dpr]);
    ctx.strokeStyle = ink(0.55 - i * 0.05);
    ctx.lineWidth = 1.3 * dpr;
    ctx.beginPath();
    ctx.moveTo(from[0], from[1]);
    ctx.lineTo(from[0] + (to[0] - from[0]) * k, from[1] + (to[1] - from[1]) * k);
    ctx.stroke();
    ctx.setLineDash([]);
  };
  ctx.font = `600 ${12 * dpr}px ${getComputedStyle(document.body).fontFamily}`;
  ctx.textBaseline = "middle";
  const label = (i, r) => {
    ctx.fillStyle = ink(0.85);
    ctx.fillText(names[i], pos[i][0] + r + 10 * dpr, pos[i][1]);
  };
  // How strong each file is: in the letter's steps the letter, and its closest as their lines reach
  // them; in the search's, by closeness to the words, lit as the sweep reaches them.
  const strength = (i) => {
    if (!search) {
      if (i === 0) return [1, 1.4];
      const near = step === 3 ? TOUR_NEAR.indexOf(i) : -1;
      const k = near >= 0 ? grow(500, near, 170) : 0;
      return [0.25 + 0.6 * k, 1 + 0.2 * k];
    }
    if (ws === 0) return [0.2, 1];
    const w = close(i);
    const k = ws === 1 ? clamp((age - 300 - (1 - w) * 1500) / 300) : 1;
    return [0.15 + (0.1 + 0.85 * w ** 1.5) * k, 0.8 + 0.8 * w * k];
  };
  if (!search && step === 3) TOUR_NEAR.forEach((j, i) => dashed(letter, pos[j], grow(500, i, 170), i));
  const spot = search ? project(0, 0, 0) : null;
  if (spot && ws === 3) TOUR_FILES.slice(0, 5).forEach((_, i) => dashed(spot, pos[i], grow(250, i, 110), i));
  const radius = [];
  TOUR_FILES.forEach((_, i) => {
    const [alpha, size] = strength(i);
    radius[i] = r0 * size;
    dot(pos[i][0], pos[i][1], radius[i], color(i), alpha);
  });
  if (!search) {
    // The letter's passages, where their numbers would sit: they appear, then fly into it.
    if (step === 2 || step === 3) {
      TOUR_PASSAGE_SPOTS.forEach(([x, y, z], i) => {
        const [sx, sy] = project(x, y, z);
        let k = 1, px = sx, py = sy;
        if (step === 2) {
          k = popIn((age - 250 - i * 170) / 380); // (as its numbers appear in the panel)
        } else {
          const f = clamp((age - 300 - i * 45) / 550); // (as they fly into the average there)
          const e = f * f * (3 - 2 * f);
          px = sx + (letter[0] - sx) * e;
          py = sy + (letter[1] - sy) * e;
          k = 1 - f;
        }
        if (k <= 0) return;
        ctx.globalAlpha = Math.min(1, k) * 0.35;
        ctx.strokeStyle = ink(0.6);
        ctx.lineWidth = dpr;
        ctx.beginPath();
        ctx.moveTo(letter[0], letter[1]);
        ctx.lineTo(px, py);
        ctx.stroke();
        ctx.globalAlpha = 1;
        dot(px, py, r0 * 0.9 * k, color(0), Math.min(1, k) * 0.9);
      });
    }
    ring(letter[0], letter[1], radius[0] + 4 * dpr, "#f54e00", 2.5);
    label(0, radius[0] + 4 * dpr);
  } else if (spot) {
    // Where the words would sit: a ring of their own (popping in the first time), the words beside
    // it until the files' names take over.
    const k = ws === 0 ? popIn((age - 300) / 420) : 1;
    if (k > 0) {
      dot(spot[0], spot[1], 6 * dpr * k, `rgb(${theme.card.join(", ")})`, 1);
      ring(spot[0], spot[1], 6 * dpr * k, ink(0.9), 2.5);
      dot(spot[0], spot[1], 2 * dpr * k, ink(0.9), 1);
    }
    if (ws <= 1 && k >= 1) {
      ctx.textAlign = "right"; // (left of it: the letter sits just to its right)
      ctx.fillStyle = ink(0.85);
      ctx.fillText(`“${t("tour.words")}”`, spot[0] - 16 * dpr, spot[1]);
      ctx.textAlign = "left";
    }
    if (ws === 1) { // the search going out from the words
      const k = clamp((age - 300) / 1500);
      if (k > 0 && k < 1) ring(spot[0], spot[1], 10 * dpr + k * Math.hypot(W, H) * 0.5, ink(0.28 * (1 - k)));
    }
    if (ws >= 2) { // the files with the words, ringed (one after another the first time)
      TOUR_FILES.forEach((f, i) => {
        if (!f.words) return;
        const k = ws === 2 ? popIn((age - 250 - i * 160) / 360) : 1;
        if (k <= 0) return;
        ring(pos[i][0], pos[i][1], (radius[i] + 3 * dpr) * k, ink(0.8 * Math.min(1, k)), 1.5);
        label(i, radius[i] + 3 * dpr);
      });
    }
    if (ws === 3) { // the results answer as their lines reach them
      TOUR_FILES.slice(0, 5).forEach((_, i) => {
        const k = clamp((age - 670 - i * 110) / 450);
        if (k > 0 && k < 1) ring(pos[i][0], pos[i][1], radius[i] + 3 * dpr + k * 14 * dpr, ink(0.6 * (1 - k)));
      });
    }
  }
  // (and the rings going out: CSS, smooth while the map stands still)
  ripple($("map-ripple"), !search ? [letter[0] / dpr, letter[1] / dpr, (radius[0] + 4 * dpr) / dpr] : null);
  ripple($("map-ripple-spot"), spot ? [spot[0] / dpr, spot[1] / dpr, 7] : null);
}

// What a step shows in the panel, coming in piece by piece.
function walkVisual() {
  const at = map.steps.at;
  return at < TOUR_SEARCH ? fileVisual(at) : searchVisual(at - TOUR_SEARCH);
}

// (its place in the order things come in)
const nth = (el, i) => { el.style.setProperty("--i", i); return el; };

// The letter's steps, as large as the panel allows: its page is read (a line goes down it), comes
// apart into its passages, each passage gets its numbers beside it (the rows share the room), and
// their numbers fly into the average. Captions under what they name.
function fileVisual(at) {
  const visual = (cls, ...kids) => h("div", { class: `mx-visual${cls ? ` ${cls}` : ""}` }, ...kids);
  const passages = t("tour.letter").split("|");
  const caption = h("div", { class: "mx-caption mx-in late" }, tp("map.passages", passages.length));
  if (at === 0) {
    return visual("",
      nth(h("div", { class: "mx-read mx-in" }, h("div", { class: "mx-letter" }, ...passages.map((text) => h("p", {}, text)))), 0),
      nth(h("div", { class: "mx-file mx-in" }, icon("doc"), h("b", {}, tourNames()[0])), 1),
      nth(h("div", { class: "mx-dir mx-in" }, t("tour.folder")), 2));
  }
  if (at === 1) {
    return visual("", h("div", { class: "mx-pieces" }, ...passages.map((text, i) =>
      nth(h("div", { class: "mx-piece" }, h("span", { class: "mx-num" }, fmt(i + 1)), h("p", {}, text)), i))), caption);
  }
  if (at === 2) {
    // (each row's share of the room, less the caption and the gaps; the numbers as tall as a row)
    const n = passages.length, size = `min(calc((100cqh - ${30 + (n - 1) * 12}px) / ${n}), 36cqw)`;
    return visual("rows", h("div", { class: "mx-rows", style: `--n: ${n}` }, ...passages.map((text, i) =>
      nth(h("div", { class: "mx-row mx-in" }, h("span", { class: "mx-ptext" }, text), printCanvas(TOUR_PRINTS[i], size, 250 + i * 170)), i))), caption);
  }
  const prints = TOUR_PRINTS.map((v, i) => nth(printCanvas(v, 40), i));
  const mean = printCanvas(TOUR_MEAN, "min(80cqw, calc(100cqh - 110px))", 650);
  mean.classList.add("mx-mean");
  return visual("", h("div", { class: "mx-prints merge" }, ...prints), mean, h("div", { class: "mx-caption mx-in late" }, t("map.average")));
}

// The search's steps, as large as the panel allows: the words become numbers, every file by how
// close its meaning is (bars filling up as the map lights up), the files that hold the words
// (counted up, the words marked), the results (one after another). Lists end where the room does.
function searchVisual(s) {
  const visual = (cls, ...kids) => h("div", { class: `mx-visual${cls ? ` ${cls}` : ""}` }, ...kids);
  const names = tourNames();
  const dot = (i) => h("i", { class: "mc-dot", style: `background: ${KIND_COLOR[KIND_GROUP[TOUR_FILES[i].kind]]}` });
  const named = (i, order, ...more) => nth(h("div", { class: "mc-rel mx-result mx-in" }, dot(i), h("span", { class: "mc-rel-name" }, names[i]), ...more), order);
  if (s === 0) {
    return visual("", h("div", { class: "mx-words mx-in" }, `“${t("tour.words")}”`),
      printCanvas(TOUR_WORDS, "min(80cqw, calc(100cqh - 120px))", 450), h("div", { class: "mx-caption mx-in late" }, t("map.numbers")));
  }
  if (s === 1) {
    return visual("wide",
      h("div", { class: "mx-bars" }, ...TOUR_FILES.map((f, i) => {
        const share = Math.max(0, Math.min(1, (f.s - 0.55) / 0.27)); // (the match ring's scale)
        return nth(h("div", { class: `mx-bar-row mx-in${i >= TOUR_LIKELY ? " mx-trim" : ""}`, style: `--hue: ${Math.round(share * 120)}` },
          dot(i), h("span", { class: "mc-rel-name" }, names[i]),
          h("span", { class: "mx-bar" }, h("i", { style: `--w: ${(share * 100).toFixed(1)}%` })), h("b", {}, pct(Math.round(share * 100)))), i);
      })),
      h("div", { class: "mx-caption" }, t("map.closest_meaning")));
  }
  if (s === 2) {
    const n = h("b", { class: "mx-count" }, "0");
    countUp(n, TOUR_LIKELY);
    const words = queryTerms(t("tour.words"));
    const found = [t("tour.letter").split("|")[0], ...t("tour.snippets").split("|")]; // (where each holds the words)
    return visual("", n, h("div", { class: "mx-caption" }, tp("map.with_words", TOUR_LIKELY)),
      h("div", { class: "mx-hits" }, ...found.slice(0, TOUR_LIKELY).map((text, i) => nth(h("div", { class: "mx-hit mx-in" },
        h("div", { class: "mx-hit-name" }, dot(i), h("span", { class: "mc-rel-name" }, names[i])),
        h("p", { class: "mx-snip" }, ...highlighted(text, words))), i + 1))));
  }
  // The likely ones (all the words: a full ring), the line, and the rest (by meaning) as far as the
  // room goes.
  const rows = [
    ...TOUR_FILES.slice(0, TOUR_LIKELY).map((f, i) => named(i, i, matchBadge({ exact: true, similarity: f.s }))),
    nth(h("div", { class: "more-divider static mx-in" }, h("span", { class: "more-label" }, tp("search.less_likely", TOUR_FILES.length - TOUR_LIKELY))), TOUR_LIKELY),
    ...TOUR_FILES.slice(TOUR_LIKELY).map((f, i) => {
      const row = named(TOUR_LIKELY + i, TOUR_LIKELY + 1 + i, matchBadge({ similarity: f.s }));
      if (i) row.classList.add("mx-trim");
      return row;
    }),
  ];
  return visual("wide", h("div", { class: "mc-related mx-results" }, ...rows));
}

// A number counting up to n, from a moment from now.
function countUp(el, n, ms = 700) {
  const t0 = performance.now() + 250;
  const tick = (now) => {
    const k = Math.max(0, Math.min(1, (now - t0) / ms));
    el.textContent = fmt(Math.round(n * (1 - (1 - k) ** 3)));
    if (k < 1 && el.isConnected) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

// 0 to 1 with a little overshoot, for things that pop in.
function popIn(k) {
  if (k <= 0) return 0;
  if (k >= 1) return 1;
  return 1 + 2.8 * (k - 1) ** 3 + 1.8 * (k - 1) ** 2;
}

// A vector as its 16 × 16 grid, `size` wide (pixels, or a CSS length); with `develop`, its squares
// appear in a sweep from the top left, starting that many ms from now.
function printCanvas(v, size, develop = null) {
  const s = typeof size === "number" ? `${size}px` : size;
  const c = h("canvas", { class: "mx-print", width: 160, height: 160, style: `width: ${s}; height: ${s}` });
  if (develop == null) {
    drawFingerprint(v, null, c);
    return c;
  }
  const t0 = performance.now() + develop;
  drawFingerprint(v, null, c, 0);
  const tick = (now) => {
    const k = Math.max(0, Math.min(1, (now - t0) / 650));
    drawFingerprint(v, null, c, k);
    if (k < 1 && (c.isConnected || now < t0)) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  return c;
}

for (const b of $("map-views").querySelectorAll("button")) b.addEventListener("click", () => setMapView(b.dataset.view));
$("map-help").addEventListener("click", () => toggleHelp());

// The column beside the map is as wide as you drag its edge to (kept in the settings; a double-click
// gives it its own width back).
function sideWidth(w) {
  const main = $("map-resize").parentElement;
  if (w > 0) main.style.setProperty("--side-w", `${w}px`);
  else main.style.removeProperty("--side-w");
}

{
  const handle = $("map-resize");
  let drag = null;
  handle.addEventListener("pointerdown", (e) => {
    drag = { w: 0 };
    handle.setPointerCapture(e.pointerId);
    handle.classList.add("drag");
    e.preventDefault();
  });
  handle.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const r = handle.parentElement.getBoundingClientRect();
    drag.w = Math.round(Math.max(260, Math.min(r.width - 320, r.right - e.clientX - handle.offsetWidth / 2)));
    sideWidth(drag.w);
  });
  const end = () => {
    if (!drag) return;
    handle.classList.remove("drag");
    if (drag.w) saveSettings({ map_side: drag.w });
    drag = null;
  };
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
  handle.addEventListener("dblclick", () => {
    sideWidth(0);
    saveSettings({ map_side: 0 });
  });
}

// ---------- kind and folder ----------

// Search and the map show one kind of files, or all: one choice for both, as the folder is.
function setKind(kind) {
  if (state.kind === kind) return;
  state.kind = kind;
  for (const b of document.querySelectorAll("#kinds button, #map-kinds button")) b.classList.toggle("active", b.dataset.kind === kind);
  updateViews();
  runSearch();
  if (map.selected && !visible(map.selected)) clearSelection();
  else if (map.related) setClosest();
  if ($("map-q").value.trim()) mapSearch();
  startMapLoop();
}

// Search and the map can be narrowed to one folder: any folder inside the indexed ones.
function setScope(path) {
  state.scope = path ? { path, name: baseName(path), prefix: `${path.toLowerCase().replace(/[\\/]+$/, "")}\\` } : null;
  renderScopes();
  if (state.similar) runSimilar(state.similar);
  else if ($("q").value.trim()) runSearch();
  if (map.selected && !visible(map.selected)) clearSelection();
  else if (map.related) setClosest();
  if ($("map-q").value.trim()) mapSearch();
  startMapLoop();
}

function renderScopes() {
  const s = state.scope;
  for (const box of [$("scope-search"), $("scope-map")]) {
    const pill = h("button", { type: "button", class: `scope-pill${s ? " on" : ""}`, onclick: () => openScopeMenu(box) },
      icon("folder"), h("span", { class: "scope-name" }, s ? shortDir(s.path) : t("search.all_folders")), icon("chevronDown"));
    setTip(pill, s ? s.path : t("scope.pick"));
    const parts = [pill];
    if (s) {
      const x = h("button", { type: "button", class: "chip-x", onclick: () => setScope(null) }, icon("close"));
      setTip(x, t("scope.clear"));
      parts.push(x);
    }
    box.classList.toggle("on", !!s);
    box.replaceChildren(...parts);
  }
}

// A small folder browser. A click on a folder searches in it; the › at its end shows the folders
// inside it instead. The field on top finds a folder anywhere by its name. Archives and emails are
// folders too, with their own icon.
const DIR_ICON = { archive: "archive", mail: "mail" };
let scopeFindTimer = null;

async function openScopeMenu(box, path = state.scope?.path || "") {
  const menu = $("scope-menu");
  const find = h("input", { type: "search", spellcheck: "false", placeholder: `${t("scope.find")}…`, "aria-label": t("scope.find") });
  const list = h("div", { class: "menu-list" });
  find.addEventListener("input", () => {
    clearTimeout(scopeFindTimer);
    scopeFindTimer = setTimeout(() => fillScopeList(box, list, path, find.value.trim()), 150);
  });
  menu.replaceChildren(h("label", { class: "menu-find" }, icon("search"), find), list);
  if (!(await fillScopeList(box, list, path, ""))) return;
  menu.hidden = false;
  const r = box.getBoundingClientRect();
  menu.style.transform = `translate(${Math.round(Math.min(r.left, innerWidth - menu.offsetWidth - 8))}px, ${Math.round(r.bottom + 6)}px)`;
}

// The menu's rows: the folders in `path`, or those named like `text` anywhere. (Only the last one
// asked for is shown: typing and opening can overtake each other.)
let scopeAsked = 0;
async function fillScopeList(box, list, path, text) {
  const asked = ++scopeAsked;
  let data;
  try {
    data = await api(`/api/dirs?${new URLSearchParams(text ? { find: text } : { path })}`);
  } catch (e) {
    toast(e.message, "alert");
    return false;
  }
  if (asked !== scopeAsked) return true;
  const choose = (p) => { setScope(p); closeScopeMenu(); };
  const isScope = (p) => state.scope?.path.toLowerCase() === p.toLowerCase();
  const row = (name, label, count, onclick, { on = false, open = null } = {}) => {
    const main = h("button", { type: "button", class: `menu-row${on ? " on" : ""}`, onclick },
      icon(name), h("span", { class: "menu-name" }, label), on ? menuMark("check") : null,
      count != null ? h("span", { class: "menu-count" }, fmt(count)) : null);
    if (!open) return main;
    const more = h("button", { type: "button", class: "menu-open", onclick: open }, icon("chevronRight"));
    setTip(more, t("scope.inside"));
    return h("div", { class: "menu-line" }, main, more);
  };
  const rows = [];
  if (text) {
    for (const d of data.dirs) {
      const label = shortDir(d.path);
      const el = row(DIR_ICON[d.kind] || "folder", label, d.count, () => choose(d.path), { on: isScope(d.path) });
      setTip(el, d.path);
      rows.push(el);
    }
    if (!data.dirs.length) rows.push(h("div", { class: "menu-empty" }, t("scope.none_found")));
    list.replaceChildren(...rows);
    return true;
  }
  const roots = (state.last?.folders || []).map((f) => f.path.toLowerCase());
  const parent = !path ? null : roots.includes(path.toLowerCase()) ? "" : path.replace(/[\\/][^\\/]+[\\/]?$/, "");
  if (state.scope) rows.push(row("globe", t("scope.clear"), null, () => { setScope(null); closeScopeMenu(); }));
  if (parent !== null) {
    // Where you are: back a level on the left, then the way down from the indexed folder to this
    // one, this one in ink, with the files in it you can find; a click on it searches in it.
    const back = h("button", { type: "button", class: "menu-open menu-back", onclick: () => fillScopeList(box, list, parent, "") },
      icon("chevronLeft"));
    setTip(back, parent ? baseName(parent) : t("search.all_folders"));
    const crumbs = shortDir(path).split(" › ");
    const last = crumbs.pop();
    const here = h("button", { type: "button", class: `menu-row menu-here${isScope(path) ? " on" : ""}`, onclick: () => choose(path) },
      h("span", { class: "menu-name crumbs" }, crumbs.length ? h("span", { class: "crumbs-up" }, `${crumbs.join(" › ")} ›`) : null,
        h("b", {}, last)),
      isScope(path) ? menuMark("check") : null,
      data.count != null ? h("span", { class: "menu-count" }, fmt(data.count)) : null);
    setTip(here, `${path}\n${t("scope.here")}`);
    rows.push(h("div", { class: "menu-line" }, back, here), h("div", { class: "menu-sep" }));
  }
  for (const d of data.dirs) {
    rows.push(row(DIR_ICON[d.kind] || "folder", d.name, d.count, () => choose(d.path),
      { on: isScope(d.path), open: () => fillScopeList(box, list, d.path, "") }));
  }
  if (!data.dirs.length) rows.push(h("div", { class: "menu-empty" }, t("scope.no_subfolders")));
  list.replaceChildren(...rows);
  return true;
}

const closeScopeMenu = () => { $("scope-menu").hidden = true; };
document.addEventListener("pointerdown", (e) => {
  if (!$("scope-menu").hidden && !e.target.closest("#scope-menu, .scope")) closeScopeMenu();
}, true);

// ---------- right-click ----------

// The app window has no browser menu. Right-click offers to copy marked text, and in a text field
// to cut, copy, paste and select all. Elsewhere it does nothing.
function openContextMenu(e) {
  const field = e.target.closest?.("input[type=search], input:not([type]), input[type=text], textarea");
  const marked = String(getSelection() || "");
  if (!field && !marked.trim()) return closeContextMenu();
  e.preventDefault();
  hideTip();
  const ctrl = t("keys.ctrl");
  const item = (label, keys, enabled, run) => h("button", {
    type: "button", class: "menu-row", role: "menuitem", disabled: !enabled,
    onclick: () => { closeContextMenu(); run(); },
  }, h("span", { class: "menu-name" }, label), h("span", { class: "menu-count" }, keys));
  const picked = field && field.selectionStart !== field.selectionEnd;
  const rows = field ? [
    item(t("edit.cut"), `${ctrl}+X`, picked, () => { field.focus(); document.execCommand("cut"); }),
    item(t("edit.copy"), `${ctrl}+C`, picked, () => { field.focus(); document.execCommand("copy"); }),
    item(t("edit.paste"), `${ctrl}+V`, true, () => pasteInto(field)),
    item(t("edit.select_all"), `${ctrl}+A`, !!field.value, () => { field.focus(); field.select(); }),
  ] : [item(t("edit.copy"), `${ctrl}+C`, true, () => copyText(marked))];
  const menu = $("ctx-menu");
  menu.replaceChildren(...rows);
  menu.hidden = false;
  const { clientWidth: w, clientHeight: hgt } = document.documentElement; // stays inside the page
  const x = Math.max(8, Math.min(e.clientX, w - menu.offsetWidth - 8));
  const y = Math.max(8, Math.min(e.clientY, hgt - menu.offsetHeight - 8));
  menu.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
}

const closeContextMenu = () => { $("ctx-menu").hidden = true; };

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    document.execCommand("copy"); // the marked text is still marked
  }
}

async function pasteInto(field) {
  field.focus();
  try {
    field.setRangeText(await navigator.clipboard.readText(), field.selectionStart, field.selectionEnd, "end");
    field.dispatchEvent(new Event("input", { bubbles: true }));
  } catch {
    toast(t("edit.paste_failed"), "alert"); // the clipboard can't be read here; the keyboard still works
  }
}

document.addEventListener("contextmenu", openContextMenu);
document.addEventListener("pointerdown", (e) => { if (!e.target.closest("#ctx-menu")) closeContextMenu(); }, true);
window.addEventListener("blur", closeContextMenu);
window.addEventListener("scroll", closeContextMenu, true);

// ---------- keyboard ----------

document.addEventListener("keydown", (e) => {
  hideTip();
  if (!$("ctx-menu").hidden && e.key === "Escape") {
    e.preventDefault();
    closeContextMenu();
    return;
  }
  if (!$("folder-modal").hidden) {
    if (e.key === "Escape") { e.preventDefault(); closeFolderInfo(); }
    return;
  }
  if (!$("types-modal").hidden) {
    if (e.key === "Escape") { e.preventDefault(); closeTypes(); }
    return;
  }
  if (!$("confirm-modal").hidden) { // Enter presses the focused button by itself
    if (e.key === "Escape") { e.preventDefault(); confirmDone?.(false); }
    return;
  }
  if (!$("scope-menu").hidden && e.key === "Escape") {
    e.preventDefault();
    closeScopeMenu();
    return;
  }
  if (!$("sort-menu").hidden && e.key === "Escape") {
    e.preventDefault();
    closeSortMenu();
    return;
  }
  if (!$("update-modal").hidden) {
    if (e.key === "Escape") { e.preventDefault(); closeUpdateModal(); }
    return;
  }
  if (!$("lightbox").hidden) {
    if (e.key === "Escape" || e.key === " ") closeLightbox();
    else if (e.key === "ArrowLeft") stepLightbox(-1);
    else if (e.key === "ArrowRight") stepLightbox(1);
    else if (e.key === "Enter") openFile(state.results[state.lightbox]);
    else return;
    e.preventDefault();
    return;
  }
  const q = $("q");
  const typingElsewhere = e.target.matches("input, select, textarea") && e.target !== q;
  if (typingElsewhere) return;

  if ((e.key === "/" && document.activeElement !== q) || (e.ctrlKey && (e.key === "k" || e.key === "f"))) {
    e.preventDefault();
    goTo("search");
    q.focus();
    q.select();
    return;
  }
  if (state.page === "map" && map.help && e.key === "Escape") {
    e.preventDefault();
    toggleHelp(false);
    return;
  }
  if (state.page === "map" && map.selected) { // ← (or Backspace): back to the file before; →: forward
    if (e.key === "Escape") clearSelection();
    else if (e.key === "ArrowLeft" || e.key === "Backspace") stepBack();
    else if (e.key === "ArrowRight") stepForward();
    else return;
    e.preventDefault();
    return;
  }
  if (state.page !== "search") return;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    if (!state.results.length) return;
    e.preventDefault();
    const n = visibleResults();
    const step = e.key === "ArrowDown" ? 1 : -1;
    select(state.selected < 0 ? (step > 0 ? 0 : n - 1) : (state.selected + step + n) % n);
  } else if (e.key === "Enter" && state.selected >= 0) {
    e.preventDefault();
    const r = state.results[state.selected];
    if (e.ctrlKey) revealFile(r);
    else if (e.shiftKey) openLightbox(state.selected);
    else openFile(r);
  } else if (e.key === " " && state.selected >= 0 && document.activeElement !== q) {
    e.preventDefault(); // (in the search box, a space is a space)
    openLightbox(state.selected);
  } else if (e.key === "Escape") {
    if (state.selected >= 0) select(-1);
    else if (state.similar) clearSimilar();
    else if (q.value) { q.value = ""; runSearch(); }
  }
});

// ---------- start ----------

async function start() {
  fillIcons();
  try {
    const res = await fetch("/static/i18n.json");
    i18n.catalog = await res.json();
  } catch { /* keep English from the page */ }
  let first = null;
  try { first = await api("/api/status"); } catch {}
  state.appMode = !!first?.app_mode;
  if (first) {
    for (const key of ["view_files", "view_images"]) {
      state.views[key] = state.viewSaved[key] = first.settings[key] || state.views[key];
    }
  }
  updateViews();
  applyTheme(); // reads the theme's colours, and tells the window's title bar
  setLanguage(first ? first.settings.language_resolved : "en");
  route();
  renderEmpty();
  if (first) render(first);
  poll();
}

start();
