import { messages as i18nMessages } from "./i18n/messages.js";
// Pure presentation of the proxy snapshot returned by the desktop bridge
// (/api/state → proxy). Kept free of DOM and i18n imports so the settings
// page and its Node test can share exactly the same row builder.
//
// Labels are deliberately specific ("Proxy source" rather than "Source") so the
// shared translation table cannot accidentally rewrite unrelated UI text.
const sourceNames = {
  environment: i18nMessages.me523a41d10a6490b,
  system: i18nMessages.m24bbc60c5fb3b169,
  direct: i18nMessages.m06ae310b6a9b1ce4,
  off: i18nMessages.m29a31f33c0978424,
  unavailable: i18nMessages.m17555f381702f8c7,
};

export function proxyRows(proxy) {
  const info = proxy || {};
  const rows = [[i18nMessages.m805c408a0d547ad7, sourceNames[info.source] || i18nMessages.m90b31ad093acf71f]];
  if (info.http) rows.push([i18nMessages.mfe51a8c86091b1e6, info.http]);
  if (info.https) rows.push([i18nMessages.m67047f316c5f3c88, info.https]);
  if (info.noProxy) rows.push([i18nMessages.mb7a8cb3914c9c746, info.noProxy]);
  if (info.note) rows.push([i18nMessages.ma675fb3508dd344f, info.note]);
  return rows;
}
