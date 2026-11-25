import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  all,
  h,
  input,
  select,
  dialog,
  confirm,
  execute,
  rows,
  summary,
  notify,
} from "../ui/core.js";
import { entry, kindNames } from "./model.js";

export function editor({ memory = null, parent = "", merge = [], onSaved }) {
  const isEdit = Boolean(memory?.id);
  const element = dialog({
    title: merge.length ? i18nMessages.m6ddee19d156f840c : isEdit ? i18nMessages.m64109d1badb9c34b : i18nMessages.m5b1f957445009143,
    wide: true,
    submit: merge.length ? i18nMessages.m0692fab592dd4ca2 : i18nMessages.ma3030bf8f16dc63c,
    body:
      `<div class="form-grid">${input("title", i18nMessages.mc3405f8c7d9d392a, memory?.title || "", { required: true })}${select("type", i18nMessages.mba40014ff496f64e, Object.entries(kindNames), String(memory?.kind || "context").toLowerCase())}${select(
        "importance",
        i18nMessages.m2cba3baa4753bf6d,
        [
          ["important", i18nMessages.mf69c3342e7848220],
          ["trivial", i18nMessages.m3c7aeeff30223416],
        ],
        memory?.importance || "important",
      )}${!isEdit || merge.length ? input("parent", i18nMessages.m92eea16bf73d86f4, memory?.parent_id || parent) : ""}</div>` +
      `${i18nMessages.m4cbca6388f352d15}${h(memory?.content || "")}${i18nMessages.m385100bdd5f0dc94}` +
      `${input("tags", i18nMessages.m34544a92724c9411, Array.isArray(memory?.tags) ? memory.tags.join(",") : memory?.tags || "")}<div data-candidates-list></div>` +
      (!isEdit
        ? `${i18nMessages.m642401f8b75bd1f0}`
        : "") +
      (merge.length
        ? `${i18nMessages.mc0793d9af9358b80}${merge.length}${i18nMessages.m2326dd1a08da49e5}`
        : ""),
    onSubmit: async (values) => {
      const args = {
        title: values.title.trim(),
        content: values.content,
        type: values.type,
        importance: values.importance,
        tags: values.tags,
      };
      let envelope;
      if (isEdit && !merge.length) {
        const current = entry(await execute("memory_show", { id: memory.id }));
        if (current.updated_at !== memory.updated_at)
          throw new Error(i18nMessages.m25f198ee791de831);
        delete args.type;
        args.kind = values.type;
        args.id = memory.id;
        envelope = await execute("memory_update", args);
      } else {
        args.parent = values.parent.trim();
        if (!args.parent) delete args.parent;
        if (values.force) args.force = true;
        if (merge.length) {
          if (
            !(await confirm(
              i18nMessages.m06669a1ab0c76a98,
              `${i18nMessages.me594342a55a002cf}${merge.length}${i18nMessages.m529676e5e1c9bfc3}`,
              i18nMessages.m06669a1ab0c76a98,
            ))
          )
            return false;
          args.merge_ids = merge.join(",");
        }
        envelope = await execute("memory_create", args);
        const status = envelope?.status;
        if (status === "candidates" || !summary(envelope).id)
          throw new Error(
            i18nMessages.mfd52803bca1371ad,
          );
      }
      notify(merge.length ? i18nMessages.m37b98753bcfc7f83 : i18nMessages.mf54726ed5fe33df0);
      await onSaved?.(
        summary(envelope).id || memory?.id,
        merge.length
          ? null
          : {
              kind: isEdit ? "edit" : "create",
              before: memory,
              importance: values.importance,
              action: summary(envelope).action,
            },
      );
    },
  });
  $("[data-template]", element).addEventListener("click", () => {
    const field = $("[name=content]", element);
    if (!field.value.trim()) field.value = i18nMessages.mb219aa3abd310dab;
    field.focus();
  });
  $("[data-candidates]", element).addEventListener("click", async (event) => {
    const button = event.currentTarget,
      content = $("[name=content]", element).value.trim();
    if (!content) {
      notify(i18nMessages.m51e3f68859ea0c65, true);
      return;
    }
    button.disabled = true;
    const area = $("[data-candidates-list]", element);
    area.innerHTML = i18nMessages.mfd4b6a38ba0533a8;
    try {
      const result = await execute("memory_candidates", { content });
      const report = result.details || {};
      const source = Array.isArray(result.details)
        ? result.details
        : [...(report.merge || []), ...(report.parent || [])];
      const candidates = source
        .map((value) => {
          try {
            return entry(value);
          } catch {
            return null;
          }
        })
        .filter(
          (item, index, items) =>
            item && items.findIndex((other) => other?.id === item.id) === index,
        );
      if (!candidates.length) {
        area.innerHTML =
          i18nMessages.mc5bfb0c47101036d;
        return;
      }
      area.innerHTML = `<div class="candidate-list">${candidates.map((row) => `<div><div><strong>${h(row.title)}</strong><small>${h(row.id)}</small></div><button type="button" data-parent="${h(row.id)}${i18nMessages.maa7a470238218e18}${h(row.id)}${i18nMessages.m3f54d042968a1d86}`).join("")}</div>`;
      all("[data-parent]", area).forEach((control) =>
        control.addEventListener("click", () => {
          const parentField = $("[name=parent]", element);
          if (parentField) parentField.value = control.dataset.parent;
          else notify(i18nMessages.m76d37b83dc0ff550);
        }),
      );
      all("[data-read]", area).forEach((control) =>
        control.addEventListener("click", async () => {
          try {
            const item = entry(
              await execute("memory_show", { id: control.dataset.read }),
            );
            dialog({
              title: item.title,
              body: `<div class="reading-content">${h(item.content)}</div>`,
              submit: i18nMessages.m3fd47edce45b3603,
              onSubmit: () => {},
              wide: true,
            });
          } catch (error) {
            notify(error.message, true);
          }
        }),
      );
    } catch (error) {
      area.textContent = error.message;
      area.classList.add("form-error");
    } finally {
      button.disabled = false;
    }
  });
  return element;
}
