import { messages as i18nMessages } from "../ui/i18n/messages.js";
export function entry(value) {
  const candidate = value?.details?.entry || value?.entry || value;
  if (!candidate || typeof candidate.id !== "string")
    throw new Error(i18nMessages.m61282823c8f47717);
  return candidate;
}
export function list(envelope) {
  if (!Array.isArray(envelope?.details))
    throw new Error(i18nMessages.me322468834bcc755);
  return envelope.details.map((value) => entry(value));
}
export function inventory(records) {
  const byId = new Map(records.map((row) => [row.id, row]));
  const children = new Map();
  for (const row of records) {
    const parent = byId.has(row.parent_id) ? row.parent_id : "";
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent).push(row.id);
  }
  return { byId, children };
}
export const kindNames = {
  knowledge: i18nMessages.m2ce2847cbc4743b6,
  context: i18nMessages.m6fc38ef3602fe789,
  decision: i18nMessages.mf4aafaba3be7ad87,
  preference: i18nMessages.m58cc643c3c0d4d56,
  task: i18nMessages.m5253040db8643c85,
  emotion: i18nMessages.m7f1bc9fc4867e103,
  time: i18nMessages.m8b6ff498515bcc2f,
  skill: i18nMessages.m99aea2f9131ad6da,
};
export function report(envelope) {
  return {
    ...(envelope?.summary || {}),
    ...(envelope?.details && !Array.isArray(envelope.details)
      ? envelope.details
      : {}),
  };
}
