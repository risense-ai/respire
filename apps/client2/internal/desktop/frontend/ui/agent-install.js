import { messages as i18nMessages } from "./i18n/messages.js";
// Shared install-mode rules for the two install surfaces (onboarding wizard
// and the injection page). One wording and qualification source keeps the
// goal lists, statuses and receipts from drifting apart.
export const AGENT_STATE_NAMES = {
  fresh: i18nMessages.m45e8dc062d9442d9,
  stale: i18nMessages.m2360fdc65bc8a542,
  none: i18nMessages.mdcaae157c6b5c6b1,
  absent: i18nMessages.mea82e426ec0f953a,
};

// Scope support: prompt targets need their own prompt file; plugin targets
// need a matching Respire plugin. Prompt-only copies of plugin-only hosts
// are excluded here, never by the backend.
export function scopeSupports(scope, target) {
  if (scope === "prompt") return target.mode !== "plugin";
  if (scope === "plugin") return Boolean(target.plugin);
  return false;
}

// "Detected" gates the default goal list on both surfaces; the install
// page's Show more is the only explicit bypass, and it still keeps the
// scope filter above.
export function targetDetected(target) {
  return target.likelyInstalled === true;
}

export function targetsForScope(
  targets,
  scope,
  { detectedOnly = false } = {},
) {
  return (targets || []).filter(
    (target) =>
      scopeSupports(scope, target) && (!detectedOnly || targetDetected(target)),
  );
}

// Status follows the active scope: a dual-capability target reports its
// prompt state on the prompt side and its plugin state on the plugin side.
export function agentStatus(target, scope = "prompt") {
  if (scope === "plugin" ? target.plugin : target.mode === "plugin")
    return target.plugin?.source ? i18nMessages.mb4de11ea6d82ee58 : i18nMessages.m144d3447574a4a03;
  return AGENT_STATE_NAMES[target.state] || target.state || i18nMessages.m4d8c1c5b42830791;
}

// agentResultMessage mirrors inject-set receipts so the wizard and the
// install page report the same per-target outcome wording.
export function agentResultMessage(result) {
  if (result.message) return result.message;
  const parts = [];
  if (result.changed !== undefined)
    parts.push(
      result.changed
        ? result.enabled
          ? i18nMessages.maa76aaf71c82a086
          : i18nMessages.m2494b564825e6582
        : i18nMessages.m3c66ead36a955724,
    );
  if (result.plugin)
    parts.push(
      result.plugin.ok
        ? `${i18nMessages.m53e6d2cfb6087e85}${result.plugin.message}`
        : `${i18nMessages.mfdf8b6fd8dd6db38}${result.plugin.message}`,
    );
  return parts.join(" · ") || i18nMessages.mf28461bb49c85647;
}
