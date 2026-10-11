"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  page: "search", kind: "all", scope: null, similar: null,
  foldersKey: "", settingsKey: "", deviceKey: "", folderInfo: null,
  ready: false, appMode: false, hasFolders: null,
  results: [], selected: -1, terms: [], lightbox: -1,
  update: null, resultShown: false, applying: false, last: null, statusClick: null,
  views: { view_files: "cards", view_images: "grid" }, viewSaved: {},
};
let searchCtl = null;
let searchTimer = null;
let toastTimer = null;
let pollTimer = null;

const EXT_COLORS = {
  pdf: "#d93025", doc: "#1d4aff", docx: "#1d4aff", ppt: "#e8590c", pptx: "#e8590c",
  xls: "#2f9e44", xlsx: "#2f9e44", csv: "#2f9e44", tsv: "#2f9e44", md: "#7048e8",
  txt: "#6b6d65", html: "#e8590c", htm: "#e8590c", json: "#b17816", yaml: "#b17816",
  yml: "#b17816", toml: "#b17816", py: "#1d4aff", js: "#b17816", ts: "#1d4aff",
  rs: "#e8590c", go: "#0c8599", ipynb: "#e8590c",
};

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
  gauge: '<path d="M4.5 17a8 8 0 1 1 15 0"/><path d="M12 14l4-4.5"/>',
  speed1: '<path d="M6 18v-3"/><path class="off" d="M12 18v-7M18 18V6"/>',
  speed2: '<path d="M6 18v-3M12 18v-7"/><path class="off" d="M18 18V6"/>',
  speed3: '<path d="M6 18v-3M12 18v-7M18 18V6"/>',
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
  up: '<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>',
  moon: '<path d="M19.5 14.5A8 8 0 1 1 9.5 4.5a6.5 6.5 0 0 0 10 10z"/>',
  sun: '<circle cx="12" cy="12" r="3.8"/><path d="M12 3v1.8M12 19.2V21M3 12h1.8M19.2 12H21M5.6 5.6l1.3 1.3M17.1 17.1l1.3 1.3M5.6 18.4l1.3-1.3M17.1 6.9l1.3-1.3"/>',
  monitor: '<rect x="3" y="4.5" width="18" height="12" rx="1.8"/><path d="M9 20h6M12 16.5V20"/>',
  contrast: '<circle cx="12" cy="12" r="8.5"/><path class="solid" d="M12 3.5a8.5 8.5 0 0 1 0 17z"/>',
  cube: '<path d="M12 3 20 7.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5 12 12l8-4.5M12 12v9"/>',
  focus: '<circle cx="12" cy="12" r="3"/><path d="M4 9V5.5A1.5 1.5 0 0 1 5.5 4H9M15 4h3.5A1.5 1.5 0 0 1 20 5.5V9M20 15v3.5a1.5 1.5 0 0 1-1.5 1.5H15M9 20H5.5A1.5 1.5 0 0 1 4 18.5V15"/>',
  mouse: '<rect x="6.5" y="3.5" width="11" height="17" rx="5.5"/><path d="M12 7.5v3"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5M12 7.6v.2"/>',
  skip: '<path d="M5 6.5 12 12l-7 5.5zM12 6.5 19 12l-7 5.5z"/>',
  ram: '<rect x="3" y="6.5" width="18" height="10" rx="1.5"/><path d="M7.5 10v3M12 10v3M16.5 10v3M6.5 16.5v2.5M10 16.5v2.5M14 16.5v2.5M17.5 16.5v2.5"/>',
  leaf: '<path d="M5 19.5C4.5 11 9.5 5 19.5 4.5 19.5 14 14 19.5 5 19.5z"/><path d="M5 19.5 13.5 11"/>',
  bolt: '<path d="M13.5 3 5.5 13.5h6l-1 7.5 8-10.5h-6z"/>',
  battery: '<rect x="2.5" y="7.5" width="16.5" height="9" rx="2"/><path d="M21.5 10.5v3M6 10.5v3"/>',
  away: '<path d="M20 14.5A8.5 8.5 0 1 1 9.5 4a7 7 0 0 0 10.5 10.5z"/><path d="M15 4.5h3.5L15 8.5h3.5"/>',
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

function setTip(el, text) {
  el.dataset.tip = text;
  // Icon-only controls get the tooltip's title as their accessible name.
  if (el.matches("button, a, input, select") && !el.textContent.trim()) {
    el.setAttribute("aria-label", text.split("\n")[0]);
  }
  if (tipFor === el) showTip(el);
}

function showTip(el) {
  const text = el.dataset.tip;
  if (!text || !el.isConnected) return hideTip();
  tipFor = el;
  const box = renderTip(text);
  const r = el.getBoundingClientRect();
  if (el.closest(".rail")) placeTip(r.right + 10, r.top + r.height / 2 - box.height / 2, box);
  else placeTip(r.left + r.width / 2 - box.width / 2, r.top - box.height - 8, box, r.bottom + 8);
}

// For things that aren't elements, like a point on the map: the tip sits above the cursor.
function showTipAt(x, y, text) {
  clearTimeout(tipTimer);
  tipFor = "point";
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
  tipEl.hidden = true;
}

// A tooltip that only repeats a name shows while the name is cut off (data-tip-cut: which child).
function tipWanted(el) {
  if (!el || !("tipCut" in el.dataset)) return el;
  const n = (el.dataset.tipCut && el.querySelector(el.dataset.tipCut)) || el;
  return n.scrollWidth > n.clientWidth ? el : null;
}

document.addEventListener("pointerover", (e) => {
  const el = tipWanted(e.target.closest?.("[data-tip]") || null);
  if (el === tipFor) return;
  clearTimeout(tipTimer);
  if (!el) return hideTip();
  showTip(el);
});
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
  setTip($("map-empty"), t("map.empty"));
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
      [["Shift", "Enter"], "keys.preview"],
      [["Esc"], "keys.clear"],
    ]],
    ["cube", "nav.map", [
      [[{ mouse: "map.drag" }], "map.rotate"],
      [[{ mouse: "map.scroll" }], "map.zoom"],
      [[{ mouse: "map.click" }], "map.inspect"],
      [["←"], "map.back_short"],
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
const thumbUrl = (r) => `/api/thumb/${r.file_id}?v=${Math.floor(r.mtime)}-${r.size}`;
const previewUrl = (r) => `/api/preview/${r.file_id}?v=${Math.floor(r.mtime)}-${r.size}`;

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
setInterval(() => { if (!$("q").value) rotatePlaceholder(); }, 4500);

// ---------- pages ----------

function route() {
  const page = (location.hash.match(/^#\/(\w+)/) || [])[1];
  const pages = ["search", "map", "folders", "settings"];
  state.page = pages.includes(page) ? page : "search";
  for (const p of pages) $(`page-${p}`).hidden = p !== state.page;
  for (const a of document.querySelectorAll(".nav a")) {
    a.classList.toggle("active", a.dataset.page === state.page);
  }
  hideTip();
  closeScopeMenu();
  if (state.page === "search") $("q").focus();
  if (state.page === "map") openMap();
  if (state.page === "folders" && state.last) showProblemsBadge(state.last.folders); // seen: the badge goes
}

window.addEventListener("hashchange", route);

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
  renderFolders(s.folders, s.progress, phase);
  renderSettings(s);
  renderUpdate(s);
}

// ---------- status (bottom of the rail) ----------

// What the indexer is doing with a folder right now. One answer for the status ring, the
// folders list and the tray, so they never contradict each other.
function workPhase({ progress: p, resources }) {
  if (p.state !== "indexing" && p.state !== "scanning") return null;
  if (p.paused) return "paused"; // you pressed pause
  if (resources.decision.duty === 0 || p.note) return "waiting"; // a game, memory running out…
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

function renderActivity({ model, progress: p, folders, resources }, phase) {
  const wasReady = state.ready;
  state.ready = model.status === "ready";
  const resting = model.status === "asleep";
  const d = resources.decision;
  const total = folders.reduce((a, f) => a + f.indexed, 0);
  let s;

  if (model.status === "downloading") {
    s = { kind: "busy", icon: "download", spin: true, lines: [t("activity.downloading"), t("activity.downloading_sub")] };
  } else if (model.status !== "ready" && model.status !== "error" && !resting) {
    s = { kind: "busy", icon: "hourglass", spin: true, lines: [t("activity.waking"), t("activity.waking_sub")] };
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
      icon: { paused: "pause", waiting: "hourglass", scanning: "search" }[phase],
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
// goes away once the folders were looked at. How many problems were already seen stays here.
const BADGE_SEEN = "quiekel.problemsSeen";

function problemsSeen() {
  try { return Number(localStorage.getItem(BADGE_SEEN)) || 0; } catch { return 0; }
}

function setProblemsSeen(n) {
  try { localStorage.setItem(BADGE_SEEN, String(n)); } catch {}
}

function showProblemsBadge(folders) {
  const problems = folders.reduce((a, f) => a + f.errors, 0);
  if (state.page === "folders" || problems < problemsSeen()) setProblemsSeen(problems);
  const badge = $("nav-badge");
  const show = problems > problemsSeen();
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

function renderFolders(folders, progress, phase) {
  const active = phase ? progress.folder : null;
  const key = JSON.stringify([folders, active, phase, progress.done, progress.total, Math.floor(Date.now() / 60000)]);
  const hadFolders = state.hasFolders;
  state.hasFolders = folders.length > 0;
  showProblemsBadge(folders);
  if (hadFolders !== state.hasFolders && !$("q").value.trim() && !state.similar) renderEmpty();
  if (key === state.foldersKey) return;
  state.foldersKey = key;

  const list = $("folders");
  list.replaceChildren();
  if (!folders.length) {
    list.append(h("div", { class: "empty compact" }, h("img", { class: "logo", src: "/static/pig.svg", alt: "" }), waitingText()));
  }
  for (const f of folders) {
    const here = active === f.path;
    const paused = here && phase === "paused";
    // Clicking the folder searches inside it.
    const main = h("button", { type: "button", class: "folder-main", onclick: () => { setScope(f.path); goTo("search"); } },
      h("span", { class: "folder-icon" }, icon("folder")),
      h("span", { class: "folder-text" },
        h("span", { class: "folder-name" }, baseName(f.path)),
        h("span", { class: "folder-path" }, f.path)));
    setTip(main, t("scope.here"));
    list.append(h("div", { class: `folder${here ? ` active ${phase}` : ""}` },
      main,
      h("div", { class: "stats" },
        stat("doc", fmt(f.indexed), t("folders.col.files")),
        stat("image", fmt(f.images), t("folders.col.images")),
        f.errors ? stat("alert", fmt(f.errors), tp("folders.unreadable", f.errors), "bad", () => openFolderInfo(f)) : null,
        here ? phaseStat(phase, progress) : stat("clock", ago(f.last_scan_at), t("folders.col.scanned"))),
      h("div", { class: "folder-actions" },
        // The folder being worked on can be paused right here (it's the same pause as in the rail).
        here ? iconButton(paused ? "play" : "pause", t(paused ? "activity.resume" : "activity.pause"),
          () => post(paused ? "/api/resume" : "/api/pause").then(refresh).catch((e) => toast(e.message, "alert")),
          paused ? "primary" : "") : null,
        iconButton("info", t("folder.details"), () => openFolderInfo(f)),
        iconButton(f.watch ? "eye" : "eyeOff", t("folders.watch_title"),
          () => post(`/api/folders/${f.id}/watch`, { watch: !f.watch }).then(refresh), f.watch ? "on" : ""),
        iconButton("refresh", t("folders.rescan_title"),
          () => post(`/api/folders/${f.id}/rescan`).then(() => { toast(t("folders.rescan_queued"), "refresh"); refresh(); })),
        iconButton("trash", t("folders.remove_title"), () => removeFolder(f), "danger")),
    ));
  }
  // A folder that was searched in is gone: search everywhere again.
  if (state.scope && !folders.some((f) => state.scope.prefix.startsWith(`${f.path.toLowerCase().replace(/[\\/]+$/, "")}\\`))) {
    setScope(null);
  }
}

// Reasons the indexer knows ("!damaged_image") come as keys; anything else as it was reported.
const problemText = (error) => (error?.startsWith("!") ? t(`problem.${error.slice(1)}`) : error);

// ---------- a folder's details ----------
//
// Its numbers, the files that couldn't be read, and everything that isn't searched, by reason. A
// new unreadable file shows as a red mark on the folder once: looking here counts as seen. "Try
// again" reads them anew (a rescan does too).

const WHY_ICON = {
  type: "help", off: "eyeOff", media: "film", program: "app", mailbox: "mail", hidden: "eyeOff",
  office_temp: "hourglass", program_folder: "folder", link: "link", too_many: "archive", encrypted: "lock",
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
  const name = baseName(path);
  return h("div", { class: `fm-file${isNew ? " new" : ""}` },
    h("span", { class: "fm-file-text" }, h("b", {}, name), why ? h("span", { class: "fm-why" }, why) : null,
      h("span", { class: "fm-where" }, path.slice(0, -name.length - 1))),
    iconButton("folder", t("result.reveal"), reveal, "small"));
}

// One reason: what it is and how many; opened, which types and a few of the files.
function reasonRow(r) {
  const label = r.key ? t(`why.${r.key}`) : r.raw;
  const more = r.n - r.files.length;
  const exts = Object.entries(r.exts || {}).sort((a, b) => b[1] - a[1]).slice(0, 40)
    .map(([ext, n]) => h("span", { class: "type-chip static" }, ext || t("why.no_ext"), h("small", {}, fmt(n))));
  return h("details", { class: "fm-reason" },
    h("summary", {}, icon(WHY_ICON[r.key] || "skip"),
      h("span", { class: "fm-file-text" }, h("b", {}, label), r.key ? h("span", { class: "fm-what" }, t(`why.${r.key}_d`)) : null),
      h("span", { class: "fm-count" }, fmt(r.n)), icon("chevronDown")),
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
    cell("text", "folder.passages", fmt(f.chunks)),
    cell("archive", "folder.size", bytes(f.bytes || 0)),
  );
}

async function openFolderInfo(f) {
  hideTip();
  state.folderInfo = f.id;
  $("fm-name").textContent = baseName(f.path);
  $("fm-path").textContent = f.path;
  renderFolderSummary(f);
  $("fm-problems").hidden = $("fm-skips").hidden = true;
  $("fm-list").replaceChildren();
  $("fm-reasons").replaceChildren();
  $("folder-modal").hidden = false;
  let d;
  try {
    d = await api(`/api/folders/${f.id}/details`);
  } catch (e) {
    toast(e.message, "alert");
    return;
  }
  if (state.folderInfo !== f.id) return;
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
// Every type the app reads, by group: click one to switch it off or on. Types it doesn't know
// can be added: they're read as plain text. And the rules for archives, emails and hidden files.

const TYPE_ICON = { doc: "doc", mail: "mail", text: "text", code: "code", image: "image", raw: "camera", archive: "archive" };
const TYPE_RULES = [["attachments", "mail"], ["nested_archives", "archive"], ["big_archives", "archive"],
  ["hidden_files", "eyeOff"], ["program_folders", "folder"]];
const SHOWN_TYPES = 18; // a long group shows this many until it's opened up

async function openTypes() {
  hideTip();
  try {
    state.types = await api("/api/filetypes");
  } catch (e) {
    toast(e.message, "alert");
    return;
  }
  state.typesOpen = {};
  renderTypes();
  $("types-modal").hidden = false;
}

const closeTypes = () => { $("types-modal").hidden = true; };

function renderTypes() {
  const m = state.types;
  const off = new Set(m.off);
  const groups = m.groups.map(({ key, exts }) => {
    const on = exts.filter((ext) => !off.has(ext)).length;
    const all = h("input", { type: "checkbox", class: "switch", "aria-label": t(`types.group.${key}`) });
    all.checked = on > 0;
    all.addEventListener("change", () => saveTypes({
      types_off: all.checked ? m.off.filter((ext) => !exts.includes(ext)) : [...new Set([...m.off, ...exts])] }));
    const open = state.typesOpen[key] || exts.length <= SHOWN_TYPES + 2;
    const chips = (open ? exts : exts.slice(0, SHOWN_TYPES)).map((ext) => h("button", {
      type: "button", class: `type-chip${off.has(ext) ? " off" : ""}`, "aria-pressed": String(!off.has(ext)),
      onclick: () => saveTypes({ types_off: off.has(ext) ? m.off.filter((e) => e !== ext) : [...m.off, ext] }),
    }, ext));
    if (!open) {
      chips.push(h("button", { type: "button", class: "type-chip more",
        onclick: () => { state.typesOpen[key] = true; renderTypes(); } }, t("types.more", { n: fmt(exts.length - SHOWN_TYPES) })));
    }
    return h("section", { class: "tm-group" },
      h("div", { class: "tm-head" }, icon(TYPE_ICON[key]), h("b", {}, t(`types.group.${key}`)),
        h("span", { class: "tm-count" }, t("types.count", { on: fmt(on), all: fmt(exts.length) })), all),
      h("div", { class: "tm-chips" }, ...chips));
  });

  const input = h("input", { class: "input", id: "tm-add-input", placeholder: ".xyz", spellcheck: "false", maxlength: "17",
    "aria-label": t("types.add") });
  const add = () => { if (input.value.trim()) saveTypes({ types_added: [...m.added, input.value.trim()] }, true); };
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") add(); });
  const added = h("section", { class: "tm-group" },
    h("div", { class: "tm-head" }, icon("plus"), h("b", {}, t("types.added_title")), h("span", { class: "tm-count" }, t("types.added_hint"))),
    h("div", { class: "tm-chips" },
      ...m.added.map((ext) => h("span", { class: "type-chip added" }, ext,
        iconButton("close", t("types.remove"), () => saveTypes({ types_added: m.added.filter((e) => e !== ext) }), "tiny"))),
      h("span", { class: "tm-add" }, input, iconButton("plus", t("types.add"), add))));

  const rules = TYPE_RULES.map(([key, ic]) => {
    const sw = h("input", { type: "checkbox", class: "switch", "aria-label": t(`types.rule.${key}`) });
    sw.checked = !!m.rules[key];
    sw.addEventListener("change", () => saveTypes({ [key]: sw.checked }));
    return h("label", { class: "tm-rule" }, icon(ic),
      h("span", { class: "fm-file-text" }, h("b", {}, t(`types.rule.${key}`)), h("span", { class: "fm-what" }, t(`types.rule.${key}_d`))), sw);
  });
  $("tm-body").replaceChildren(...groups, added,
    h("section", { class: "tm-group" }, h("div", { class: "tm-head" }, icon("settings"), h("b", {}, t("types.rules_title"))),
      h("div", { class: "tm-rules" }, ...rules)),
    h("p", { class: "help-note" }, t("types.note")));
  $("tm-reset").disabled = !m.off.length && !m.added.length && TYPE_RULES.every(([key]) => m.rules[key] === m.defaults[key]);
}

async function saveTypes(change, adding = false) {
  try {
    await post("/api/settings", change);
    state.types = await api("/api/filetypes");
    renderTypes();
    if (adding) $("tm-add-input").focus();
    state.settingsKey = "";
    refresh();
  } catch (e) { toast(e.message, "alert"); }
}

$("types-open").addEventListener("click", openTypes);
$("tm-close").addEventListener("click", closeTypes);
$("types-modal").addEventListener("click", (e) => { if (e.target.id === "types-modal") closeTypes(); });
$("tm-reset").addEventListener("click", () => saveTypes({ types_off: [], types_added: [], ...state.types.defaults }));

async function removeFolder(f) {
  const yes = await confirmDialog({
    title: t("folders.confirm_title", { name: baseName(f.path) }), text: t("folders.confirm_text"), ok: t("folders.forget"),
  });
  if (!yes) return;
  try {
    await api(`/api/folders/${f.id}`, { method: "DELETE" });
    toast(t("folders.removed"), "trash");
    refresh();
  } catch (e) { toast(e.message, "alert"); }
}

async function addFolder(path) {
  $("folder-error").hidden = true;
  try {
    const f = await post("/api/folders", { path });
    $("path-input").value = "";
    toast(t("folders.added", { name: baseName(f.path) }), "folderPlus");
    refresh();
  } catch (e) {
    goTo("folders");
    $("folder-error").textContent = e.message;
    $("folder-error").hidden = false;
  }
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

$("pick-btn").addEventListener("click", async (e) => {
  e.currentTarget.disabled = true;
  await pickFolder();
  $("pick-btn").disabled = false;
});

$("path-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const p = $("path-input").value.trim();
  if (p) addFolder(p);
});

// ---------- settings ----------

const shortGpu = (name) => (name || "GPU").replace(/^NVIDIA\s+/i, "").replace(/^GeForce\s+/i, "");

function renderSettings({ settings, resources, model }) {
  const key = JSON.stringify([settings, i18n.lang]);
  if (key !== state.settingsKey) {
    state.settingsKey = key;
    for (const b of $("modes").querySelectorAll("button")) {
      const mode = b.dataset.mode;
      const on = mode === settings.perf_mode;
      b.classList.toggle("active", on);
      b.setAttribute("aria-checked", on);
      setTip(b, t(`mode.${mode}`));
    }
    const changed = [settings.types_off.length && t("types.summary_off", { n: fmt(settings.types_off.length) }),
      settings.types_added.length && t("types.summary_added", { n: fmt(settings.types_added.length) })].filter(Boolean);
    $("types-summary").textContent = changed.length ? changed.join(" · ") : t("types.all_on");
    setTip($("types-open"), t("types.edit"));
    $("autostart-row").hidden = settings.autostart == null;
    $("autostart").checked = !!settings.autostart;
    $("autostart").setAttribute("aria-label", t("settings.autostart"));
    $("check-updates").checked = !!settings.check_updates;
    setTip($("check-updates"), t("settings.updates_check"));
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
          if (key === viewKey() && state.results.length) renderResults(state.results);
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
  renderDevice(model, m);
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

// Where the model runs, plus "you're away", as small chips.
function renderDevice(model, m) {
  const away = m.idle_s >= 180;
  const key = JSON.stringify([model.status, model.on_gpu, model.cuda, model.gpu_name, model.engine, away, i18n.lang]);
  if (key === state.deviceKey) return;
  state.deviceKey = key;
  const chips = [];
  if (model.status === "ready") {
    const card = model.engine === "directml" ? `${model.gpu_name} (DirectML)` : model.gpu_name;
    const where = model.on_gpu ? card : t(model.cuda ? "device.cpu_released" : "device.cpu");
    chips.push(stat("chip", model.on_gpu ? shortGpu(model.gpu_name) : "CPU", t("settings.model_on", { device: where })));
    if (model.engine === "cpu") chips.push(stat("info", t("settings.no_gpu"), t("settings.cpu_only"))); // why indexing is slow
  } else {
    const k = { loading: "model.loading", downloading: "model.downloading", error: "model.error" }[model.status] || "model.not_loaded";
    chips.push(stat("hourglass", "", t("settings.model_state", { state: t(k) })));
  }
  if (away) chips.push(stat("moon", "", t("settings.away")));
  $("device").replaceChildren(...chips);
}

for (const b of $("modes").querySelectorAll("button")) {
  b.addEventListener("click", () => saveSettings({ perf_mode: b.dataset.mode }));
}
$("autostart").addEventListener("change", (e) => saveSettings({ autostart: e.target.checked }));
$("check-updates").addEventListener("change", (e) => saveSettings({ check_updates: e.target.checked }));
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

function renderUpdate({ update: u }) {
  state.update = u;
  $("version").textContent = `v${u.current}`;
  setTip($("brand"), `Quiekel Embed v${u.current}`);

  // What the last check found goes with the button that checks (a new version also gets a badge).
  const found = u.latest ? t("update.available", { version: u.latest.version })
    : u.error ? t(u.error.key, u.error.params)
    : u.last_check ? t("update.up_to_date", { ago: ago(u.last_check) }) : "";
  setTip($("check-now"), found ? `${t("update.check")}\n${found}` : t("update.check"));

  $("update-badge").hidden = !u.latest;
  if (u.latest) setTip($("update-badge"), t("update.sticker", { version: u.latest.version }));

  if (u.last_result && !state.resultShown) {
    state.resultShown = true;
    const r = u.last_result;
    toast(r.ok ? t("update.done", { tag: r.tag }) : t("update.failed", { error: r.error }), r.ok ? "check" : "alert");
  }
}

function openUpdateModal() {
  const u = state.update;
  if (!u || !u.latest) return;
  const l = u.latest;
  $("um-title").textContent = t("update.modal_title", { version: l.version });
  $("um-sub").textContent = `v${u.current} → v${l.version}`
    + (l.published ? ` · ${new Date(l.published).toLocaleDateString(i18n.lang)}` : "");
  $("um-notes").textContent = plainNotes(l.notes);
  $("um-error").hidden = true;
  const hint = $("um-hint");
  hint.hidden = u.can_apply;
  hint.textContent = t(u.git_checkout ? "update.app_only" : "update.not_git");
  const apply = $("um-apply");
  apply.hidden = !u.can_apply;
  apply.disabled = state.applying;
  apply.classList.toggle("working", state.applying);
  $("update-modal").hidden = false;
}

const closeUpdateModal = () => { $("update-modal").hidden = true; };

$("update-badge").addEventListener("click", openUpdateModal);
$("um-later").addEventListener("click", closeUpdateModal);
$("um-github").addEventListener("click", () => post("/api/update/open").catch((e) => toast(e.message, "alert")));
$("update-modal").addEventListener("click", (e) => { if (e.target.id === "update-modal") closeUpdateModal(); });
$("um-apply").addEventListener("click", async () => {
  const apply = $("um-apply");
  state.applying = true;
  apply.disabled = true;
  apply.classList.add("working");
  $("um-error").hidden = true;
  try {
    await post("/api/update/apply");
    $("um-hint").hidden = false;
    $("um-hint").textContent = t("update.restarting");
  } catch (e) {
    state.applying = false;
    apply.disabled = false;
    apply.classList.remove("working");
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
    if (u.latest) openUpdateModal();
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
  if (state.similar) return runSimilar(state.similar);
  if (!q) return renderResults(null);
  const qs = filtersQuery();
  qs.set("q", q);
  state.terms = queryTerms(q);
  await fetchResults(`/api/search?${qs}`);
}

async function runSimilar(target) {
  state.similar = target;
  state.terms = [];
  $("similar-chip").hidden = false;
  $("similar-name").textContent = target.name;
  setTip($("similar-chip"), target.name);
  await fetchResults(`/api/similar/${target.id}?${filtersQuery()}`);
}

async function fetchResults(url) {
  searchCtl?.abort();
  searchCtl = new AbortController();
  try {
    const res = await fetch(url, { signal: searchCtl.signal });
    if (!res.ok) throw new Error(apiMessage((await res.json().catch(() => ({}))).detail, res.statusText));
    const data = await res.json();
    state.semantic = data.semantic;
    updateNotice();
    renderResults(data.results);
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

function pigImage(tip, small = false) {
  const img = h("img", { src: "/static/pig.svg", alt: "", class: small ? "logo small" : "logo" });
  if (tip) setTip(img, tip);
  return img;
}

// No folders yet: one friendly sentence, its "add a folder" part a link to the folder picker.
function waitingText() {
  const [before, after = ""] = t("search.waiting").split("{folder}");
  // A link, not a button: buttons don't flow with text, so the comma after them could wrap.
  const link = h("a", { href: "#/folders", class: "link", onclick: (e) => { e.preventDefault(); pickFolder(); } },
    t("search.waiting_folder"));
  return h("p", { class: "waiting" }, before, link, after);
}

function renderEmpty() {
  const empty = $("empty");
  empty.hidden = false;
  if (state.hasFolders === false) return empty.replaceChildren(pigImage(), waitingText());
  empty.replaceChildren(pigImage());
}

function renderMessage(msg) {
  state.results = [];
  $("results").replaceChildren();
  $("empty").hidden = false;
  $("empty").replaceChildren(pigImage(null, true), h("p", {}, msg));
}

function renderResults(items) {
  const box = $("results");
  box.replaceChildren();
  hideTip();
  state.selected = -1;
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
  items.forEach((r, i) => box.append(make(r, i)));
}

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
  });
}

const openFile = (r) => post(`/api/open/${r.file_id}`).catch((e) => toast(e.message, "alert"));
const revealFile = (r) => post(`/api/reveal/${r.file_id}`).catch((e) => toast(e.message, "alert"));
const similarTo = (r) => { goTo("search"); window.scrollTo(0, 0); closeLightbox(); runSimilar({ id: r.file_id, name: r.name }); };

function actionButtons(r) {
  return h("span", { class: "actions" },
    iconButton("external", t("result.open"), () => openFile(r)),
    iconButton("folder", t("result.reveal"), () => revealFile(r)),
    iconButton("similar", t("result.similar"), () => similarTo(r)),
  );
}

function extBadge(r) {
  const ext = r.name.includes(".") ? r.name.split(".").pop().toLowerCase() : r.kind;
  const color = EXT_COLORS[ext] || (r.kind === "code" ? "#7048e8" : "#6b6d65");
  return h("div", { class: "ext", style: `--c: ${color}` }, ext.slice(0, 4));
}

// How well a result matches, as signal bars (meaning) and "Aa" (your words); words in the tooltip.
function matchBadge(r) {
  const parts = [];
  const lines = [];
  if (r.similarity != null) {
    const n = r.similarity >= 0.72 ? 3 : r.similarity >= 0.64 ? 2 : 1;
    lines.push(t(["match.possible", "match.good", "match.great"][n - 1]));
    parts.push(h("span", { class: `bars q${n}` }, h("i"), h("i"), h("i")));
  }
  if (r.exact || r.similarity == null) {
    lines.push(t(r.exact ? "match.all_words_title" : "match.some_words_title"));
    parts.push(h("span", { class: `aa${r.exact ? " exact" : ""}` }, "Aa"));
  }
  const el = h("span", { class: "match" }, ...parts);
  setTip(el, lines.join(" · "));
  return el;
}

function row(r, i) {
  let visual;
  if (r.preview) {
    visual = h("img", { class: "thumb", src: thumbUrl(r), loading: "lazy", alt: "", onclick: () => openLightbox(i) });
    visual.addEventListener("error", () => visual.replaceWith(extBadge(r)), { once: true });
  } else {
    visual = extBadge(r);
  }
  const dir = h("button", { type: "button", class: "dir mono", onclick: () => setScope(r.dir) }, r.dir);
  setTip(dir, t("scope.here"));
  const el = h("div", { class: "result", "data-i": i },
    visual,
    h("div", { class: "body" },
      h("div", { class: "title-row" },
        h("span", { class: "name", onclick: () => openFile(r) }, r.name),
        matchBadge(r),
        actionButtons(r)),
      dir,
      r.snippet ? h("p", { class: "snippet", onclick: () => el.classList.toggle("expanded") },
        highlighted(r.snippet, state.terms)) : null,
      h("div", { class: "info" }, `${ago(r.mtime)} · ${bytes(r.size)}`),
    ),
  );
  el.addEventListener("mouseenter", () => select(i, false));
  return el;
}

// List view: one line per file. The passage that matched shows when you point at the name.
function line(r, i) {
  let visual;
  if (r.preview) {
    visual = h("img", { class: "mini", src: thumbUrl(r), loading: "lazy", alt: "", onclick: () => openLightbox(i) });
    visual.addEventListener("error", () => visual.replaceWith(extBadge(r)), { once: true });
  } else {
    visual = extBadge(r);
  }
  const name = h("span", { class: "name", onclick: () => openFile(r) }, r.name);
  const passage = (r.snippet || "").replace(/\s+/g, " ").trim();
  setTip(name, passage ? (passage.length > 280 ? `${passage.slice(0, 280)}…` : passage) : r.path);
  const dir = h("button", { type: "button", class: "dir mono", onclick: () => setScope(r.dir) }, r.dir);
  setTip(dir, t("scope.here"));
  const el = h("div", { class: "line", "data-i": i },
    visual, name, dir,
    h("span", { class: "info" }, ago(r.mtime)), h("span", { class: "info size" }, bytes(r.size)),
    matchBadge(r), actionButtons(r));
  el.addEventListener("mouseenter", () => select(i, false));
  return el;
}

function tile(r, i) {
  const name = h("span", { class: "name", onclick: () => openFile(r) }, r.name);
  setTip(name, r.path);
  const picture = r.preview
    ? h("img", { src: thumbUrl(r), loading: "lazy", alt: r.name, onclick: () => openLightbox(i) })
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
  const els = $("results").children;
  if (state.selected >= 0 && els[state.selected]) els[state.selected].classList.remove("selected");
  state.selected = i;
  if (i >= 0 && els[i]) {
    els[i].classList.add("selected");
    if (scroll) els[i].scrollIntoView({ block: "nearest" });
  }
}

$("search-form").addEventListener("submit", (e) => {
  e.preventDefault();
  clearTimeout(searchTimer);
  clearSimilar(false);
  runSearch();
});

$("q").addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => { clearSimilar(false); runSearch(); }, 300);
});

for (const b of $("kinds").querySelectorAll("button")) {
  b.addEventListener("click", () => {
    $("kinds").querySelectorAll("button").forEach((x) => x.classList.toggle("active", x === b));
    state.kind = b.dataset.kind;
    updateViews();
    runSearch();
  });
}

function clearSimilar(rerun = true) {
  if (!state.similar) return;
  state.similar = null;
  $("similar-chip").hidden = true;
  if (rerun) runSearch();
}

$("similar-clear").addEventListener("click", () => clearSimilar());

// ---------- lightbox ----------

function previewable() {
  return state.results.map((r, i) => (r.preview ? i : -1)).filter((i) => i >= 0);
}

function openLightbox(i) {
  const r = state.results[i];
  if (!r || !r.preview) return;
  hideTip();
  state.lightbox = i;
  $("lb-img").src = previewUrl(r);
  $("lb-name").textContent = r.name;
  setTip($("lb-name"), r.path);
  $("lb-actions").replaceChildren(...actionButtons(r).childNodes);
  const list = previewable();
  $("lb-prev").style.visibility = list.indexOf(i) > 0 ? "visible" : "hidden";
  $("lb-next").style.visibility = list.indexOf(i) < list.length - 1 ? "visible" : "hidden";
  $("lightbox").hidden = false;
}

function stepLightbox(dir) {
  const list = previewable();
  const pos = list.indexOf(state.lightbox) + dir;
  if (pos >= 0 && pos < list.length) openLightbox(list[pos]);
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
const KIND_GROUP = { doc: "docs", text: "text", code: "code", image: "images" };
const KIND_COLOR = { docs: "#1d4aff", text: "#7048e8", code: "#f54e00", images: "#f2547f" };
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
  hover: null, rowHover: null, selected: null, history: [], related: null, closest: [], vector: null,
  info: null, tab: "preview",
  highlight: null, dim: null, dimValues: null, dimCache: new Map(),
  show: { docs: true, text: true, code: true, images: true },
  drag: null, animStart: 0, last: 0, raf: 0, timer: 0, spinTimer: 0, screen: [],
};
let mapSearchTimer = null;

const visible = (p) => !!p && map.show[p.g] && (!state.scope || p.lp.startsWith(state.scope.prefix));

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
  if (map.selected) {
    const again = map.byId.get(map.selected.id);
    if (again) selectPoint(again, { remember: false, fly: false });
    else clearSelection();
  }
  $("map-empty").hidden = points.length > 0;
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
  // It turns while you look at it: not while another window has the focus (a game, say).
  const turning = map.spin && document.hasFocus();
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

  // A faint grid on the cube's floor, then its edges: nearer ones a little stronger.
  const ink = (a) => `rgba(${theme.ink}, ${a.toFixed(3)})`;
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
    const near = Math.max(0, Math.min(1, ((c[a][2] + c[b][2]) / 2 + 1.6) / 3.2));
    ctx.strokeStyle = ink(0.08 + 0.1 * near);
    ctx.beginPath();
    ctx.moveTo(c[a][0], c[a][1]);
    ctx.lineTo(c[b][0], c[b][1]);
    ctx.stroke();
  }

  // The dots, far ones first. Each keeps its screen record between frames (no garbage).
  const list = [];
  const frame = (map.frame = (map.frame || 0) + 1);
  let fading = false;
  for (const p of map.points) {
    if (!visible(p)) continue;
    const [px, py, pz, s] = project(p.fx + (p.x - p.fx) * ease, p.fy + (p.y - p.fy) * ease, p.fz + (p.z - p.fz) * ease);
    if (pz > CAMERA_D - 0.5) continue; // right in front of the camera
    const q = p.q || (p.q = { p });
    q.px = px; q.py = py; q.pz = pz; q.s = s; q.r = 0; q.frame = frame;
    q.fade = p.born ? Math.min(1, (now - p.born) / 700) : 1;
    if (q.fade < 1) fading = true;
    list.push(q);
  }
  list.sort((a, b) => a.pz - b.pz);
  const at = { get: (id) => { const q = map.byId.get(id)?.q; return q && q.frame === frame ? q : undefined; } };

  const sel = map.selected && at.get(map.selected.id);
  const dims = map.dimValues;
  if (sel && map.closest.length && !dims) { // dashed lines to the closest files
    ctx.setLineDash([4 * dpr, 4 * dpr]);
    ctx.lineWidth = 1.3 * dpr;
    map.closest.forEach((cl, i) => {
      const q = at.get(cl.id);
      if (!q) return;
      ctx.strokeStyle = ink(0.5 - i * 0.06);
      ctx.beginPath();
      ctx.moveTo(sel.px, sel.py);
      ctx.lineTo(q.px, q.py);
      ctx.stroke();
    });
    ctx.setLineDash([]);
  }

  const base = Math.max(2.2, Math.min(5.5, 7.5 - 1.6 * Math.log10(Math.max(10, map.points.length))));
  const hl = map.highlight;
  const rel = map.related;
  for (const q of list) {
    const p = q.p;
    const depth = Math.min(1, Math.max(0, (q.s - 0.75) / 0.6));
    let color = KIND_COLOR[p.g];
    let alpha = 0.4 + 0.6 * depth;
    let size = 1;
    if (dims) {
      color = diverging((dims.values.get(p.id) ?? 0) / dims.scale);
      alpha = 0.95;
    } else if (rel && p !== map.selected) {
      const w = rel.get(p.id)?.w ?? 0; // 1 = the most related file, 0 = the least
      alpha = (0.05 + 0.95 * w ** 6) * (0.7 + 0.3 * depth);
      size = 0.75 + 0.6 * w ** 4;
    } else if (hl) {
      const lit = hl.has(p.id);
      alpha = lit ? 1 : 0.1;
      size = lit ? 1.4 : 1;
    }
    q.r = base * (0.5 + 0.5 * q.s) * dpr * Math.sqrt(zoom) * size;
    // One fill per dot: a canvas draws 6,000 single circles faster than a few paths holding
    // them all (measured: 12 ms against 19 ms a frame).
    ctx.globalAlpha = alpha * q.fade;
    ctx.beginPath();
    ctx.arc(q.px, q.py, q.r, 0, TAU);
    ctx.fillStyle = color;
    ctx.fill();
    if (hl && hl.has(p.id) && !dims) {
      ctx.lineWidth = 1.5 * dpr;
      ctx.strokeStyle = ink(0.8);
      ctx.stroke();
    }
  }
  ctx.globalAlpha = 1;
  for (const [p, color] of [[map.hover || map.rowHover, ink(0.8)], [map.selected, "#f54e00"]]) {
    const q = p && at.get(p.id);
    if (!q) continue;
    ctx.lineWidth = 2.5 * dpr;
    ctx.strokeStyle = color;
    ctx.beginPath();
    ctx.arc(q.px, q.py, q.r + 4 * dpr, 0, TAU);
    ctx.stroke();
  }
  map.screen = list;
  if (map.drag || map.fly || appear < 1 || fading) {
    startMapLoop();
  } else if (turning) {
    // The slow turn looks the same at 30 frames a second as at a 144 Hz screen's rate,
    // for a fraction of the work.
    clearTimeout(map.spinTimer);
    map.spinTimer = setTimeout(startMapLoop, 33);
  }
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
  if (remember && map.selected && !same) map.history.push(map.selected.id);
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
  map.closest = pairs.filter(([id]) => visible(map.byId.get(id))).slice(0, 6).map(([id, s]) => ({ id, s }));
  renderRelatedList();
}

function clearSelection() {
  map.selected = null;
  map.history = [];
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
}

// Back to the file you looked at before.
function stepBack() {
  const p = map.byId.get(map.history.pop());
  if (p) selectPoint(p, { remember: false });
}

function showCard(p) {
  $("map-card").hidden = false;
  $("mc-name").textContent = p.name;
  const dir = $("mc-dir");
  dir.textContent = p.dir;
  setTip(dir, t("scope.here"));
  dir.onclick = () => setScope(p.dir);
  $("mc-actions").replaceChildren(...actionButtons({ file_id: p.id, name: p.name }).childNodes);
  $("mc-back").disabled = !map.history.length;
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

// The card's preview: the picture for photos and PDFs, else the start of the text.
function showPreview(p, info) {
  const box = $("mc-preview");
  const key = info ? `${p.id}:${info.mtime}:${info.size}` : "";
  if (key && box.dataset.key === key) return; // already showing it
  box.dataset.key = key;
  if (!info) {
    box.className = "mc-preview";
    return box.replaceChildren();
  }
  const text = () => {
    box.className = "mc-preview";
    box.replaceChildren(info.passage
      ? h("p", { class: `mc-text${info.kind === "code" ? " mono" : ""}` }, info.passage)
      : h("span", { class: "mc-none" }, icon(info.kind in ICONS ? info.kind : "doc")));
  };
  if (!info.preview) return text();
  const img = h("img", { src: thumbUrl({ file_id: p.id, mtime: info.mtime, size: info.size }), alt: "",
    onclick: () => openFile({ file_id: p.id }) });
  // (a picture still loading for a file you've since left must not touch the card)
  img.addEventListener("load", () => { if (img.isConnected) box.classList.remove("loading"); }, { once: true });
  img.addEventListener("error", () => { if (img.isConnected) text(); }, { once: true });
  box.replaceChildren(img);
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
// similar they are: the number, and a bar for it (unrelated files score around 0.6, close ones 0.9).
function renderRelatedList() {
  const number = (s) => s.toLocaleString(i18n.lang, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  $("mc-related").replaceChildren(...map.closest.slice(0, 5).map((cl) => {
    const p = map.byId.get(cl.id);
    const strength = Math.max(0.06, Math.min(1, (cl.s - 0.55) / 0.4));
    const row = h("button", { type: "button", class: "mc-rel", onclick: () => selectPoint(p) },
      h("i", { class: "mc-dot", style: `background: ${KIND_COLOR[p.g]}` }),
      h("span", { class: "mc-rel-name" }, p.name),
      h("span", { class: "mc-rel-num" }, number(cl.s)),
      h("span", { class: "mc-rel-bar" }, h("b", { style: `width: ${Math.round(strength * 100)}%` })));
    row.addEventListener("pointerenter", () => { map.rowHover = p; startMapLoop(); });
    row.addEventListener("pointerleave", () => { map.rowHover = null; startMapLoop(); });
    setTip(row, p.name);
    row.dataset.tipCut = ".mc-rel-name";
    return row;
  }));
  $("mc-back").disabled = !map.history.length;
}

// ---------- explanations: how the map works, what the settings do ----------

function openHelp(title, intro, ...content) {
  hideTip();
  $("help-title").textContent = title;
  $("help-intro").textContent = intro || "";
  $("help-intro").hidden = !intro;
  $("help-body").replaceChildren(...content);
  $("help").hidden = false;
}

const closeHelp = () => { $("help").hidden = true; };
$("help-close").addEventListener("click", closeHelp);
$("help").addEventListener("click", (e) => { if (e.target.id === "help") closeHelp(); });

const MAP_HELP = [["cube", "dots"], ["similar", "close"], ["focus", "squeeze"], ["gauge", "numbers"],
  ["dots", "print"], ["external", "buttons"], ["mouse", "move"]];

$("mc-info").addEventListener("click", () => openHelp(t("map.how"), "", ...MAP_HELP.map(([name, key]) =>
  h("div", { class: "help-row" }, icon(name), h("p", {}, h("b", {}, t(`map.help.${key}_t`)), " ", t(`map.help.${key}`))))));

// A table of options side by side; the one in use is marked.
function compare(columns, current, rows, note) {
  const named = (ic, text) => h("span", { class: "cmp-name" }, icon(ic), h("span", {}, text));
  const head = h("tr", {}, h("th"), ...columns.map(([key, name, ic]) =>
    h("th", { class: key === current ? "current" : "" }, named(ic, name))));
  const body = rows.map(([ic, label, cells]) => h("tr", {}, h("th", {}, named(ic, label)),
    ...cells.map((cell, i) => h("td", { class: columns[i][0] === current ? "current" : "" }, cell))));
  return [h("table", { class: "compare" }, h("thead", {}, head), h("tbody", {}, ...body)),
    ...(note ? [h("p", { class: "help-note" }, note)] : [])];
}

// Performance: how hard indexing works in each situation (the governor's rules, governor.py), and
// what's kept ready for searching (store.py, and the indexer's _manage_device).
function performanceHelp() {
  const pace = (duty) => h("span", { class: "pace" },
    h("span", { class: "pace-bar" }, h("i", { style: `width: ${Math.round(duty * 100)}%` })),
    duty ? pct(Math.round(duty * 100)) : t("help.paused"));
  const range = (text) => h("span", { class: "pace" }, h("span", { class: "pace-bar" }, h("i", { style: "width: 25%" })), text);
  const s = state.last;
  const passages = (s?.folders || []).reduce((a, f) => a + f.chunks, 0);
  const mb = Math.max(1, Math.round(s?.memory?.vectors_mb || passages * 0.001024));
  const ram = t("help.mem_data_fast", { mb: fmt(mb), n: fmt(passages) });
  const rows = [
    ["mouse", t("help.sit_working"), [0.25, 0.75, 1].map(pace)],
    ["away", t("help.sit_away"), [0.5, 1, 1].map(pace)],
    ["battery", t("help.sit_battery"), [0, 0.25, 1].map(pace)],
    ["gauge", t("help.sit_busy"), [range(t("help.slower_early")), range(t("help.slower_late")), pace(1)]],
    ["search", t("help.mem_search"), [t("help.mem_search_lean"), t("help.mem_search_fast"), t("help.mem_search_fast")]],
    ["ram", t("help.mem_data"), [t("help.mem_data_lean"), ram, ram]],
    ["chip", t("help.mem_model"), [t("help.mem_model_lean"), t("help.mem_model_given"), t("help.mem_model_kept")]],
  ];
  const modes = [["gentle", t("mode.gentle"), "speed1"], ["balanced", t("mode.balanced"), "speed2"], ["full", t("mode.full"), "speed3"]];
  const note = `${t("help.speed_note")} ${t("help.mem_note", { mb: fmt(s?.memory?.app_mb || 0) })}`;
  openHelp(t("settings.speed"), t("help.speed_intro"), ...compare(modes, s?.settings.perf_mode, rows, note));
}

$("speed-info").addEventListener("click", (e) => { e.preventDefault(); performanceHelp(); });
for (const b of $("mc-tabs").querySelectorAll("button")) b.addEventListener("click", () => setCardTab(b.dataset.tab));
setCardTab("preview");

// The file's 256 numbers as a 16 × 16 grid: pink above zero, blue below.
function drawFingerprint(v, active = null) {
  const canvas = $("mc-print");
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
    const a = Math.min(1, Math.abs(x) / top) ** 0.7;
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
  Object.assign(map, { yaw: 0.7, pitch: 0.35 });
  flyTo(map.selected ? { zoom: FOCUS_ZOOM } : OVERVIEW);
}

async function mapSearch() {
  const q = $("map-q").value.trim();
  if (!q) {
    map.highlight = null;
  } else {
    try {
      const qs = new URLSearchParams({ q, limit: 40 });
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
      hideTip();
      startMapLoop();
      return;
    }
    const p = mapHit(e);
    if (p !== map.hover) { map.hover = p; startMapLoop(); }
    canvas.style.cursor = p ? "pointer" : "grab";
    if (p) {
      showTipAt(e.clientX, e.clientY, `${p.name}\n${p.dir.split(/[\\/]/).slice(-2).join("\\")}`);
    } else if (tipFor === "point") {
      hideTip();
    }
  });
  canvas.addEventListener("pointerup", (e) => {
    const click = map.drag && map.drag.moved < 5;
    map.drag = null;
    if (!click) return;
    const p = mapHit(e);
    if (p) selectPoint(p);
    else if (map.selected) clearSelection();
  });
  canvas.addEventListener("pointerleave", () => { map.hover = null; if (tipFor === "point") hideTip(); });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    map.fly = null;
    map.cam.zoom = Math.max(0.5, Math.min(6, map.cam.zoom * Math.exp(-e.deltaY * 0.0012)));
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
$("map-spin").addEventListener("click", () => { map.spin = !map.spin; map.spinBefore = null; updateSpin(); startMapLoop(); });
$("map-reset").addEventListener("click", resetMapView);
$("map-search").addEventListener("submit", (e) => { e.preventDefault(); clearTimeout(mapSearchTimer); mapSearch(); });
$("map-q").addEventListener("input", () => { clearTimeout(mapSearchTimer); mapSearchTimer = setTimeout(mapSearch, 300); });
for (const b of $("map-kinds").querySelectorAll("button")) {
  b.addEventListener("click", () => {
    map.show[b.dataset.kind] = !map.show[b.dataset.kind];
    b.classList.toggle("active", map.show[b.dataset.kind]);
    if (map.selected && !visible(map.selected)) clearSelection();
    startMapLoop();
  });
}
document.addEventListener("visibilitychange", () => { if (!document.hidden && state.page === "map") startMapLoop(); });
window.addEventListener("focus", () => { if (state.page === "map") startMapLoop(); });

// ---------- folder scope ----------

// Search and the map can be narrowed to one folder: any folder inside the indexed ones.
function setScope(path) {
  state.scope = path ? { path, name: baseName(path), prefix: `${path.toLowerCase().replace(/[\\/]+$/, "")}\\` } : null;
  renderScopes();
  if (state.similar) runSimilar(state.similar);
  else if ($("q").value.trim()) runSearch();
  if (map.selected && !visible(map.selected)) clearSelection();
  if ($("map-q").value.trim()) mapSearch();
  startMapLoop();
}

function renderScopes() {
  const s = state.scope;
  for (const box of [$("scope-search"), $("scope-map")]) {
    const pill = h("button", { type: "button", class: `scope-pill${s ? " on" : ""}`, onclick: () => openScopeMenu(box) },
      icon("folder"), h("span", { class: "scope-name" }, s ? s.name : t("search.all_folders")), icon("chevronDown"));
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

// A small folder browser: click a folder to search in it and see the folders inside it.
async function openScopeMenu(box, path = state.scope?.path || "") {
  let data;
  try {
    data = await api(`/api/dirs?${new URLSearchParams({ path })}`);
  } catch (e) {
    toast(e.message, "alert");
    return;
  }
  const menu = $("scope-menu");
  const roots = (state.last?.folders || []).map((f) => f.path.toLowerCase());
  const parent = !path ? null : roots.includes(path.toLowerCase()) ? "" : path.replace(/[\\/][^\\/]+[\\/]?$/, "");
  const row = (name, label, count, onclick, on = false) => h("button", { type: "button", class: `menu-row${on ? " on" : ""}`, onclick },
    icon(name), h("span", { class: "menu-name" }, label), count != null ? h("span", { class: "menu-count" }, fmt(count)) : null);
  const rows = [];
  if (state.scope) rows.push(row("globe", t("scope.clear"), null, () => { setScope(null); closeScopeMenu(); }));
  if (parent !== null) rows.push(row("up", parent ? baseName(parent) : t("search.all_folders"), null, () => openScopeMenu(box, parent)));
  for (const d of data.dirs) {
    rows.push(row("folder", d.name, d.count, () => { setScope(d.path); openScopeMenu(box, d.path); },
      state.scope?.path.toLowerCase() === d.path.toLowerCase()));
  }
  if (!data.dirs.length) rows.push(h("div", { class: "menu-empty" }, t("scope.no_subfolders")));
  menu.replaceChildren(...rows);
  menu.hidden = false;
  const r = box.getBoundingClientRect();
  menu.style.transform = `translate(${Math.round(Math.min(r.left, innerWidth - menu.offsetWidth - 8))}px, ${Math.round(r.bottom + 6)}px)`;
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
  if (!$("help").hidden) {
    if (e.key === "Escape") { e.preventDefault(); closeHelp(); }
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
  if (!$("update-modal").hidden) {
    if (e.key === "Escape") { e.preventDefault(); closeUpdateModal(); }
    return;
  }
  if (!$("lightbox").hidden) {
    if (e.key === "Escape") closeLightbox();
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
  if (state.page === "map" && map.selected) { // ← (or Backspace): back to the file before
    if (e.key === "Escape") clearSelection();
    else if (e.key === "ArrowLeft" || e.key === "Backspace") stepBack();
    else return;
    e.preventDefault();
    return;
  }
  if (state.page !== "search") return;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    if (!state.results.length) return;
    e.preventDefault();
    const n = state.results.length;
    const step = e.key === "ArrowDown" ? 1 : -1;
    select(state.selected < 0 ? (step > 0 ? 0 : n - 1) : (state.selected + step + n) % n);
  } else if (e.key === "Enter" && state.selected >= 0) {
    e.preventDefault();
    const r = state.results[state.selected];
    if (e.ctrlKey) revealFile(r);
    else if (e.shiftKey && r.preview) openLightbox(state.selected);
    else openFile(r);
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
