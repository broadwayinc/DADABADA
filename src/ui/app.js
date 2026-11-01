"use strict";
// dadabada admin UI. Plain DOM, no framework. Every value from the server is set with textContent.
// Structure and classes follow the skapi.com dashboard (navBar, sideNav, the list-page kit, bq-meta cards).

const S = { token: null, info: null, users: [], userById: {}, timer: null, menuOpen: false };
const THEME_KEY = "bq-theme";
const TAGLINE = "Database Administrator";

// ---------- small helpers ----------

function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") el.className = v;
      else if (k === "text") el.textContent = v;
      else if (k === "value") el.value = v;
      else if (k === "checked") el.checked = !!v;
      else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2).toLowerCase(), v);
      else el.setAttribute(k, v === true ? "" : String(v));
    }
  }
  add(el, kids);
  return el;
}
function add(el, kids) {
  for (const k of kids.flat(Infinity)) {
    if (k === null || k === undefined || k === false) continue;
    el.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return el;
}
function clear(el) {
  while (el.firstChild) el.removeChild(el.firstChild);
  return el;
}

const SVG = "http://www.w3.org/2000/svg";
const WAVES = ["M16 20 C21 13 27 13 32 20 S43 27 48 20", "M16 32 C21 25 27 25 32 32 S43 39 48 32", "M16 44 C21 37 27 37 32 44 S43 51 48 44"];
function mark(size, tile) {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", "0 0 64 64");
  svg.setAttribute("width", size);
  svg.setAttribute("height", size);
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("class", tile ? "mark tile" : "mark");
  if (tile) {
    const r = document.createElementNS(SVG, "rect");
    r.setAttribute("width", "64");
    r.setAttribute("height", "64");
    r.setAttribute("rx", "14");
    svg.append(r);
  }
  const sw = size <= 16 ? 7 : size <= 24 ? 6 : 5;
  for (const d of WAVES) {
    const p = document.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    p.setAttribute("stroke-width", sw);
    svg.append(p);
  }
  return svg;
}
// Line icons (24px grid), the Material Symbols the skapi nav uses, drawn as strokes.
const ICONS = {
  settings: "M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z",
  database: "M12 3c4.4 0 8 1.3 8 3s-3.6 3-8 3-8-1.3-8-3 3.6-3 8-3Z M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6 M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6",
  search: "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16Z M21 21l-4.3-4.3",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z",
  group: "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2 M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z M22 21v-2a4 4 0 0 0-3-3.9 M16 3.1a4 4 0 0 1 0 7.8",
  key: "M21 2l-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.8 7.8 5.5 5.5 0 0 1 7.8-7.8Zm0 0L15.5 7.5m0 0 3 3L22 7l-3-3m-3.5 3.5L19 4",
  tag: "M20.6 13.4 13.4 20.6a2 2 0 0 1-2.8 0L2 12V2h10l8.6 8.6a2 2 0 0 1 0 2.8Z M7 7h.01",
  tune: "M4 21v-7 M4 10V3 M12 21v-9 M12 8V3 M20 21v-5 M20 12V3 M1 14h6 M9 8h6 M17 16h6",
  build: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.8-3.8a6 6 0 0 1-7.9 7.9l-7 7a2.1 2.1 0 0 1-3-3l7-7a6 6 0 0 1 7.9-7.9l-3.8 3.8Z",
  refresh: "M23 4v6h-6 M1 20v-6h6 M3.5 9a9 9 0 0 1 14.9-3.4L23 10 M1 14l4.6 4.4A9 9 0 0 0 20.5 15",
  chevron: "M9 18l6-6-6-6",
  close: "M18 6L6 18 M6 6l12 12",
  arrowLeft: "M19 12H5 M12 19l-7-7 7-7",
  arrowRight: "M5 12h14 M12 5l7 7-7 7",
  sun: "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10Z M12 1v2 M12 21v2 M4.2 4.2l1.4 1.4 M18.4 18.4l1.4 1.4 M1 12h2 M21 12h2 M4.2 19.8l1.4-1.4 M18.4 5.6l1.4-1.4",
  moon: "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z",
  warn: "M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0Z M12 9v4 M12 17h.01",
  trash: "M3 6h18 M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2 M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6",
};
function icon(name, cls) {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  if (cls) svg.setAttribute("class", cls);
  const p = document.createElementNS(SVG, "path");
  p.setAttribute("d", ICONS[name]);
  svg.append(p);
  return svg;
}

function store(k, v) {
  try {
    if (v === undefined) return sessionStorage.getItem(k);
    if (v === null) sessionStorage.removeItem(k);
    else sessionStorage.setItem(k, v);
  } catch (_) {
    return null;
  }
  return null;
}
function fmtTime(ms) {
  if (!ms) return "";
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
function fmtNum(n) {
  return typeof n === "number" ? n.toLocaleString("en-US") : String(n ?? "");
}
function fmtBytes(n) {
  if (typeof n !== "number") return "";
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MB`;
  return `${(n / 1073741824).toFixed(2)} GB`;
}
function valText(v) {
  return typeof v === "string" ? JSON.stringify(v) : String(v);
}
function pretty(v) {
  return JSON.stringify(v, null, 2);
}
function userName(id) {
  const u = S.userById[id];
  return u ? u.username : id || "";
}
function levels(withAny, withPrivate) {
  const out = [];
  if (withAny) out.push(["", withAny]);
  for (let i = 0; i <= 99; i++) out.push([String(i).padStart(2, "0"), String(i).padStart(2, "0")]);
  if (withPrivate) out.push(["**", "** private"]);
  return out;
}
function select(options, value, props) {
  const s = h("select", props);
  for (const [v, label] of options) s.append(h("option", { value: v, text: label }));
  s.value = value ?? "";
  return s;
}
// an advanced-search row: 7rem label + control(s)
function row(label, ...controls) {
  return h("div", { class: "bq-adv-row" }, h("label", { class: "bq-adv-label", text: label }), controls);
}
function section(title, ...kids) {
  return h("div", { class: "bq-adv-section" }, title ? h("div", { class: "bq-adv-section-title", text: title }) : null, kids);
}
function divider() {
  return h("div", { class: "bq-adv-divider" });
}
function advInput(props) {
  return h("input", Object.assign({ class: "bq-adv-input", type: "text", spellcheck: "false" }, props || {}));
}
function advSelect(options, value, props) {
  return select(options, value, Object.assign({ class: "bq-adv-select" }, props || {}));
}
function check(label, checked, props) {
  const input = h("input", Object.assign({ type: "checkbox", checked }, props || {}));
  return { el: h("label", { class: "bq-checkbox" }, input, h("span", { text: label })), input };
}
function passwordInput(props, adv) {
  const input = h("input", Object.assign({ type: "password", autocomplete: "off", spellcheck: "false", class: adv ? "bq-adv-input" : null }, props || {}));
  const btn = h("button", {
    type: "button",
    text: "show",
    "aria-label": "Show or hide the password",
    onclick: () => {
      const shown = input.type === "text";
      input.type = shown ? "password" : "text";
      btn.textContent = shown ? "show" : "hide";
    },
  });
  return { el: h("div", { class: adv ? "pw pw--adv" : "pw" }, input, btn), input };
}
function toast(msg, isError) {
  const t = h("div", { class: "bq-toast" + (isError ? " error" : ""), role: isError ? "alert" : "status", text: msg });
  document.getElementById("toasts").append(t);
  setTimeout(() => t.remove(), isError ? 7000 : 3500);
}
async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("Copied.");
  } catch (_) {
    toast("Could not copy (the clipboard needs HTTPS); select the text instead.", true);
  }
}

// ---------- theme (navBar.vue) ----------

function isDark() {
  return document.documentElement.dataset.theme === "dark";
}
function applyTheme(dark) {
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  const paper = getComputedStyle(document.documentElement).getPropertyValue("--bq-paper").trim();
  const tc = document.querySelector('meta[name="theme-color"]');
  if (tc && paper) tc.setAttribute("content", paper);
  for (const t of document.querySelectorAll(".bq-theme-toggle")) {
    t.classList.toggle("is-dark", dark);
    t.setAttribute("aria-checked", String(dark));
    t.title = dark ? "Switch to light mode" : "Switch to dark mode";
  }
}
function initTheme() {
  let stored = null;
  try {
    stored = localStorage.getItem(THEME_KEY);
  } catch (_) {}
  // Light is the default; dark only when chosen with the toggle.
  applyTheme(stored === "dark");
}
function themeToggle() {
  const dark = isDark();
  return h(
    "button",
    {
      class: "bq-theme-toggle" + (dark ? " is-dark" : ""),
      type: "button",
      role: "switch",
      "aria-checked": String(dark),
      "aria-label": "Toggle dark mode",
      title: dark ? "Switch to light mode" : "Switch to dark mode",
      onclick: () => {
        const d = !isDark();
        applyTheme(d);
        try {
          localStorage.setItem(THEME_KEY, d ? "dark" : "light");
        } catch (_) {}
      },
    },
    h("span", { class: "bq-theme-knob", "aria-hidden": "true" }),
    h("span", { class: "bq-theme-icon bq-theme-sun" }, icon("sun")),
    h("span", { class: "bq-theme-icon bq-theme-moon" }, icon("moon")),
  );
}

// ---------- server calls ----------

class ApiErr extends Error {
  constructor(code, message, status) {
    super(message);
    this.code = code;
    this.status = status;
  }
}
async function post(method, body, auth) {
  const headers = { "Content-Type": "application/json" };
  if (auth && S.token) headers.Authorization = "Bearer " + S.token;
  let res;
  try {
    res = await fetch("/v1/" + method, { method: "POST", headers, body: JSON.stringify(body || {}) });
  } catch (_) {
    throw new ApiErr("NETWORK", "Could not reach the server. Is DADABADA still running?", 0);
  }
  let data = null;
  try {
    data = await res.json();
  } catch (_) {
    throw new ApiErr("BAD_RESPONSE", "The server sent something that is not JSON.", res.status);
  }
  if (data && data.error) {
    const e = new ApiErr(data.error.code, data.error.message, res.status);
    if (auth && res.status === 401) signedOut("Your admin session ended. Sign in again.");
    throw e;
  }
  return data;
}
function admin(cmd, args) {
  return post("admin", Object.assign({ cmd }, args || {}), true);
}
async function run(btn, fn) {
  if (btn) btn.disabled = true;
  try {
    return await fn();
  } catch (e) {
    if (e.status !== 401) toast(e.message || String(e), true);
    return undefined;
  } finally {
    if (btn) btn.disabled = false;
  }
}
async function loadUsers() {
  const list = await admin("user_list");
  S.users = list;
  S.userById = {};
  for (const u of list) S.userById[u.user_id] = u;
  return list;
}

// ---------- modal (modal.vue + bq-modal-delete) ----------

function closeLayer() {
  clear(document.getElementById("layer"));
  document.removeEventListener("keydown", escClose);
}
function escClose(e) {
  if (e.key === "Escape") closeLayer();
}
// A confirmation; with `typed`, the user must type that word to confirm.
function confirmBox({ title, text, warn, confirm = "Confirm", danger = false, typed = null, iconName }) {
  return new Promise((resolve) => {
    const layer = document.getElementById("layer");
    const scrim = h("div", { class: "bq-scrim" });
    const box = h("div", { class: "bq-dialog", role: "alertdialog", "aria-modal": "true", "aria-label": title });
    const done = (v) => {
      box.remove();
      scrim.remove();
      document.removeEventListener("keydown", onKey);
      resolve(v);
    };
    const onKey = (e) => e.key === "Escape" && done(false);
    const input = typed ? h("input", { type: "text", autocomplete: "off", class: "bq-modal-input", placeholder: typed, "aria-label": `Type ${typed} to confirm` }) : null;
    const ok = h("button", { class: "bq-modal-delete-btn " + (danger ? "bq-modal-delete-btn--delete" : "bq-modal-delete-btn--primary"), type: "button", text: confirm, disabled: !!typed, onclick: () => done(true) });
    if (input) {
      input.addEventListener("input", () => (ok.disabled = input.value !== typed));
      input.addEventListener("keydown", (e) => e.key === "Enter" && input.value === typed && done(true));
    }
    add(box, [
      h("div", { class: "bq-modal-delete-header" + (danger ? "" : " bq-modal-delete-header--ink") }, icon(iconName || (danger ? "trash" : "warn"), "bq-modal-delete-icon"), h("span", { text: title })),
      h("div", { class: "bq-modal-delete-body" }, h("p", { text: text }), warn ? h("p", { class: "bq-modal-delete-warn", text: warn }) : null, input ? [h("p", { class: "bq-modal-delete-warn", text: `Type ${typed} to confirm.` }), input] : null),
      h("div", { class: "bq-modal-delete-footer" }, h("button", { class: "bq-modal-delete-btn bq-modal-delete-btn--cancel", type: "button", text: "Cancel", onclick: () => done(false) }), ok),
    ]);
    scrim.addEventListener("click", () => done(false));
    layer.append(scrim, box);
    document.addEventListener("keydown", onKey);
    (input || ok).focus();
  });
}

// ---------- server stopped or restarting ----------

// A page of its own: nothing else on the admin page works while the server is down.
function serverDown(restarting, dataFolder) {
  if (S.timer) {
    clearInterval(S.timer);
    S.timer = null;
  }
  closeLayer();
  const root = clear(document.getElementById("root"));
  const wait = h("p", { class: "bq-login-foot", text: restarting ? "Waiting for the server..." : "" });
  root.append(
    h(
      "div",
      { class: "bq-login" },
      h("div", { class: "bq-login-brand" }, mark(56, true), h("span", { class: "wordmark", text: "DADABADA" }), h("span", { class: "tagline", text: TAGLINE })),
      h("h1", { class: "bq-title", text: restarting ? "Restarting" : "Stopped" }),
      restarting
        ? h("p", { class: "small muted", text: "The server is stopping cleanly and starting again in the same process. This page asks you to sign in again when it is back." })
        : h("div", null, h("p", { class: "small muted", text: "The server stopped cleanly. Start it again on the server machine, as the operating system user that runs it:" }), h("pre", { class: "bq-code mt-sm", text: `$ dadabada start --data ${dataFolder || "<data folder>"}` })),
      wait,
    ),
  );
  if (!restarting) return;
  // the old process answers until it stops; the new one does not know this admin session, so it answers 401
  const began = Date.now();
  const tick = async () => {
    try {
      const res = await fetch("/v1/admin", { method: "POST", headers: { "Content-Type": "application/json", Authorization: "Bearer " + S.token }, body: JSON.stringify({ cmd: "ping" }) });
      if (res.status === 401) return signedOut("DADABADA restarted. Sign in again.");
    } catch (_) {
      /* down while it restarts */
    }
    wait.textContent = `Waiting for the server... ${Math.round((Date.now() - began) / 1000)} s`;
    setTimeout(tick, 1000);
  };
  setTimeout(tick, 1000);
}

// ---------- login (login.vue) ----------

function signedOut(message) {
  S.token = null;
  store("dd_admin", null);
  if (S.timer) clearInterval(S.timer);
  closeLayer();
  showLogin(message);
}
async function showLogin(message) {
  const root = clear(document.getElementById("root"));
  let info = S.info;
  try {
    info = S.info = await post("adminInfo", {}, false);
  } catch (e) {
    message = e.message;
  }
  const page = h("div", { class: "bq-login" });
  page.append(
    h("div", { class: "bq-login-brand" }, mark(56, true), h("span", { class: "wordmark", text: "DADABADA" }), h("span", { class: "tagline", text: TAGLINE })),
    h("h1", { class: "bq-title", text: "Admin login" }),
  );
  if (info && !info.web_admin) {
    page.append(
      h("div", { class: "bq-adv-search" }, h("div", { class: "bq-adv-section-title", text: "Web admin is off" }), h("p", { class: "small muted", text: "On the server, the operating system user that runs DADABADA turns it on with:" }), h("pre", { class: "bq-code mt-sm", text: "$ dadabada admin --data <data folder> web-password" }), h("p", { class: "small muted mt-sm", text: "Then reload this page." })),
      h("p", { class: "bq-login-foot", text: `DADABADA v${info.version}` }),
    );
    root.append(page);
    return;
  }
  const pw = passwordInput({ id: "pw", autocomplete: "current-password" });
  const err = h("div", { class: "bq-error", hidden: !message, role: "alert", text: message || "" });
  const btn = h("button", { class: "btn", type: "submit", text: "Login" });
  const form = h(
    "form",
    {
      class: "bq-form",
      onsubmit: async (e) => {
        e.preventDefault();
        btn.disabled = true;
        err.hidden = true;
        try {
          const r = await post("adminLogin", { password: pw.input.value }, false);
          S.token = r.token;
          store("dd_admin", r.token);
          await boot();
        } catch (e2) {
          err.textContent = e2.message;
          err.hidden = false;
          pw.input.select();
        } finally {
          btn.disabled = false;
        }
      },
    },
    h("label", { class: "bq-label" }, h("span", { text: "Password" }), pw.el),
    err,
    h("div", { class: "bq-bottom" }, btn),
  );
  page.append(form, h("p", { class: "bq-login-foot", text: `Only the admin signs in here. Users and agents use the API. DADABADA v${info ? info.version : ""}` }));
  root.append(page);
  pw.input.focus();
}

// ---------- shell (navBar + service layout + sideNav) ----------

const PAGES = [
  ["overview", "Overview", "settings"],
  ["records", "Records", "database"],
  ["query", "Query", "search"],
  ["collections", "Collections", "folder"],
  ["uniques", "Unique IDs", "tag"],
  ["users", "Users", "group"],
  ["keys", "Agent keys", "key"],
  ["settings", "Settings", "tune"],
  ["tools", "Tools", "build"],
];
function currentPage() {
  const p = (location.hash || "").replace(/^#\/?/, "").split("?")[0];
  return PAGES.some(([k]) => k === p) ? p : "overview";
}
function hashParams() {
  const q = location.hash.split("?")[1] || "";
  const out = {};
  for (const part of q.split("&")) {
    if (!part) continue;
    const [k, v] = part.split("=");
    out[decodeURIComponent(k)] = decodeURIComponent(v || "");
  }
  return out;
}
function go(page, params) {
  const q = params ? "?" + Object.entries(params).filter(([, v]) => v !== "" && v != null).map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&") : "";
  const target = "#/" + page + (q.length > 1 ? q : "");
  if (location.hash === target) render();
  else location.hash = target;
}
async function boot() {
  try {
    await loadUsers();
  } catch (e) {
    if (e.status === 401) return;
    showLogin(e.message);
    return;
  }
  render();
}
function navList(onPick) {
  const page = currentPage();
  const nav = h("nav", { class: "bq-nav", "aria-label": "Sections" });
  for (const [key, label, ic] of PAGES) {
    nav.append(h("button", { class: "bq-nav-item", type: "button", "aria-current": key === page ? "page" : null, onclick: () => { onPick && onPick(); go(key); } }, h("span", { text: label }), icon(ic, "bq-nav-icon")));
  }
  return nav;
}
async function logout() {
  try {
    await post("adminLogout", {}, true);
  } catch (_) {
    /* signing out anyway */
  }
  signedOut("Signed out.");
}
function render() {
  if (!S.token) return;
  if (S.timer) {
    clearInterval(S.timer);
    S.timer = null;
  }
  closeLayer();
  const page = currentPage();
  const root = clear(document.getElementById("root"));

  // top bar: theme toggle + logout (no profile menu: the admin is the only account here)
  const logoutLink = h("button", { class: "bq-bar-logout", type: "button", text: "[Logout]", onclick: logout });
  // phones: the menu is a full-screen overlay sliding in from the right, like skapi.com's (bq-mo-overlay)
  const mobileNav = h("div", { class: "bq-mo-overlay" + (S.menuOpen ? " open" : ""), role: "dialog", "aria-label": "Menu", "aria-modal": "true" });
  const setMenu = (open) => { S.menuOpen = open; mobileNav.classList.toggle("open", open); burger.setAttribute("aria-expanded", String(open)); document.body.classList.toggle("bq-menu-open", open); };
  const burger = h("button", { class: "bq-bar-burger bq-mobile-only", type: "button", "aria-label": "Menu", "aria-expanded": String(S.menuOpen), text: "☰", onclick: () => setMenu(!S.menuOpen) });
  mobileNav.append(
    h("div", { class: "bq-mo-header" }, h("a", { class: "bq-bar-logo", href: "#/overview", onclick: () => setMenu(false) }, mark(34, false), h("span", { text: "DADABADA" })), h("button", { class: "bq-mo-close", type: "button", "aria-label": "Close menu", onclick: () => setMenu(false) }, icon("close"))),
    h("div", { class: "bq-mo-body" }, navList(() => setMenu(false))),
    h("div", { class: "bq-mo-footer" }, h("button", { class: "bq-cta-link", type: "button", text: "[Logout]", onclick: logout })),
  );
  if (S.menuOpen) document.body.classList.add("bq-menu-open");
  const topbar = h(
    "header",
    { class: "bq-topbar" },
    h(
      "div",
      { class: "bq-bar" },
      h("a", { class: "bq-bar-logo", href: "#/overview" }, mark(34, false), h("span", { text: "DADABADA" })),
      h("div", { class: "bq-bar-right" }, themeToggle(), h("div", { class: "bq-desktop-nav" }, logoutLink), burger),
    ),
    mobileNav,
  );

  const main = h("main", { class: "bq-main" });
  const layout = h("div", { class: "bq-service-layout" }, h("aside", { class: "bq-sidebar" }, h("div", { class: "bq-sidenav" }, navList())), main);
  root.append(topbar, layout);
  const pages = { overview, records, query, collections, uniques, users, keys, settings, tools };
  pages[page](main, hashParams());
}

function pageHead(title, desc) {
  return [h("h1", { class: "bq-page-title", text: title }), desc ? h("p", { class: "bq-page-desc", text: desc }) : null];
}
function card(title, ...kids) {
  return h("div", { class: "bq-meta" }, title ? h("h2", { class: "bq-section-title" }, title) : null, kids);
}
function settingRow(label, hint, action) {
  return h("div", { class: "bq-row" }, h("div", { class: "bq-toggle-label" }, h("span", { text: label }), hint ? h("small", { class: "bq-toggle-hint", text: hint }) : null), action || null);
}
function metaRow(label, value) {
  return h("div", { class: "bq-meta-row" }, h("span", { text: label }), typeof value === "string" ? h("span", { class: "bq-mono", text: value }) : h("span", null, value));
}
function emptyRow(colspan, text) {
  return h("tr", null, h("td", { class: "bq-empty-cell", colspan, text }));
}
function table(heads, body) {
  return h("div", { class: "tableWrap bq-desktop-only" }, h("table", { class: "customTbl" }, h("thead", null, h("tr", null, heads.map((t) => (typeof t === "string" ? h("th", { text: t }) : h("th", { class: t.cls, text: t.text }))))), body));
}
const PAGE_SIZE = 100;

// JSON shown pretty with tokens coloured (text nodes only; nothing from the data becomes markup)
function jsonView(v) {
  const pre = h("pre", { class: "bq-code" });
  const text = typeof v === "string" ? v : pretty(v);
  const re = /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false)\b|\bnull\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|[{}[\],]/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) pre.append(text.slice(last, m.index));
    const tok = m[0];
    let cls = "j-punc";
    if (m[1]) cls = m[2] ? "j-key" : "j-str";
    else if (m[3]) cls = "j-bool";
    else if (tok === "null") cls = "j-null";
    else if (/^-?\d/.test(tok)) cls = "j-num";
    if (m[1] && m[2]) {
      pre.append(h("span", { class: "j-key", text: m[1] }), h("span", { class: "j-punc", text: m[2] }));
    } else pre.append(h("span", { class: cls, text: tok }));
    last = re.lastIndex;
  }
  if (last < text.length) pre.append(text.slice(last));
  return pre;
}
// A JSON editor: Tab indents, Enter keeps the indentation, validation on input.
function jsonArea(value, rows, placeholder) {
  const ta = h("textarea", { class: "bq-adv-textarea code", rows: rows || 8, spellcheck: "false", placeholder: placeholder || "" });
  ta.value = value;
  const err = h("p", { class: "bq-adv-mode-hint bq-adv-mode-hint--danger", hidden: true });
  const validate = () => {
    try {
      if (ta.value.trim() !== "") JSON.parse(ta.value);
      ta.classList.remove("invalid");
      err.hidden = true;
      return true;
    } catch (e) {
      ta.classList.add("invalid");
      err.textContent = "Not valid JSON: " + e.message;
      err.hidden = false;
      return false;
    }
  };
  ta.addEventListener("input", validate);
  ta.addEventListener("keydown", (e) => {
    const s = ta.selectionStart;
    const en = ta.selectionEnd;
    if (e.key === "Tab") {
      e.preventDefault();
      if (s !== en && ta.value.slice(s, en).includes("\n")) {
        const from = ta.value.lastIndexOf("\n", s - 1) + 1;
        const block = ta.value.slice(from, en);
        const lines = block.split("\n");
        const out = e.shiftKey ? lines.map((l) => l.replace(/^ {1,2}/, "")) : lines.map((l) => "  " + l);
        ta.setRangeText(out.join("\n"), from, en, "select");
      } else if (e.shiftKey) {
        const from = ta.value.lastIndexOf("\n", s - 1) + 1;
        const n = ta.value.slice(from, from + 2) === "  " ? 2 : ta.value[from] === " " ? 1 : 0;
        if (n) ta.setRangeText("", from, from + n, "end");
      } else ta.setRangeText("  ", s, en, "end");
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    } else if (e.key === "Backspace" && s === en && s > 0) {
      // only spaces between the line start and the cursor: remove one indent step (2 spaces), like an editor
      const from = ta.value.lastIndexOf("\n", s - 1) + 1;
      const lead = ta.value.slice(from, s);
      if (lead.length >= 2 && /^ +$/.test(lead)) {
        e.preventDefault();
        const n = lead.length % 2 === 0 ? 2 : 1;
        ta.setRangeText("", s - n, s, "end");
        ta.dispatchEvent(new Event("input", { bubbles: true }));
      }
    } else if ((e.key === "}" || e.key === "]") && s === en) {
      // closing a block on a line that only holds the indent: step the indent back first
      const from = ta.value.lastIndexOf("\n", s - 1) + 1;
      const lead = ta.value.slice(from, s);
      if (lead.length >= 2 && /^ +$/.test(lead)) {
        e.preventDefault();
        ta.setRangeText(e.key, s - 2, s, "end");
        ta.dispatchEvent(new Event("input", { bubbles: true }));
      }
    } else if (e.key === "Enter") {
      e.preventDefault();
      const from = ta.value.lastIndexOf("\n", s - 1) + 1;
      const indent = (ta.value.slice(from, s).match(/^ */) || [""])[0];
      const before = ta.value[s - 1];
      const after = ta.value[s];
      let ins = "\n" + indent;
      if (before === "{" || before === "[") {
        ins += "  ";
        if (after === "}" || after === "]") ins += "\n" + indent;
      }
      ta.setRangeText(ins, s, en, "end");
      if (before === "{" || before === "[") {
        if (after === "}" || after === "]") ta.selectionStart = ta.selectionEnd = s + 1 + indent.length + 2;
      }
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    }
  });
  const format = () => {
    try {
      ta.value = pretty(JSON.parse(ta.value));
      ta.dispatchEvent(new Event("input", { bubbles: true }));
    } catch (_) {
      validate();
    }
  };
  return { ta, err, validate, format };
}

// ---------- record rendering ----------

function indexChips(index, max) {
  const list = index || [];
  const shown = max ? list.slice(0, max) : list;
  const wrap = h("div", { class: "chips" });
  for (const i of shown) wrap.append(h("span", { class: "chip", title: `${i.name} = ${valText(i.value)}` }, h("span", { class: "k", text: i.name }), valText(i.value)));
  if (max && list.length > max) wrap.append(h("span", { class: "chip", text: `+${list.length - max}` }));
  return wrap;
}
function tagChips(tags) {
  const wrap = h("div", { class: "chips" });
  for (const t of tags || []) wrap.append(h("span", { class: "chip tag", text: t }));
  return wrap;
}
function levelBadge(l) {
  return h("span", { class: "bq-badge" + (l === "**" ? " ink" : ""), text: l === "**" ? "private" : `level ${l}` });
}
function recordRows(list, onChange) {
  const body = h("tbody");
  for (const r of list) {
    body.append(
      h(
        "tr",
        { class: "clickable", tabindex: "0", onclick: () => openRecord(r.record_id, onChange), onkeydown: (e) => e.key === "Enter" && openRecord(r.record_id, onChange) },
        h("td", { class: "mono-sm", text: r.record_id }),
        h("td", null, h("div", { text: r.collection.name }), h("span", { class: "bq-cell-sub", text: r.collection.access_level === "**" ? "private" : "level " + r.collection.access_level })),
        h("td", { text: userName(r.user_id) }),
        h("td", null, indexChips(r.index, 3)),
        h("td", null, tagChips(r.tags)),
        h("td", { class: "bq-cell-muted", text: fmtTime(r.updated) }),
        h("td", { class: "overflow bq-cell-muted mono-sm", text: JSON.stringify(r.data) }),
      ),
    );
  }
  return body;
}
function recordMobile(list, onChange) {
  const wrap = h("div", { class: "bq-mobile-list" });
  for (const r of list) {
    wrap.append(
      h(
        "div",
        { class: "bq-mobile-item", onclick: () => openRecord(r.record_id, onChange) },
        h("div", { class: "bq-mobile-item-content" }, h("div", { class: "bq-mobile-item-title", text: `${r.collection.name} · ${r.collection.access_level === "**" ? "private" : "level " + r.collection.access_level}` }), h("div", { class: "bq-mobile-item-id", text: r.record_id }), h("div", { class: "bq-mobile-item-tags", text: [(r.index || []).map((i) => `${i.name}=${valText(i.value)}`).join(" "), (r.tags || []).map((t) => "#" + t).join(" ")].filter(Boolean).join(" · ") || JSON.stringify(r.data) })),
        icon("chevron", "bq-mobile-item-chevron"),
      ),
    );
  }
  return wrap;
}
const RECORD_HEADS = ["Record ID", "Collection", "Owner", "Indexes", "Tags", "Updated", "Data"];

// A paged list of anything: `fetchPage(start_key)` returns {list, start_key, end_of_list};
// `draw(all)` returns the elements for the whole list so far.
function pagedList(container, fetchPage, draw, opts = {}) {
  let all = [];
  let key = null;
  const holder = h("div");
  const moreBtn = h("button", { class: "bq-page-btn", type: "button" }, "Load more", icon("arrowRight", "bq-arrow-icon bq-arrow--trail"));
  const more = h("div", { class: "bq-pagination", hidden: true }, moreBtn);
  const count = h("span", { class: "bq-record-count" });
  let loading = false;
  const reload = () => {
    all = [];
    key = null;
    load();
  };
  const load = async () => {
    loading = true;
    if (opts.onLoading) opts.onLoading(true);
    const r = await run(moreBtn, () => fetchPage(key));
    loading = false;
    if (opts.onLoading) opts.onLoading(false);
    if (!r) return;
    all = all.concat(r.list);
    key = r.end_of_list ? null : r.start_key;
    clear(holder).append(...[].concat(draw(all)));
    more.hidden = r.end_of_list;
    count.textContent = `${all.length} shown${r.end_of_list ? "" : ", more available"}`;
  };
  moreBtn.addEventListener("click", load);
  container.append(holder, more);
  load();
  return { count, reload, isLoading: () => loading };
}
function pagedRecords(container, fetchPage, opts = {}) {
  return pagedList(
    container,
    fetchPage,
    (all) => {
      if (!all.length) return [table(RECORD_HEADS, h("tbody", null, emptyRow(7, opts.empty || "No records found"))), h("div", { class: "bq-mobile-list" }, h("div", { class: "bq-mobile-empty", text: opts.empty || "No records found" }))];
      return [table(RECORD_HEADS, recordRows(all, opts.reload)), recordMobile(all, opts.reload)];
    },
    opts,
  );
}
// The search bar of the list pages: advanced-filter glyph on the left (inside the box), input, clear glyph.
function searchBar(input, onAdv, advActive) {
  const advBtn = h("button", { class: "bq-search-bar-icon-btn" + (advActive ? " active" : ""), type: "button", title: "Advanced filter", "aria-label": "Advanced filter", onclick: () => advBtn.classList.toggle("active", onAdv()) }, icon("tune"));
  const clearBtn = h("button", { class: "bq-search-bar-clear", type: "button", title: "Clear", "aria-label": "Clear search", hidden: !input.value, onclick: () => { input.value = ""; input.dispatchEvent(new Event("input")); input.focus(); } }, icon("close"));
  input.addEventListener("input", () => (clearBtn.hidden = !input.value));
  return h("div", { class: "bq-search-bar" }, advBtn, input, clearBtn);
}
// a search bar with no advanced filter: the magnifying glass sits on the right and runs the search
function plainSearchBar(input, onSearch) {
  const clearBtn = h("button", { class: "bq-search-bar-clear", type: "button", title: "Clear", "aria-label": "Clear search", hidden: !input.value, onclick: () => { input.value = ""; input.dispatchEvent(new Event("input")); input.focus(); } }, icon("close"));
  const goBtn = h("button", { class: "bq-search-bar-icon-btn", type: "button", title: "Search", "aria-label": "Search", onclick: onSearch }, icon("search"));
  input.addEventListener("input", () => (clearBtn.hidden = !input.value));
  input.addEventListener("keydown", (e) => e.key === "Enter" && onSearch());
  return h("div", { class: "bq-search-bar" }, input, clearBtn, goBtn);
}
function refreshBtn(getPager) {
  const b = h("button", { class: "bq-icon-btn", type: "button", title: "Refresh", "aria-label": "Refresh", onclick: () => { const p = getPager(); if (p) p.reload(); } }, icon("refresh"));
  return b;
}

// record detail: the records.vue detail view (back link + bq-adv-search form)
async function openRecord(id, onChange) {
  const r = await run(null, () => admin("record_get", { record_id: id }));
  if (!r) return;
  const main = document.querySelector("main.bq-main");
  const back = () => render();
  const val = (text) => h("span", { class: "bq-form-value", text });
  const sub = r.collection.subscription || {};
  const subOn = Object.entries(sub).filter(([, v]) => v).map(([k]) => k);
  const src = r.source || {};
  const srcOn = [];
  if (src.referencing_limit !== null && src.referencing_limit !== undefined) srcOn.push(`referencing_limit ${src.referencing_limit}`);
  for (const k of ["prevent_multiple_referencing", "can_remove_referencing_records", "only_granted_can_reference", "allow_granted_to_grant_others"]) if (src[k]) srcOn.push(k);
  const grants = h("span", { class: "bq-form-value muted", text: "Loading..." });
  admin("record_grants", { record_id: id })
    .then((g) => {
      clear(grants);
      grants.classList.remove("muted");
      if (!g.list.length) grants.textContent = "No one";
      else grants.append(h("div", { class: "chips" }, g.list.map((x) => h("span", { class: "chip", text: `${userName(x.user_id)} (${x.user_id})` }))));
    })
    .catch(() => (grants.textContent = "Could not load grants."));
  const del = h("button", {
    class: "bq-adv-btn bq-adv-btn--danger",
    type: "button",
    text: "Delete",
    onclick: async () => {
      const ok = await confirmBox({ title: "Delete record", text: `Record ${id} in ${r.collection.name} will be permanently deleted.`, warn: "Records pointing to it lose their reference, or are deleted too if this record allows it. This cannot be undone.", confirm: "Delete", danger: true });
      if (!ok) return;
      const res = await run(del, () => admin("record_del", { record_id: id }));
      if (res) {
        toast(`Deleted ${id}.`);
        if (onChange) onChange();
        render();
      }
    },
  });
  clear(main).append(
    h("button", { class: "bq-back-btn bq-cta-link", type: "button", onclick: back }, h("span", { class: "bq-arrow bq-arrow--lead" }, icon("arrowLeft", "bq-arrow-icon")), "Back to list"),
    h("div", { class: "bq-detail-head" }, h("h2", { text: `Record ${id}` }), h("div", { class: "bq-detail-actions" }, h("button", { class: "bq-tool-btn", type: "button", text: "Copy JSON", onclick: () => copy(pretty(r)) }), h("button", { class: "bq-tool-btn", type: "button", text: "Edit", onclick: () => editRecord(r, onChange) }))),
    h(
      "div",
      { class: "bq-adv-search bq-form-wrap" },
      h(
        "div",
        { class: "bq-adv-form" },
        section("Record", row("Record ID", val(id)), row("Collection", val(r.collection.name)), row("Access level", h("span", { class: "bq-form-value" }, levelBadge(r.collection.access_level))), row("Owner", val(`${userName(r.user_id)} (${r.user_id})`)), r.agent ? row("Agent key", val(typeof r.agent === "string" ? r.agent : `${r.agent.name} (${r.agent.key_id})`)) : null, r.unique_id ? row("Unique ID", val(r.unique_id)) : null, row("Reference", r.reference ? h("span", { class: "bq-form-value" }, h("button", { class: "bq-cta-link", type: "button", text: r.reference, onclick: () => openRecord(r.reference, onChange) })) : val("None")), row("Referenced by", val(`${fmtNum(r.referenced_count)} records`)), row("Read-only", val(r.readonly ? "Yes" : "No")), row("Uploaded", val(fmtTime(r.uploaded))), row("Updated", val(fmtTime(r.updated))), r.ip ? row("Written from", val(r.ip)) : null),
        divider(),
        section("Indexes", r.index && r.index.length ? row(`${r.index.length} index${r.index.length === 1 ? "" : "es"}`, h("span", { class: "bq-form-value" }, indexChips(r.index))) : row("Indexes", val("None"))),
        divider(),
        section("Tags", row("Tags", r.tags && r.tags.length ? h("span", { class: "bq-form-value" }, tagChips(r.tags)) : val("None"))),
        divider(),
        section("Subscription", row("Settings", subOn.length ? h("span", { class: "bq-form-value" }, h("div", { class: "chips" }, subOn.map((k) => h("span", { class: "chip", text: k })))) : val("All off"))),
        divider(),
        section("Source rules", row("Rules", srcOn.length ? h("span", { class: "bq-form-value" }, h("div", { class: "chips" }, srcOn.map((k) => h("span", { class: "chip", text: k })))) : val("Defaults: anyone who can read it may reference it")), src.referencing_index_restrictions && src.referencing_index_restrictions.length ? row("Restrictions", h("div", { class: "bq-adv-rowlist" }, jsonView(src.referencing_index_restrictions))) : null),
        divider(),
        section("Private access", row("Granted to", grants)),
        divider(),
        section("Data", jsonView(r.data)),
        h("div", { class: "bq-adv-footer" }, del, h("span", { class: "bq-adv-footer-spacer" }), h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Back", onclick: back }), h("button", { class: "bq-adv-btn", type: "button", text: "Edit", onclick: () => editRecord(r, onChange) })),
      ),
    ),
  );
  window.scrollTo(0, 0);
}

// ---------- record editor (recordForm.vue shape) ----------

function typeOf(v) {
  return typeof v === "number" ? "number" : typeof v === "boolean" ? "boolean" : "text";
}
function indexRowEditor(list, name, value) {
  const nameIn = advInput({ value: name || "", placeholder: "name, e.g. price or shop.brand", "aria-label": "Index name" });
  const type = advSelect([["text", "String"], ["number", "Number"], ["boolean", "Boolean"]], value === undefined ? "text" : typeOf(value), { "aria-label": "Value type" });
  const valIn = advInput({ value: value === undefined ? "" : String(value), placeholder: "value", "aria-label": "Index value" });
  const boolIn = advSelect([["true", "true"], ["false", "false"]], String(value === false ? "false" : "true"), { "aria-label": "Index value" });
  const sync = () => {
    valIn.hidden = type.value === "boolean";
    boolIn.hidden = type.value !== "boolean";
  };
  type.addEventListener("change", sync);
  sync();
  const rowEl = h("div", { class: "bq-idx-row" }, nameIn, type, h("div", { class: "bq-idx-val" }, valIn, boolIn), h("button", { class: "bq-tool-btn", type: "button", text: "Remove", onclick: () => rowEl.remove() }));
  rowEl._read = () => {
    const n = nameIn.value.trim();
    if (!n) return null;
    let v;
    if (type.value === "boolean") v = boolIn.value === "true";
    else if (type.value === "number") {
      v = Number(valIn.value);
      if (valIn.value.trim() === "" || !Number.isFinite(v)) throw new Error(`Index "${n}" needs a number.`);
    } else v = valIn.value;
    return { name: n, value: v };
  };
  list.append(rowEl);
  return rowEl;
}
function editRecord(rec, onChange) {
  const isNew = !rec;
  const r = rec || { collection: { name: "", access_level: "00", subscription: {} }, index: [], tags: [], source: {}, data: null };
  const main = document.querySelector("main.bq-main");
  const back = () => (isNew ? render() : openRecord(r.record_id, onChange));
  const owner = advInput({ placeholder: "User ID or username of the owner", "aria-label": "Owner" });
  const coll = advInput({ value: r.collection.name, placeholder: "e.g. products", "aria-label": "Collection" });
  const lvl = advSelect(levels(null, true), r.collection.access_level, { "aria-label": "Access level", class: "bq-adv-select bq-adv-select--half" });
  const idxList = h("div", { class: "bq-adv-rowlist" });
  for (const i of r.index || []) indexRowEditor(idxList, i.name, i.value);
  const tags = advInput({ value: (r.tags || []).join(", "), placeholder: "sale, lighting (separate with commas)" });
  const unique = advInput({ value: r.unique_id || "", placeholder: "(optional) e.g. sku-1042" });
  const ref = advInput({ value: r.reference || "", placeholder: "(optional) record id or unique id this record points to" });
  const ro = check("Only the admin can change or delete it afterwards", !!r.readonly);
  const sub = r.collection.subscription || {};
  const subKeys = [["is_subscription_record", "Only subscribers can read it"], ["upload_to_feed", "Put it in subscribers' feeds"], ["notify_subscribers", "Notify subscribers"], ["feed_referencing_records", "Feed me records that point here"], ["notify_referencing_records", "Notify me when a record points here"]];
  const subChecks = subKeys.map(([k, label]) => [k, check(label, !!sub[k])]);
  const nTitle = advInput({ placeholder: "Title" });
  const nBody = advInput({ placeholder: "Text" });
  const notifyRows = [row("Notification", nTitle), row("", nBody)];
  const notifyCheck = subChecks.find(([k]) => k === "notify_subscribers")[1].input;
  const syncNotify = () => notifyRows.forEach((n) => (n.hidden = !notifyCheck.checked || !isNew));
  notifyCheck.addEventListener("change", syncNotify);
  syncNotify();
  const src = r.source || {};
  const limit = advInput({ type: "number", min: "0", value: src.referencing_limit ?? "", placeholder: "unlimited (0 = no one may point here)" });
  const srcChecks = [["prevent_multiple_referencing", "One referencing record per user"], ["can_remove_referencing_records", "Deleting it deletes records that point to it"], ["only_granted_can_reference", "Only granted users may point to it"], ["allow_granted_to_grant_others", "Granted users may grant others"]].map(([k, label]) => [k, check(label, !!src[k])]);
  const restr = jsonArea(pretty(src.referencing_index_restrictions || []), 4, '[{"name": "rating", "value": 1, "range": 5}]');
  const data = jsonArea(r.data === undefined ? "null" : pretty(r.data), 10);
  const preview = h("div");
  const errBox = h("div", { class: "bq-error", hidden: true, role: "alert" });

  const build = () => {
    const cfg = {};
    if (!isNew) cfg.record_id = r.record_id;
    const subscription = {};
    for (const [k, c] of subChecks) subscription[k] = c.input.checked;
    cfg.collection = { name: coll.value.trim(), access_level: lvl.value, subscription };
    const index = [];
    for (const rowEl of idxList.children) {
      const v = rowEl._read && rowEl._read();
      if (v) index.push(v);
    }
    cfg.index = index.length ? index : isNew ? undefined : null;
    const t = tags.value.split(",").map((x) => x.trim()).filter(Boolean);
    cfg.tags = t.length ? t : isNew ? undefined : null;
    cfg.unique_id = unique.value.trim() || (isNew ? undefined : null);
    cfg.reference = ref.value.trim() || (isNew ? undefined : null);
    cfg.readonly = ro.input.checked;
    const source = { referencing_limit: limit.value === "" ? null : Number(limit.value) };
    for (const [k, c] of srcChecks) source[k] = c.input.checked;
    source.referencing_index_restrictions = restr.ta.value.trim() ? JSON.parse(restr.ta.value) : [];
    cfg.source = source;
    if (isNew && notifyCheck.checked && (nTitle.value || nBody.value)) cfg.notification = { title: nTitle.value, body: nBody.value };
    return JSON.parse(JSON.stringify({ data: data.ta.value.trim() === "" ? null : JSON.parse(data.ta.value), config: cfg }));
  };
  const refresh = () => {
    try {
      clear(preview).append(jsonView(isNew ? { as_user: owner.value.trim(), body: build() } : build()));
    } catch (e) {
      clear(preview).append(h("pre", { class: "bq-code", text: "(fix the form: " + e.message + ")" }));
    }
  };
  const save = h("button", { class: "bq-adv-btn", type: "submit", text: isNew ? "Create record" : "Save changes" });
  const form = h(
    "form",
    {
      class: "bq-adv-form",
      autocomplete: "off",
      oninput: refresh,
      onchange: refresh,
      onsubmit: async (e) => {
        e.preventDefault();
        errBox.hidden = true;
        if (!data.validate() || !restr.validate()) return;
        let b;
        try {
          if (isNew && !owner.value.trim()) throw new Error("Enter the user id or username of the owner.");
          b = build();
        } catch (e2) {
          errBox.textContent = e2.message;
          errBox.hidden = false;
          return;
        }
        save.disabled = true;
        try {
          const res = await admin("record_post", isNew ? { as_user: owner.value.trim(), body: b } : { body: b });
          toast(isNew ? `Created ${res.record_id}.` : `Saved ${res.record_id}.`);
          if (onChange) onChange();
          if (isNew) go("records");
          else openRecord(res.record_id, onChange);
        } catch (e2) {
          errBox.textContent = e2.message;
          errBox.hidden = false;
          window.scrollTo(0, document.body.scrollHeight);
        } finally {
          save.disabled = false;
        }
      },
    },
    section("Record", isNew ? row("Owner *", owner) : null, isNew ? h("p", { class: "bq-adv-mode-hint", text: "Records belong to users. The record is created as this user, within their access level." }) : null, row("Collection *", coll), row("Access level", lvl), row("Unique ID", unique), row("Reference", ref), row("Read-only", h("div", { class: "bq-checks" }, ro.el))),
    divider(),
    section("Indexes", row("Indexes", h("div", { class: "bq-adv-rowlist" }, idxList, h("div", null, h("button", { class: "bq-tool-btn", type: "button", text: "+ Add index", onclick: () => { indexRowEditor(idxList); refresh(); } })))), !isNew ? h("p", { class: "bq-adv-mode-hint", text: "Saving replaces every index with the list above." }) : null),
    divider(),
    section("Tags", row("Tags", tags)),
    divider(),
    section("Subscription", row("Settings", h("div", { class: "bq-checks bq-checks--column" }, subChecks.map(([, c]) => c.el))), notifyRows),
    divider(),
    section("Source rules", h("p", { class: "bq-adv-mode-hint", text: "Rules for records that point to this one." }), row("Limit", limit), row("Rules", h("div", { class: "bq-checks bq-checks--column" }, srcChecks.map(([, c]) => c.el))), row("Restrictions", h("div", { class: "bq-adv-rowlist" }, restr.ta, restr.err))),
    divider(),
    section("Data", row("JSON", h("div", { class: "bq-adv-rowlist" }, data.ta, data.err, h("div", null, h("button", { class: "bq-tool-btn", type: "button", text: "Format", onclick: data.format }))))),
    divider(),
    h("details", null, h("summary", { class: "bq-adv-section-title", text: "Request that will be sent" }), preview),
    errBox,
    h("div", { class: "bq-adv-footer" }, h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Cancel", onclick: back }), save),
  );
  clear(main).append(
    h("button", { class: "bq-back-btn bq-cta-link", type: "button", onclick: back }, h("span", { class: "bq-arrow bq-arrow--lead" }, icon("arrowLeft", "bq-arrow-icon")), isNew ? "Back to list" : "Back to record"),
    h("div", { class: "bq-detail-head" }, h("h2", { text: isNew ? "New record" : `Edit ${r.record_id}` })),
    h("div", { class: "bq-adv-search bq-form-wrap" }, form),
  );
  refresh();
  window.scrollTo(0, 0);
  (isNew ? owner : coll).focus();
}

// ---------- pages ----------

async function overview(main) {
  const stats = h("div", { class: "bq-stats" });
  const server = h("div");
  // the version is what the application reports (adminInfo at load, status on refresh), never typed here
  const version = h("span", { class: "bq-brand-version", text: S.info && S.info.version ? `DADABADA v${S.info.version}` : "DADABADA" });
  const banner = h("div", { class: "bq-brand" }, mark(56, true), h("div", { class: "bq-brand-text" }, version, h("span", { class: "bq-brand-tagline", text: TAGLINE })));
  const logBox = h("pre", { class: "bq-log", "aria-label": "Server log", "aria-live": "polite" });
  const logHint = h("p", { class: "bq-adv-mode-hint", text: "What the server prints: one line per request, background work, and a status line every 10 seconds when something changed. Live." });
  main.append(banner, stats, card("Server", server), card("Log", logHint, logBox));
  // live log: ask for lines after the last one seen, every 2 seconds; keep the view at the bottom unless scrolled up
  let lastSeq = 0;
  let shown = 0;
  const pullLog = async () => {
    let r;
    try {
      r = await admin("logs", { after: lastSeq });
    } catch (_) {
      return;
    }
    if (!r.list.length) return;
    const stick = logBox.scrollHeight - logBox.scrollTop - logBox.clientHeight < 8;
    for (const l of r.list) {
      logBox.append(h("span", { class: "bq-log-line", text: l.line + "\n" }));
      shown++;
    }
    while (shown > r.kept) {
      logBox.firstChild.remove();
      shown--;
    }
    lastSeq = r.last;
    if (stick || shown === r.list.length) logBox.scrollTop = logBox.scrollHeight;
  };
  const draw = async () => {
    const s = await run(null, () => admin("status"));
    if (!s) return;
    version.textContent = `DADABADA v${s.version}`;
    clear(stats);
    for (const [k, v] of [["Records", s.records], ["Collections", s.collections], ["Users", s.users], ["Outbox", s.outbox], ["Sessions", s.sessions], ["Connections", s.connections], ["Requests", s.requests], ["Listeners", s.listeners]]) {
      const text = fmtNum(v);
      // a long number shrinks and may wrap at the separators; the exact value stays in the title
      stats.append(h("div", { class: "bq-stat" }, h("span", { class: "bq-stat-label", text: k }), h("span", { class: "bq-stat-value", title: text, "data-long": text.length > 9 ? "1" : null, text })));
    }
    clear(server);
    for (const [k, v] of [["Data folder", s.data], ["Port", `${s.port} (${s.tls ? "TLS" : "plain HTTP"})`], ["Indexes", s.max_indexes ? `${s.max_indexes} per record` : "unlimited"], ["Tags", s.max_tags ? `${s.max_tags} per record` : "unlimited"], ["Notifications", `kept ${s.notify_keep}`], ["Private keys", `last ${s.private_key_ttl}`], ["Loose rules", s.loose_version === s.loose_version_built ? `version ${s.loose_version}` : `stored ${s.loose_version}, built ${s.loose_version_built}: run dadabada admin rebuild-loose`], ["Web admin", `${s.web_admin_sessions} session${s.web_admin_sessions === 1 ? "" : "s"}`]]) {
      server.append(metaRow(k, v));
    }
  };
  await draw();
  await pullLog();
  let n = 0;
  S.timer = setInterval(() => {
    pullLog();
    if (++n % 5 === 0) draw();
  }, 2000);
}

function records(main, params) {
  const coll = advInput({ value: params.collection || "", placeholder: "any", list: "dd-colls", "aria-label": "Collection" });
  const user = advInput({ value: params.user || "", placeholder: "any (user id or username)", "aria-label": "Owner" });
  const lvl = advSelect(levels("any", true), params.level || "", { "aria-label": "Access level", class: "bq-adv-select bq-adv-select--half" });
  const order = advSelect([["desc", "newest first"], ["asc", "oldest first"]], params.order || "desc", { "aria-label": "Order", class: "bq-adv-select bq-adv-select--half" });
  const limit = advSelect([["25", "25"], ["50", "50"], ["100", "100"], ["250", "250"], ["500", "500"]], params.limit || String(PAGE_SIZE), { "aria-label": "Page size", class: "bq-adv-select bq-adv-select--half" });
  const dl = h("datalist", { id: "dd-colls" });
  admin("collections", { body: { fetch: { limit: 500 } } }).then((r) => r.list.forEach((c) => dl.append(h("option", { value: c.collection })))).catch(() => {});
  const apply = () => go("records", { collection: coll.value.trim(), user: user.value.trim(), level: lvl.value, order: order.value, limit: limit.value });
  const search = h("input", { class: "bq-search-bar-input", type: "text", placeholder: "Search by collection name", value: params.collection || "", spellcheck: "false", "aria-label": "Collection", onkeydown: (e) => { if (e.key === "Enter") { coll.value = search.value; apply(); } } });
  let advOpen = !!(params.user || params.level);
  const adv = h("div", { class: "bq-adv-search", hidden: !advOpen }, h("form", { class: "bq-adv-form", onsubmit: (e) => { e.preventDefault(); apply(); } }, section("Filter", row("Collection", coll), row("Owner", user), row("Access level", lvl), row("Order", order), row("Page size", limit)), h("div", { class: "bq-adv-footer" }, h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Clear", onclick: () => go("records") }), h("button", { class: "bq-adv-btn", type: "submit", text: "Show" })), dl));
  const listHolder = h("div");
  let pager;
  const count = h("span", { class: "bq-record-count" });
  const refresh = refreshBtn(() => pager);
  main.append(
    ...pageHead("Records", "Every record in the database, private ones included."),
    h("div", { class: "bq-create-bar" }, h("button", { class: "btn", type: "button", text: "+ Create", onclick: () => editRecord(null, () => pager && pager.reload()) })),
    h("div", { class: "bq-toolbar" }, searchBar(search, () => { advOpen = !advOpen; adv.hidden = !advOpen; return advOpen; }, advOpen), refresh, h("div", { class: "bq-toolbar-right" }, count)),
    adv,
    listHolder,
  );
  const body = { fetch: { limit: Number(params.limit || PAGE_SIZE), ascending: params.order === "asc" } };
  if (params.collection) body.collection = params.collection;
  if (params.user) body.user = params.user;
  if (params.level) body.access_level = params.level;
  pager = pagedRecords(listHolder, (key) => admin("records", { body: Object.assign({}, body, { fetch: Object.assign({}, body.fetch, { start_key: key }) }) }), { onLoading: (on) => refresh.classList.toggle("spinning", on) });
  pager.reload = pager.reload;
  count.replaceWith(pager.count);
}

const CONDS = [["=", "="], [">", ">"], [">=", ">= (starts with)"], ["<", "<"], ["<=", "<= (ends with)"], ["range", "Range"]];
function typed(text, how) {
  const t = text.trim();
  if (how === "text") return text;
  if (how === "number") {
    const n = Number(t);
    if (t === "" || !Number.isFinite(n)) throw new Error(`"${text}" is not a number.`);
    return n;
  }
  if (how === "boolean") {
    if (t !== "true" && t !== "false") throw new Error("Use true or false.");
    return t === "true";
  }
  if (t === "true" || t === "false") return t === "true";
  if (t !== "" && Number.isFinite(Number(t))) return Number(t);
  return text;
}
// The getRecords query form, shared by the Query page and the Tools delete-by-query card.
function queryForm(opts = {}) {
  let mode = opts.jsonOnly ? "json" : "form";
  const tabs = h("div", { class: "bq-tabs", role: "tablist" });
  const tabForm = h("button", { class: "bq-tab active", type: "button", role: "tab", text: "Form" });
  const tabJson = h("button", { class: "bq-tab", type: "button", role: "tab", text: "JSON" });
  tabs.append(tabForm, tabJson);
  const lookup = advSelect([["list", "Search a collection"], ["record_id", "One record by ID"], ["unique_id", "One record by unique ID"]], "list", { "aria-label": "Kind" });
  const one = advInput({ placeholder: "record id or unique id" });
  const dlId = "dd-colls-" + Math.random().toString(36).slice(2, 8);
  const coll = advInput({ placeholder: "Collection name", list: dlId });
  const dl = h("datalist", { id: dlId });
  admin("collections", { body: { fetch: { limit: 500 } } }).then((r) => r.list.forEach((c) => dl.append(h("option", { value: c.collection })))).catch(() => {});
  const lvl = advSelect(levels("every level", true), "", { "aria-label": "Access level", class: "bq-adv-select bq-adv-select--half" });
  const sub = advInput({ placeholder: "(optional) user id, for subscription records" });
  const tag = advInput({ placeholder: "(optional)" });
  const refKind = advSelect([["", "None"], ["record", "Points to record"], ["user", "Uploaded by"]], "", { "aria-label": "Reference", class: "bq-adv-select bq-adv-select--half" });
  const refVal = advInput({ placeholder: "record id, unique id or user id" });
  const idxName = advInput({ placeholder: "Index name, e.g. price, title, shop., $updated" });
  const cond = advSelect(CONDS, "=", { "aria-label": "Condition", class: "bq-adv-select bq-adv-select--half" });
  const val = advInput({ placeholder: "Index value (empty = every record with the index)" });
  const rangeVal = advInput({ placeholder: "Range end (inclusive)" });
  const how = advSelect([["auto", "Auto"], ["text", "String"], ["number", "Number"], ["boolean", "Boolean"]], "auto", { "aria-label": "Value type", class: "bq-adv-select bq-adv-select--half" });
  const loose = check("Loose match: ignore case, accents, spaces and punctuation; Korean initials", false);
  const order = advSelect([["asc", "Ascending"], ["desc", "Descending"]], opts.defaultOrder || "asc", { "aria-label": "Order", class: "bq-adv-select bq-adv-select--half" });
  const limit = advSelect([["10", "10"], ["25", "25"], ["50", "50"], ["100", "100"], ["500", "500"], ["1000", "1000"]], String(PAGE_SIZE), { "aria-label": "Page size", class: "bq-adv-select bq-adv-select--half" });
  const preview = h("div");
  const raw = jsonArea('{\n  "query": {\n    "collection": "products"\n  },\n  "fetch": {\n    "limit": 100\n  }\n}', 14);
  const oneRow = row("ID", one);
  const listPart = [
    section("Collection", row("Name", coll), row("Access level", lvl), row("Subscription", sub)),
    divider(),
    section("Filter", row("Tag", tag), row("Reference", h("div", { class: "bq-adv-inline-group" }, refKind, refVal))),
    divider(),
    section("Index", row("Name", idxName), row("Value", h("div", { class: "bq-adv-inline-group" }, how, val)), row("Condition", h("div", { class: "bq-adv-inline-group" }, cond, rangeVal)), row("", h("div", { class: "bq-checks" }, loose.el))),
    divider(),
  ];
  const formPart = h("div", { class: "bq-adv-form bq-adv-form--flat" }, section("Lookup", row("Kind", lookup), oneRow), divider(), listPart, section("Results", row("Order", order), row("Page size", limit)));
  const jsonPart = h("div", { class: "bq-adv-rowlist" }, raw.ta, raw.err, h("div", null, h("button", { class: "bq-tool-btn", type: "button", text: "Format", onclick: raw.format })));
  const build = () => {
    if (lookup.value !== "list") {
      const v = one.value.trim();
      if (!v) throw new Error("Enter an id.");
      return { query: { [lookup.value]: v } };
    }
    const c = coll.value.trim();
    if (!c) throw new Error("A collection is needed.");
    const q = {};
    q.collection = lvl.value || sub.value.trim() ? { name: c } : c;
    if (lvl.value) q.collection.access_level = lvl.value;
    if (sub.value.trim()) q.collection.subscription = sub.value.trim();
    if (tag.value.trim()) q.tag = tag.value.trim();
    if (refKind.value === "record" && refVal.value.trim()) q.reference = refVal.value.trim();
    if (refKind.value === "user" && refVal.value.trim()) q.reference = { user_id: refVal.value.trim() };
    const n = idxName.value.trim();
    if (n) {
      const ix = { name: n };
      if (val.value !== "") {
        ix.value = typed(val.value, n === "$user_id" || n.endsWith(".") || loose.input.checked ? "text" : how.value);
        if (cond.value === "range") {
          if (rangeVal.value === "") throw new Error("A range needs an end value.");
          ix.range = typed(rangeVal.value, loose.input.checked ? "text" : how.value);
        } else if (cond.value !== "=") ix.condition = cond.value;
      }
      if (loose.input.checked) ix.match = "loose";
      q.index = ix;
    }
    return { query: q, fetch: { limit: Number(limit.value), ascending: order.value === "asc" } };
  };
  // the body to send: from the form, or the JSON editor
  const current = () => {
    if (mode === "json") {
      if (!raw.validate()) throw new Error("The JSON is not valid.");
      return JSON.parse(raw.ta.value);
    }
    return build();
  };
  const sync = () => {
    const single = lookup.value !== "list";
    oneRow.hidden = !single;
    for (const el of listPart) el.hidden = single;
    rangeVal.hidden = cond.value !== "range";
    formPart.hidden = mode !== "form";
    jsonPart.hidden = mode !== "json";
    try {
      clear(preview).append(jsonView(mode === "form" ? build() : JSON.parse(raw.ta.value)));
    } catch (e) {
      clear(preview).append(h("pre", { class: "bq-code", text: "(" + e.message + ")" }));
    }
  };
  const setMode = (m) => {
    mode = m;
    tabForm.classList.toggle("active", m === "form");
    tabJson.classList.toggle("active", m === "json");
    tabForm.setAttribute("aria-selected", String(m === "form"));
    tabJson.setAttribute("aria-selected", String(m === "json"));
    if (m === "json") {
      try {
        raw.ta.value = pretty(build());
      } catch (_) {}
    }
    sync();
  };
  tabForm.addEventListener("click", () => setMode("form"));
  tabJson.addEventListener("click", () => setMode("json"));
  const body = h("div", { class: "bq-adv-form", oninput: sync, onchange: sync }, formPart, jsonPart, h("details", { open: true }, h("summary", { class: "bq-adv-section-title", text: "Request (getRecords body)" }), preview));
  if (opts.jsonOnly) {
    try {
      raw.ta.value = pretty(build());
    } catch (_) {}
  }
  sync();
  return { tabs, body, current, preview, raw };
}
function query(main) {
  const qf = queryForm();
  const errBox = h("div", { class: "bq-error", hidden: true, role: "alert" });
  const results = h("div");
  const runBtn = h("button", { class: "bq-adv-btn", type: "submit", text: "Run query" });
  const form = h(
    "form",
    {
      class: "bq-adv-form",
      onsubmit: async (e) => {
        e.preventDefault();
        errBox.hidden = true;
        let body;
        try {
          body = qf.current();
        } catch (e2) {
          errBox.textContent = e2.message;
          errBox.hidden = false;
          return;
        }
        clear(results);
        const count = h("span", { class: "bq-record-count" });
        results.append(h("div", { class: "bq-toolbar" }, h("span", { class: "bq-adv-section-title bq-adv-section-title--bare", text: "Results" }), h("div", { class: "bq-toolbar-right" }, count)));
        const holder = h("div");
        results.append(holder);
        const p = pagedRecords(holder, async (key) => {
          const b = JSON.parse(JSON.stringify(body));
          if (key) b.fetch = Object.assign({}, b.fetch || {}, { start_key: key });
          try {
            return await admin("query", { body: b });
          } catch (e3) {
            errBox.textContent = e3.message;
            errBox.hidden = false;
            throw e3;
          }
        });
        count.replaceWith(p.count);
      },
    },
    qf.body,
    errBox,
    h("div", { class: "bq-adv-footer" }, h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Copy request", onclick: () => copy(qf.preview.textContent) }), runBtn),
  );
  main.append(...pageHead("Query", "Run any getRecords query with admin rights. The docs list every option."), qf.tabs, h("div", { class: "bq-adv-search" }, form), results);
}

async function collections(main, params) {
  const search = h("input", { class: "bq-search-bar-input", type: "text", placeholder: "Search by collection name", value: params.q || "", spellcheck: "false", "aria-label": "Collection name" });
  const how = advSelect([[">=", "starts with"], ["<=", "ends with"], ["=", "is"]], params.how || ">=", { "aria-label": "Match", class: "bq-adv-select bq-adv-select--half" });
  const find = () => go("collections", { q: search.value.trim(), how: how.value });
  const list = h("div");
  let pager;
  const refresh = refreshBtn(() => pager);
  const count = h("span", { class: "bq-record-count" });
  main.append(
    ...pageHead("Collections", "Collections with their sizes, tags and indexes."),
    h("div", { class: "bq-toolbar" }, plainSearchBar(search, find), how, refresh, h("div", { class: "bq-toolbar-right" }, count)),
    list,
  );
  const heads = ["Collection", { text: "Records", cls: "num" }, { text: "Size", cls: "num" }, "Records by level"];
  pager = pagedList(
    list,
    (key) => admin("collections", { body: { query: params.q ? { collection: params.q, condition: params.how || ">=" } : {}, fetch: { limit: PAGE_SIZE, start_key: key } } }),
    (all) => {
      if (!all.length) return [table(heads, h("tbody", null, emptyRow(4, "No collections found"))), h("div", { class: "bq-mobile-list" }, h("div", { class: "bq-mobile-empty", text: "No collections found" }))];
      const tb = h("tbody");
      const mob = h("div", { class: "bq-mobile-list" });
      for (const c of all) {
        const lv = h("div", { class: "chips" });
        const byLevel = Object.entries(c.number_of_records_by_level || {}).sort(([a], [b]) => (a < b ? -1 : 1));
        for (const [l, n] of byLevel) lv.append(h("span", { class: "chip" }, h("span", { class: "k", text: l }), fmtNum(n)));
        tb.append(h("tr", { class: "clickable", tabindex: "0", onclick: () => openCollection(c), onkeydown: (e) => e.key === "Enter" && openCollection(c) }, h("td", { text: c.collection }), h("td", { class: "num", text: fmtNum(c.number_of_records) }), h("td", { class: "num", text: fmtBytes(c.size) }), h("td", null, lv)));
        mob.append(h("div", { class: "bq-mobile-item", onclick: () => openCollection(c) }, h("div", { class: "bq-mobile-item-content" }, h("div", { class: "bq-mobile-item-title", text: c.collection }), h("div", { class: "bq-mobile-item-tags", text: `${fmtNum(c.number_of_records)} records · ${fmtBytes(c.size)} · ` + byLevel.map(([l, n]) => `${l}:${n}`).join(" ") })), icon("chevron", "bq-mobile-item-chevron")));
      }
      return [table(heads, tb), mob];
    },
    { onLoading: (on) => refresh.classList.toggle("spinning", on) },
  );
  count.replaceWith(pager.count);
}

async function openCollection(c) {
  const main = document.querySelector("main.bq-main");
  const tags = h("span", { class: "bq-form-value muted", text: "Loading..." });
  const idx = h("div", { class: "bq-adv-rowlist" });
  const order = advSelect([["index_name", "Name"], ["number_of_records", "Records"], ["average_number", "Average number"], ["total_number", "Total number"], ["number_count", "Number count"], ["string_count", "String count"], ["bool_count", "Boolean count"], ["average_bool", "Average boolean"]], "index_name", { "aria-label": "Order indexes by", class: "bq-adv-select bq-adv-select--half" });
  const loadIdx = async () => {
    const body = { query: { collection: c.collection }, fetch: { limit: 1000, ascending: order.value === "index_name" } };
    if (order.value !== "index_name") body.query.order = { by: order.value };
    const r = await run(null, () => admin("indexes", { body }));
    if (!r) return;
    clear(idx);
    if (!r.list.length) return idx.append(h("span", { class: "bq-form-value", text: "None" }));
    const tb = h("tbody");
    for (const i of r.list) tb.append(h("tr", null, h("td", { text: i.index }), h("td", { class: "num", text: fmtNum(i.number_of_records) }), h("td", { class: "num", text: i.number_count ? `${fmtNum(i.number_count)} · avg ${fmtNum(i.average_number)}` : "" }), h("td", { class: "num", text: i.string_count ? fmtNum(i.string_count) : "" }), h("td", { class: "num", text: i.boolean_count ? `${fmtNum(i.boolean_count)} · ${fmtNum(i.total_bool)} true` : "" })));
    idx.append(h("div", { class: "tableWrap" }, h("table", { class: "customTbl" }, h("thead", null, h("tr", null, ["Index", "Records", "Numbers", "Strings", "Booleans"].map((t, n) => h("th", { class: n ? "num" : null, text: t })))), tb)));
  };
  order.addEventListener("change", loadIdx);
  clear(main).append(
    h("button", { class: "bq-back-btn bq-cta-link", type: "button", onclick: () => render() }, h("span", { class: "bq-arrow bq-arrow--lead" }, icon("arrowLeft", "bq-arrow-icon")), "Back to list"),
    h("div", { class: "bq-detail-head" }, h("h2", { text: `Collection ${c.collection}` }), h("div", { class: "bq-detail-actions" }, h("button", { class: "bq-tool-btn", type: "button", text: "Query", onclick: () => go("query") }), h("button", { class: "bq-tool-btn", type: "button", onclick: () => go("records", { collection: c.collection }) }, "Browse records", icon("arrowRight", "bq-arrow-icon bq-arrow--trail")))),
    h("div", { class: "bq-adv-search bq-form-wrap" }, h("div", { class: "bq-adv-form" }, section("Collection", row("Records", h("span", { class: "bq-form-value", text: fmtNum(c.number_of_records) })), row("Size", h("span", { class: "bq-form-value", text: fmtBytes(c.size) })), row("Tags", tags)), divider(), section("Indexes", row("Order by", order), row("Indexes", idx)))),
  );
  window.scrollTo(0, 0);
  const t = await run(null, () => admin("tags", { body: { query: { collection: c.collection }, fetch: { limit: 1000 } } }));
  if (t) {
    clear(tags);
    tags.classList.remove("muted");
    if (!t.list.length) tags.textContent = "None";
    else tags.append(h("div", { class: "chips" }, t.list.map((x) => h("span", { class: "chip tag", title: `${x.number_of_records} records`, text: `${x.tag} · ${fmtNum(x.number_of_records)}` }))));
  }
  loadIdx();
}

async function uniques(main, params) {
  const search = h("input", { class: "bq-search-bar-input", type: "text", placeholder: "Search by unique id", value: params.q || "", spellcheck: "false", "aria-label": "Unique id" });
  const how = advSelect([[">=", "starts with"], ["<=", "ends with"], ["=", "is"]], params.how || ">=", { "aria-label": "Match", class: "bq-adv-select bq-adv-select--half" });
  const find = () => go("uniques", { q: search.value.trim(), how: how.value });
  const list = h("div");
  let pager;
  const refresh = refreshBtn(() => pager);
  const count = h("span", { class: "bq-record-count" });
  main.append(
    ...pageHead("Unique IDs", "Every unique id and the record it names. Open one to see the record."),
    h("div", { class: "bq-toolbar" }, plainSearchBar(search, find), how, refresh, h("div", { class: "bq-toolbar-right" }, count)),
    list,
  );
  const heads = ["Unique ID", "Record ID"];
  pager = pagedList(
    list,
    (key) => admin("uniques", { body: { query: params.q ? { unique_id: params.q, condition: params.how || ">=" } : {}, fetch: { limit: PAGE_SIZE, start_key: key } } }),
    (all) => {
      if (!all.length) return [table(heads, h("tbody", null, emptyRow(2, "No unique ids found"))), h("div", { class: "bq-mobile-list" }, h("div", { class: "bq-mobile-empty", text: "No unique ids found" }))];
      const tb = h("tbody");
      const mob = h("div", { class: "bq-mobile-list" });
      for (const u of all) {
        tb.append(h("tr", { class: "clickable", tabindex: "0", onclick: () => openRecord(u.record_id), onkeydown: (e) => e.key === "Enter" && openRecord(u.record_id) }, h("td", { text: u.unique_id }), h("td", { class: "mono-sm", text: u.record_id })));
        mob.append(h("div", { class: "bq-mobile-item", onclick: () => openRecord(u.record_id) }, h("div", { class: "bq-mobile-item-content" }, h("div", { class: "bq-mobile-item-title", text: u.unique_id }), h("div", { class: "bq-mobile-item-id", text: u.record_id })), icon("chevron", "bq-mobile-item-chevron")));
      }
      return [table(heads, tb), mob];
    },
    { onLoading: (on) => refresh.classList.toggle("spinning", on) },
  );
  count.replaceWith(pager.count);
}

const USER_FIELDS = [["created", "Created"], ["level", "Level"], ["records", "Records"], ["subscribers", "Subscribers"], ["subscribed", "Subscribed"]];
async function users(main, params) {
  const list = h("div");
  let showCreate = false;
  const createWrap = h("div", { hidden: true });
  const name = advInput({ placeholder: "Username", autocomplete: "off" });
  const lvl = advSelect(levels(null, false), "00", { "aria-label": "Level", class: "bq-adv-select bq-adv-select--half" });
  const pw = passwordInput({ placeholder: "Password", autocomplete: "new-password", "aria-label": "Password" }, true);
  const addBtn = h("button", { class: "bq-adv-btn", type: "submit", text: "Add user" });
  const createBtn = h("button", { class: "btn", type: "button", text: "+ Add User", onclick: () => { showCreate = !showCreate; createWrap.hidden = !showCreate; if (showCreate) name.focus(); } });
  let pager;
  createWrap.append(
    h(
      "div",
      { class: "bq-adv-search mt-xs" },
      h(
        "form",
        {
          class: "bq-adv-form",
          autocomplete: "off",
          onsubmit: async (e) => {
            e.preventDefault();
            const res = await run(addBtn, () => admin("user_add", { username: name.value.trim(), password: pw.input.value, level: lvl.value }));
            if (res) {
              toast(`Added ${res.username} (user id ${res.user_id}, level ${res.access_level}).`);
              name.value = "";
              pw.input.value = "";
              showCreate = false;
              createWrap.hidden = true;
              loadUsers().catch(() => {});
              if (pager) pager.reload();
              window.scrollTo(0, 0);
            }
          },
        },
        section("Account", row("Username *", name), row("Password *", pw.el), row("Level", lvl), h("p", { class: "bq-adv-mode-hint", text: "Give the password to the user; it is stored only as a hash and cannot be recovered. Level 00 to 99 sets what the user can read and post." })),
        h("div", { class: "bq-adv-footer" }, h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Cancel", onclick: () => { showCreate = false; createWrap.hidden = true; } }), addBtn),
      ),
    ),
  );
  // search: quick box = username (starts with) or a user id; advanced = one field with a condition
  const search = h("input", { class: "bq-search-bar-input", type: "text", placeholder: "Search by username or user id", value: params.q || "", spellcheck: "false", "aria-label": "Username or user id" });
  const field = advSelect(USER_FIELDS, params.field || "created", { "aria-label": "Field", class: "bq-adv-select bq-adv-select--half" });
  const cond = advSelect([["=", "="], [">", ">"], [">=", ">="], ["<", "<"], ["<=", "<="], ["range", "Range"]], params.cond || "=", { "aria-label": "Condition", class: "bq-adv-select bq-adv-select--mid" });
  const val = advInput({ value: params.value || "", placeholder: "value (empty = every user, in this field's order)" });
  const rangeVal = advInput({ value: params.range || "", placeholder: "range end (inclusive)" });
  const order = advSelect([["desc", "newest / largest first"], ["asc", "oldest / smallest first"]], params.order || "desc", { "aria-label": "Order", class: "bq-adv-select bq-adv-select--mid" });
  const syncRange = () => (rangeVal.hidden = cond.value !== "range");
  cond.addEventListener("change", syncRange);
  syncRange();
  const applyAdv = () => go("users", { field: field.value, cond: cond.value, value: val.value.trim(), range: cond.value === "range" ? rangeVal.value.trim() : "", order: order.value });
  const applyQuick = () => go("users", { q: search.value.trim() });
  search.addEventListener("keydown", (e) => e.key === "Enter" && applyQuick());
  let advOpen = !!(params.field || params.value);
  const adv = h("div", { class: "bq-adv-search", hidden: !advOpen }, h("form", { class: "bq-adv-form", onsubmit: (e) => { e.preventDefault(); applyAdv(); } }, section("Search", row("Field", field), row("Condition", h("div", { class: "bq-adv-inline-group" }, cond, rangeVal)), row("Value", val), row("Order", order), h("p", { class: "bq-adv-mode-hint", text: "Created takes a time in milliseconds; level 00 to 99; the counters take numbers. Leave the value empty to list every user in that field's order." })), h("div", { class: "bq-adv-footer" }, h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Clear", onclick: () => go("users") }), h("button", { class: "bq-adv-btn", type: "submit", text: "Search" }))));
  const refresh = refreshBtn(() => pager);
  const count = h("span", { class: "bq-record-count" });
  main.append(
    ...pageHead("Users", "Everyone who can sign in. A lost password cannot be recovered; the account can only be deleted."),
    h("div", { class: "bq-create-bar" }, createBtn),
    createWrap,
    h("div", { class: "bq-toolbar" }, searchBar(search, () => { advOpen = !advOpen; adv.hidden = !advOpen; return advOpen; }, advOpen), refresh, h("div", { class: "bq-toolbar-right" }, count)),
    adv,
    list,
  );
  const heads = ["User", "Level", { text: "Records", cls: "num" }, { text: "Subscribers", cls: "num" }, { text: "Subscribed", cls: "num" }, "Created"];
  const q = {};
  if (params.q) {
    if (/^[A-Za-z0-9]{8}$/.test(params.q) && !S.users.some((u) => u.username === params.q)) q.user_id = params.q;
    else {
      q.username = params.q;
      q.condition = ">=";
    }
  } else {
    q.field = params.field || "created";
    if (params.value) {
      // levels are two-digit strings; "5" typed in the box is sent as "05"
      const lvl = (v) => (/^\d$/.test(v) ? "0" + v : v);
      q.value = q.field === "level" ? lvl(params.value) : Number(params.value);
      if (params.cond === "range" && params.range) q.range = q.field === "level" ? lvl(params.range) : Number(params.range);
      else if (params.cond && params.cond !== "=" && params.cond !== "range") q.condition = params.cond;
    }
  }
  pager = pagedList(
    list,
    (key) => admin("user_search", { body: { query: q, fetch: { limit: PAGE_SIZE, ascending: (params.order || "desc") === "asc", start_key: key } } }),
    (all) => {
      if (!all.length) return [table(heads, h("tbody", null, emptyRow(7, "No users found"))), h("div", { class: "bq-mobile-list" }, h("div", { class: "bq-mobile-empty", text: "No users found" }))];
      const tb = h("tbody");
      const mob = h("div", { class: "bq-mobile-list" });
      for (const u of all) {
        tb.append(h("tr", { class: "clickable", tabindex: "0", onclick: () => openUser(u, () => pager.reload()), onkeydown: (e) => e.key === "Enter" && openUser(u, () => pager.reload()) }, h("td", null, h("div", { text: u.username }), h("span", { class: "bq-cell-sub mono-sm", text: u.user_id })), h("td", { text: u.access_level }), h("td", { class: "num", text: fmtNum(u.records) }), h("td", { class: "num", text: fmtNum(u.subscribers) }), h("td", { class: "num", text: fmtNum(u.subscribed) }), h("td", { class: "bq-cell-muted", text: fmtTime(u.created) })));
        mob.append(h("div", { class: "bq-mobile-item", onclick: () => openUser(u, () => pager.reload()) }, h("div", { class: "bq-mobile-item-content" }, h("div", { class: "bq-mobile-item-title", text: u.username }), h("div", { class: "bq-mobile-item-id", text: u.user_id }), h("div", { class: "bq-mobile-item-tags", text: `Level ${u.access_level} · ${fmtNum(u.records)} records · ${fmtNum(u.subscribers)} subscribers` })), icon("chevron", "bq-mobile-item-chevron")));
      }
      return [table(heads, tb), mob];
    },
    { onLoading: (on) => refresh.classList.toggle("spinning", on) },
  );
  count.replaceWith(pager.count);
}

function openUser(u, onChange) {
  const main = document.querySelector("main.bq-main");
  const back = () => render();
  const val = (text) => h("span", { class: "bq-form-value", text });
  const lv = advSelect(levels(null, false), u.access_level, { "aria-label": "Level", class: "bq-adv-select bq-adv-select--half" });
  const saveLv = h("button", { class: "bq-tool-btn", type: "button", text: "Set level", hidden: true, onclick: async () => {
    const res = await run(saveLv, () => admin("user_level", { user: u.user_id, level: lv.value }));
    if (res) {
      toast(`${u.username} is now level ${res.access_level}.`);
      u.access_level = res.access_level;
      saveLv.hidden = true;
      if (onChange) onChange();
    }
  } });
  lv.addEventListener("change", () => (saveLv.hidden = lv.value === u.access_level));
  const del = h("button", { class: "bq-adv-btn bq-adv-btn--danger", type: "button", text: "Delete user", onclick: async () => {
    const ok = await confirmBox({ title: "Delete user", text: `${u.username} will be permanently deleted, with the ${fmtNum(u.records)} records they uploaded, their grants, subscriptions and agent keys.`, warn: "The account is removed now; the records go in the background. A deleted account cannot be restored.", confirm: "Delete", danger: true, typed: u.username });
    if (!ok) return;
    const res = await run(del, () => admin("user_del", { user: u.user_id }));
    if (res) {
      toast(`Deleted ${u.username}.`);
      loadUsers().catch(() => {});
      render();
    }
  } });
  clear(main).append(
    h("button", { class: "bq-back-btn bq-cta-link", type: "button", onclick: back }, h("span", { class: "bq-arrow bq-arrow--lead" }, icon("arrowLeft", "bq-arrow-icon")), "Back to list"),
    h("div", { class: "bq-detail-head" }, h("h2", { text: u.username }), h("div", { class: "bq-detail-actions" }, h("button", { class: "bq-tool-btn", type: "button", onclick: () => go("records", { user: u.user_id }) }, "Records", icon("arrowRight", "bq-arrow-icon bq-arrow--trail")), h("button", { class: "bq-tool-btn", type: "button", onclick: () => go("keys", { user: u.user_id }) }, "Agent keys", icon("arrowRight", "bq-arrow-icon bq-arrow--trail")))),
    h("div", { class: "bq-adv-search bq-form-wrap" }, h("div", { class: "bq-adv-form" }, section("User info", row("User ID", val(u.user_id)), row("Username", val(u.username)), row("Level", h("div", { class: "bq-adv-inline-group" }, lv, saveLv)), row("Records", val(fmtNum(u.records))), row("Subscribers", val(fmtNum(u.subscribers))), row("Subscribed", val(fmtNum(u.subscribed))), row("Created", val(fmtTime(u.created)))), divider(), section("Password", h("p", { class: "bq-adv-mode-hint", text: "Passwords are stored as hashes and cannot be shown or reset. If it is lost, delete the account and add the user again." })), h("div", { class: "bq-adv-footer" }, del, h("span", { class: "bq-adv-footer-spacer" }), h("button", { class: "bq-adv-btn bq-adv-btn--clear", type: "button", text: "Back", onclick: back })))),
  );
  window.scrollTo(0, 0);
}

async function keys(main, params) {
  const user = h("input", { class: "bq-search-bar-input", type: "text", placeholder: "Search user (user id or username)", value: params.user || "", spellcheck: "false", "aria-label": "Search user" });
  const find = () => go("keys", { user: user.value.trim() });
  const list = h("div");
  main.append(...pageHead("Agent keys", "Keys users made for agents and scripts. Users create them; the admin can see and revoke any of them."), h("div", { class: "bq-toolbar" }, plainSearchBar(user, find)), list);
  const r = await run(null, () => admin("agents", params.user ? { user: params.user } : {}));
  if (!r) return;
  const heads = ["Key", "User", "Limits", "Collections", "Expires", "Created", ""];
  if (!r.length) return list.append(table(heads, h("tbody", null, emptyRow(7, "No agent keys found"))), h("div", { class: "bq-mobile-list" }, h("div", { class: "bq-mobile-empty", text: "No agent keys found" })));
  const tb = h("tbody");
  const mob = h("div", { class: "bq-mobile-list" });
  for (const k of r) {
    const revoke = async (btn) => {
      const ok = await confirmBox({ title: "Revoke agent key", text: `Agent key ${k.key_id} ("${k.name}") of ${userName(k.user_id)} stops working at once.`, confirm: "Revoke", danger: true, iconName: "key" });
      if (!ok) return;
      if (await run(btn, () => admin("agent_revoke", { key_id: k.key_id }))) {
        toast(`Revoked ${k.key_id}.`);
        render();
      }
    };
    const limits = [k.read_only ? "read-only" : "read-write"];
    if (k.max_level) limits.push(`level up to ${k.max_level}`);
    if (k.private) limits.push("private records");
    const rb = h("button", { class: "bq-link-btn bq-danger", type: "button", text: "[Revoke]", onclick: (e) => { e.stopPropagation(); revoke(rb); } });
    tb.append(h("tr", null, h("td", null, h("div", { text: k.name }), h("span", { class: "bq-cell-sub mono-sm", text: k.key_id })), h("td", null, h("div", { text: userName(k.user_id) }), h("span", { class: "bq-cell-sub mono-sm", text: k.user_id })), h("td", null, h("div", { class: "chips" }, limits.map((l) => h("span", { class: "chip", text: l })))), h("td", null, k.collections.length ? h("div", { class: "chips" }, k.collections.map((c) => h("span", { class: "chip", text: c }))) : h("span", { class: "muted", text: "all" })), h("td", { class: "bq-cell-muted", text: k.expires ? fmtTime(k.expires) : "never" }), h("td", { class: "bq-cell-muted", text: fmtTime(k.created) }), h("td", null, rb)));
    const mb = h("button", { class: "bq-tool-btn", type: "button", text: "Revoke", onclick: () => revoke(mb) });
    mob.append(h("div", { class: "bq-mobile-item" }, h("div", { class: "bq-mobile-item-content" }, h("div", { class: "bq-mobile-item-title", text: `${k.name} · ${userName(k.user_id)}` }), h("div", { class: "bq-mobile-item-id", text: k.key_id }), h("div", { class: "bq-mobile-item-tags", text: limits.join(" · ") + (k.collections.length ? " · " + k.collections.join(",") : "") })), mb));
  }
  list.append(table(heads, tb), mob);
}

// [key, label, kind, placeholder]; the label carries the unit in parentheses
const SETTINGS = [
  ["Limits", [["max_indexes", "Indexes per record (0 = unlimited)", "number", "0"], ["max_tags", "Tags per record (0 = unlimited)", "number", "0"]]],
  ["Keep times", [["notify_keep", "Keep missed notifications for", "duration", "1d (30m, 12h, 1d, 7d)"], ["private_key_ttl", "Private access keys last", "duration", "1h (30m, 1h, 1d)"]]],
  ["Sign-in", [["session_idle_hours", "User sessions end after (hours without use)", "number", "24"], ["login_free_attempts", "Failed sign-ins before slowing down", "number", "5"], ["login_max_delay", "Longest wait between sign-in attempts", "duration", "15m"], ["admin_session_idle_minutes", "Admin sessions end after (minutes without use)", "number", "60"]]],
  ["Network", [["port", "Port", "number", "7070"], ["bind", "Listen address (0.0.0.0 = every address, 127.0.0.1 = this machine only)", "text", "0.0.0.0"], ["tls", "TLS (turning it on makes a self-signed certificate if none exists)", "bool", ""], ["tls_cert", "Certificate file, PEM (empty = the one made by init)", "path", "/srv/dadabada/tls/cert.pem"], ["tls_key", "Key file, PEM", "path", "/srv/dadabada/tls/key.pem"]]],
  ["Storage", [["rocksdb.block_cache_mb", "Block cache (MB, at least 8)", "number", "256"], ["rocksdb.blob_min_kb", "Store record bodies in blob files from (KB)", "number", "4"]]],
];
async function settings(main) {
  const s = await run(null, () => admin("settings_get"));
  main.append(...pageHead("Settings", s ? `Saved in ${s.file}.` : ""));
  if (!s) return;
  if (s.pending_restart.length) main.append(h("div", { class: "bq-banner bq-banner--warning" }, icon("warn", "bq-modal-delete-icon"), h("span", { text: `Saved but not in force until the next start: ${s.pending_restart.join(", ")}.` })));
  const inputs = {};
  const err = h("div", { class: "bq-error", hidden: true, role: "alert" });
  const saveBtn = h("button", { class: "btn", type: "submit", text: "Save settings" });
  const groups = h("div", { class: "bq-grid bq-grid--two" });
  for (const [group, items] of SETTINGS) {
    const live = items.every(([k]) => s.live.includes(k));
    const c = card(null);
    c.append(h("h2", { class: "bq-section-title" }, group, h("span", { class: "bq-title-badges" }, h("span", { class: "bq-badge " + (live ? "muted" : "ink"), text: live ? "applies now" : "next start" }))));
    for (const [key, label, kind, ph] of items) {
      const val = s.saved[key];
      let control;
      if (kind === "bool") {
        const cb = check("on", !!val);
        control = cb.el;
        inputs[key] = () => cb.input.checked;
      } else {
        const inp = advInput({ type: kind === "number" ? "number" : "text", value: val === null || val === undefined ? "" : String(val), "aria-label": label, placeholder: ph, class: "bq-adv-input" + (kind === "path" ? " bq-adv-input--wide" : "") });
        if (kind === "number") inp.min = "0";
        control = inp;
        inputs[key] = () => (kind === "number" ? (inp.value.trim() === "" ? null : Number(inp.value)) : kind === "path" ? (inp.value.trim() === "" ? null : inp.value.trim()) : inp.value.trim());
      }
      const running = s.running[key];
      const diff = !s.live.includes(key) && JSON.stringify(running) !== JSON.stringify(val);
      c.append(h("div", { class: "bq-setting" }, h("span", { class: "bq-setting-label" }, label, diff ? h("span", { class: "muted", text: ` (running now: ${running})` }) : null), control));
    }
    groups.append(c);
  }
  main.append(
    h("form", { class: "bq-form", onsubmit: async (e) => {
      e.preventDefault();
      err.hidden = true;
      const changes = {};
      for (const [k, read] of Object.entries(inputs)) {
        const v = read();
        if (JSON.stringify(v) !== JSON.stringify(s.saved[k])) changes[k] = v;
      }
      if (!Object.keys(changes).length) return toast("Nothing changed.");
      saveBtn.disabled = true;
      try {
        const r = await admin("settings_set", { settings: changes });
        const later = Object.keys(changes).filter((k) => !r.live.includes(k));
        toast(later.length ? `Saved. ${later.join(", ")} apply on the next start.` : "Saved and in force.");
        render();
      } catch (e2) {
        err.textContent = e2.message;
        err.hidden = false;
      } finally {
        saveBtn.disabled = false;
      }
    } }, groups, err, h("div", { class: "bq-create-bar bq-create-bar--end" }, saveBtn)),
  );
}

function tools(main) {
  const out = h("div", { class: "bq-tool-out", hidden: true, role: "status" });
  const show = (title, v) => {
    out.hidden = false;
    clear(out).append(h("div", { class: "bq-adv-section-title", text: "Result" }), jsonView(typeof v === "string" ? { message: v } : v));
    out.scrollIntoView({ block: "nearest" });
  };
  const backupDir = advInput({ placeholder: "/srv/backups/dadabada-2026-10-29" });
  const exFile = advInput({ placeholder: "/srv/backups/all.jsonl" });
  const exColl = advInput({ placeholder: "all collections" });
  const exUser = advInput({ placeholder: "all users (user id or username)" });
  const imFile = advInput({ placeholder: "/srv/backups/all.jsonl" });
  const imMode = advSelect([["skip", "skip what exists"], ["replace", "replace what exists"]], "skip", { "aria-label": "Mode" });
  const b1 = h("button", { class: "bq-adv-btn", type: "submit", text: "Back up" });
  const b2 = h("button", { class: "bq-adv-btn", type: "submit", text: "Export" });
  const b3 = h("button", { class: "bq-adv-btn", type: "submit", text: "Import" });
  const b5 = h("button", { class: "bq-link-btn bq-danger", type: "button", text: "[Rotate]" });
  const bRestart = h("button", { class: "bq-link-btn", type: "button", text: "[Restart]" });
  const bStop = h("button", { class: "bq-link-btn bq-danger", type: "button", text: "[Stop]" });
  const serverCmd = async (btn, restart) => {
    const s = await run(btn, () => admin("status"));
    if (!s) return;
    const ok = restart
      ? await confirmBox({ title: "Restart the server", text: "DADABADA stops cleanly (it refuses new requests and finishes the ones in progress) and starts again in the same process, with the same options. Queued background work continues after the restart.", warn: "Users wait a few seconds. Every web admin session ends, so you sign in again.", confirm: "Restart", danger: true, typed: "restart", iconName: "refresh" })
      : await confirmBox({ title: "Stop the server", text: "DADABADA stops cleanly. Users and agents cannot reach it until it is started again.", warn: `This page cannot start it again: on the server, run dadabada start --data ${s.data}`, confirm: "Stop", danger: true, typed: "stop" });
    if (!ok) return;
    const r = await run(btn, () => admin("shutdown", { restart }));
    if (r) serverDown(restart, s.data);
  };
  bRestart.addEventListener("click", () => serverCmd(bRestart, true));
  bStop.addEventListener("click", () => serverCmd(bStop, false));
  b5.addEventListener("click", async () => {
    const ok = await confirmBox({ title: "Rotate the server secret", text: "Every private access key given out so far stops working at once.", warn: "Sessions and agent keys are not affected.", confirm: "Rotate", danger: true, typed: "rotate", iconName: "key" });
    if (!ok) return;
    const r = await run(b5, () => admin("secret_rotate"));
    if (r) show("secret rotate", r.message);
  });
  const formCard = (title, btn, fn, ...rows) => card(title, h("form", { class: "bq-adv-form bq-adv-form--flat", onsubmit: async (e) => { e.preventDefault(); await run(btn, fn); } }, rows, h("div", { class: "bq-adv-footer" }, btn)));

  // delete records by query: count first, then a typed confirmation
  const qf = queryForm({ defaultOrder: "desc", jsonOnly: true });
  const delBtn = h("button", { class: "bq-adv-btn bq-adv-btn--danger", type: "submit", text: "Delete matching records" });
  const delErr = h("div", { class: "bq-error", hidden: true, role: "alert" });
  const delCard = card("Delete records by query", h("form", { class: "bq-adv-form bq-adv-form--flat", onsubmit: async (e) => {
    e.preventDefault();
    delErr.hidden = true;
    let body;
    try {
      body = qf.current();
    } catch (e2) {
      delErr.textContent = e2.message;
      delErr.hidden = false;
      return;
    }
    const c = await run(delBtn, () => admin("records_count", { body }));
    if (!c) return;
    if (!c.count) return toast("The query matches no records.");
    const ok = await confirmBox({ title: "Delete records", text: `${fmtNum(c.count)}${c.capped ? "+" : ""} record${c.count === 1 ? "" : "s"} match this query and will be permanently deleted.`, warn: "Records pointing to them lose their reference, or are deleted too where the deleted record allows it. This cannot be undone.", confirm: "Delete", danger: true, typed: "delete" });
    if (!ok) return;
    const r = await run(delBtn, () => admin("records_del_query", { body }));
    if (r) {
      toast(`Deleted ${fmtNum(r.deleted)} records.`);
      show("delete by query", r);
    }
  } }, qf.body, delErr, h("div", { class: "bq-adv-footer" }, delBtn)));

  // delete users in batch
  const usersTa = h("textarea", { class: "bq-adv-textarea", rows: 5, spellcheck: "false", placeholder: "user ids or usernames, one per line or separated by commas. * deletes every user" });
  const ub = h("button", { class: "bq-adv-btn bq-adv-btn--danger", type: "submit", text: "Delete these users" });
  const usersCard = card("Delete users", h("form", { class: "bq-adv-form bq-adv-form--flat", onsubmit: async (e) => {
    e.preventDefault();
    const list = usersTa.value.split(/[\n,]/).map((x) => x.trim()).filter(Boolean);
    if (!list.length) return toast("Enter at least one user.", true);
    const all = list.includes("*");
    let text = `${list.length} user${list.length === 1 ? "" : "s"} will be permanently deleted, with every record they uploaded, their grants, subscriptions and agent keys.`;
    if (all) {
      const s = await run(ub, () => admin("status"));
      if (!s) return;
      if (!s.users) return toast("There are no users.");
      text = `EVERY user (${fmtNum(s.users)}) will be permanently deleted, with every record, grant, subscription and agent key in the database.`;
    }
    const ok = await confirmBox({ title: all ? "Delete every user" : "Delete users", text, warn: "The accounts are removed now; the records go in the background. Deleted accounts cannot be restored.", confirm: "Delete", danger: true, typed: all ? "delete everyone" : "delete" });
    if (!ok) return;
    const r = await run(ub, () => admin("users_del", { users: list }));
    if (r) {
      toast(`Deleted ${r.deleted.length} user${r.deleted.length === 1 ? "" : "s"}${r.failed.length ? `, ${r.failed.length} failed` : ""}.`);
      loadUsers().catch(() => {});
      show("delete users", r);
    }
  } }, row("Users", usersTa), h("p", { class: "bq-adv-mode-hint", text: "Each account is removed at once; its records are removed in the background." }), h("div", { class: "bq-adv-footer" }, ub)));

  // wipe every record
  const wipe = h("button", { class: "bq-link-btn bq-danger", type: "button", text: "[Delete every record]" });
  wipe.addEventListener("click", async () => {
    const s = await run(wipe, () => admin("status"));
    if (!s) return;
    const ok = await confirmBox({ title: "Delete every record", text: `All ${fmtNum(s.records)} records in every collection will be permanently deleted. Users, agent keys and settings stay.`, warn: "This cannot be undone. Make a backup or an export first.", confirm: "Delete everything", danger: true, typed: "delete everything" });
    if (!ok) return;
    const r = await run(wipe, () => admin("records_del_all"));
    if (r) {
      toast(`Deleted ${fmtNum(r.deleted)} records.`);
      show("delete every record", r);
    }
  });

  main.append(
    ...pageHead("Tools", "Backups, export and import, batch deletes, maintenance. Paths are on the server, and the server process must be able to write there."),
    h(
      "div",
      { class: "bq-grid bq-grid--two" },
      formCard("Back up", b1, async () => show(`backup ${backupDir.value}`, await admin("backup", { dir: backupDir.value.trim() })), row("New folder", backupDir), h("p", { class: "bq-adv-mode-hint", text: "A consistent copy while the server runs. The folder must not exist yet." })),
      formCard("Export", b2, async () => {
        const a = { file: exFile.value.trim() };
        if (exColl.value.trim()) a.collection = exColl.value.trim();
        if (exUser.value.trim()) a.user = exUser.value.trim();
        show(`export ${a.file}`, await admin("export", a));
      }, row("File", exFile), row("Collection", exColl), row("User", exUser), h("p", { class: "bq-adv-mode-hint", text: "JSON lines: users (with password hashes), records, grants, subscriptions, agent keys." })),
      formCard("Import", b3, async () => show(`import ${imFile.value}`, await admin("import", { file: imFile.value.trim(), replace: imMode.value === "replace" })), row("File", imFile), row("Existing", imMode), h("p", { class: "bq-adv-mode-hint", text: "An export file. Ids are kept, so references still work." })),
      delCard,
      usersCard,
      card("Server", settingRow("Restart", "Stop cleanly and start again in the same process, with the same options. A new build at the same path is the one that starts.", bRestart), settingRow("Stop", "Stop cleanly. Starting again is done on the server: dadabada start.", bStop)),
      card("Maintenance", settingRow("Private-key secret", "Cancel every private access key.", b5), settingRow("Every record", "Empty every collection. Users, agent keys and settings stay.", wipe), settingRow("Web admin password", "Set, changed or turned off only on the server, by the operating system user: dadabada admin web-password", null)),
    ),
    out,
  );
}

// ---------- start ----------

initTheme();
window.addEventListener("hashchange", () => {
  S.menuOpen = false;
  if (S.token) render();
});
(async function start() {
  S.token = store("dd_admin");
  if (!S.token) return showLogin();
  try {
    S.info = await post("adminInfo", {}, false);
  } catch (_) {
    /* shown on the login page if needed */
  }
  await boot();
})();
