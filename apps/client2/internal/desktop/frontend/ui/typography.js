import { messages as i18nMessages } from "./i18n/messages.js";
import { $, h, api, input, select, panel, action, notify } from "./core.js";

const key = "respire-appearance";
const defaults = {
  uiFont: "",
  readFont: "",
  uiScale: 1,
  readSize: 16,
  readLh: 1.95,
};
const bounds = { uiScale: [0.6, 2], readSize: [12, 30], readLh: [1.4, 2.8] };
export function normalizeTypography(value = {}) {
  const result = { ...defaults };
  for (const name of ["uiFont", "readFont"])
    if (typeof value[name] === "string") result[name] = value[name].trim();
  for (const [name, [min, max]] of Object.entries(bounds)) {
    const number = Number(value[name]);
    if (Number.isFinite(number))
      result[name] = Math.min(max, Math.max(min, number));
  }
  return result;
}
export function loadTypography() {
  let value;
  try {
    value = JSON.parse(localStorage.getItem(key));
  } catch {
    value = null;
  }
  return normalizeTypography(value && typeof value === "object" ? value : {});
}
export function applyTypography(value = loadTypography()) {
  const root = document.documentElement;
  for (const [name, variable] of [
    ["uiFont", "--user-ui-font"],
    ["readFont", "--user-read-font"],
  ]) {
    if (value[name])
      root.style.setProperty(variable, JSON.stringify(value[name]));
    else root.style.removeProperty(variable);
  }
  root.style.setProperty("--read-size", value.readSize + "px");
  root.style.setProperty("--read-lh", String(value.readLh));
  root.style.setProperty("--ui-scale", String(value.uiScale));
  root.style.zoom = value.uiScale === 1 ? "" : value.uiScale;
}
window.addEventListener(
  "wheel",
  (event) => {
    if (!event.ctrlKey || event.defaultPrevented) return;
    event.preventDefault();
    const value = loadTypography();
    value.uiScale = event.deltaY < 0
      ? Math.min(bounds.uiScale[1], Math.round((value.uiScale + 0.05) * 100) / 100)
      : Math.max(bounds.uiScale[0], Math.round((value.uiScale - 0.05) * 100) / 100);
    localStorage.setItem(key, JSON.stringify(value));
    applyTypography(value);
    const control = document.querySelector('[name=uiScale]');
    if (control) control.value = String(value.uiScale);
  },
  { passive: false },
);
export function typographyPanel(root) {
  const section = document.createElement("section");
  section.className = "section-gap";
  const value = loadTypography();
  section.innerHTML = panel(
    i18nMessages.m9769e297c725528a,
    `${i18nMessages.mcdc39f6f046c5a7d}${input("uiFont", i18nMessages.m4584d8bc78d31a33, value.uiFont)}${input("readFont", i18nMessages.m5e8ac11194ea9c33, value.readFont)}${select(
      "uiScale",
      i18nMessages.m3a3ebd8ad6599924,
      [0.8, 0.9, 1, 1.1, 1.25, 1.4, 1.6].map((n) => [
        String(n),
        Math.round(n * 100) + "%",
      ]),
      String(value.uiScale),
    )}${input("readSize", i18nMessages.m7be4c74a440a9e9c, value.readSize, { type: "number", required: true })}${input("readLh", i18nMessages.me763a14b448d9b48, value.readLh, { type: "number", required: true })}${i18nMessages.mafe873d3747e496a}`,
  );
  root.append(section);
  for (const name of ["uiFont", "readFont"])
    $(`[name=${name}]`, section).setAttribute("list", "system-fonts");
  for (const [name, [min, max]] of Object.entries(bounds)) {
    const control = $(`[name=${name}]`, section);
    control.min = min;
    control.max = max;
    if (control.tagName === "INPUT")
      control.step = name === "readSize" ? 1 : 0.05;
  }
  const save = (value) => {
    localStorage.setItem(key, JSON.stringify(value));
    applyTypography(value);
    notify(i18nMessages.m6bc5c800e6c1cb1f);
  };
  $("form", section).onsubmit = (event) => {
    event.preventDefault();
    action($("[type=submit]", section), async () =>
      save(normalizeTypography(Object.fromEntries(new FormData(event.target)))),
    );
  };
  $("[data-reset]", section).onclick = (event) =>
    action(event.currentTarget, async () => {
      save(defaults);
      for (const [name, value] of Object.entries(defaults))
        $(`[name=${name}]`, section).value = value;
    });
  $("[data-fonts]", section).onclick = (event) =>
    action(event.currentTarget, async () => {
      const names = await api("fonts");
      $("datalist", section).innerHTML = names
        .map((name) => `<option value="${h(name)}"></option>`)
        .join("");
      notify(`${i18nMessages.m9dd8504569bf1e5c}${names.length}${i18nMessages.m8c76a08cfc414f43}`);
    });
}
