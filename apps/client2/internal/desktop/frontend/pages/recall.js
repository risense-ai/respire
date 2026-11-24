import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  h,
  summary,
  panel,
  input,
  select,
  execute,
  action,
  confirm,
  resultView,
} from "../ui/core.js";

export function recallChecks(root) {
  const section = document.createElement("section");
  section.className = "section-gap";
  section.innerHTML = panel(
    i18nMessages.m16d6f6f798913fba,
    `<form class="panel-body stack"><div class="form-grid">${input("query", i18nMessages.m80996e730cd1001a, "", { required: true })}${select(
      "mode",
      i18nMessages.m52373e4342173705,
      [
        ["fast", i18nMessages.m9373b92fd00e6772],
        ["quality", i18nMessages.m6889bb143d2a235a],
        ["benchmark", i18nMessages.m93abcd6e8620f77e],
      ],
      "fast",
    )}${i18nMessages.mfdc9328d6920c915}`,
  );
  root.append(section);
  let stopped = false;
  $("[data-stop]", section).onclick = () => {
    stopped = true;
  };
  $("form", section).onsubmit = (event) => {
    event.preventDefault();
    action($("[type=submit]", section), async () => {
      const { query, mode } = Object.fromEntries(new FormData(event.target));
      if (
        mode === "quality" &&
        !(await confirm(
          i18nMessages.m5f0bdec5d7332653,
          i18nMessages.m6e483f4b7027403d,
          i18nMessages.m53210137b13f5418,
        ))
      )
        return;
      const output = $("[data-check-result]", section);
      stopped = false;
      $("[data-stop]", section).disabled = false;
      try {
        if (mode !== "benchmark") {
          const started = performance.now();
          const result = await execute("memory_search", {
            query,
            mode,
            titles: true,
            limit: 20,
          });
          if (!section.isConnected) return;
          output.innerHTML = `${i18nMessages.m2620d8fdb202f52a}${(performance.now() - started).toFixed(0)}${i18nMessages.m48e0bc461fc9d3fd}${h(summary(result).count)}${i18nMessages.m6609876f4d01aef9}${summary(result).selection_fallback ? `${i18nMessages.mb7c63e973858ae67}${h(summary(result).selection_fallback)}</p>` : ""}${resultView(i18nMessages.m1f86cffa5cafe3d1, result)}`;
          return;
        }
        const results = [];
        let engine = "";
        for (let i = 0; i < 9 && !stopped && section.isConnected; i++) {
          output.innerHTML = `${i18nMessages.ma76046c71e7df169}${i + 1}${i18nMessages.mba90ae00395fe93d}${results.length}${i18nMessages.ma5a39986e4d9c5e6}`;
          let started = performance.now();
          const probe = summary(
            await execute("model_probe", { text: `${query} [benchmark ${i}]` }),
          );
          const wall = performance.now() - started;
          if (typeof probe.elapsed_ms !== "number" || !probe.selected)
            throw new Error(i18nMessages.mbdaa8c213ea8f258);
          if (engine && probe.selected !== engine)
            throw new Error(i18nMessages.mbee93459b876b32c);
          engine = probe.selected;
          if (stopped || !section.isConnected) break;
          started = performance.now();
          const recalled = summary(
            await execute("memory_search", {
              query,
              mode: "fast",
              titles: true,
              limit: 20,
            }),
          );
          results.push({
            round: i,
            inference: probe.elapsed_ms,
            wall,
            recall: performance.now() - started,
            count: recalled.count,
            indexed: recalled.indexed_candidates,
            pending: recalled.index_pending,
          });
        }
        if (!section.isConnected) return;
        output.innerHTML = `<p class="hint">${stopped ? i18nMessages.ma7a4cf4c563635ff : i18nMessages.mfd60472736365fa0}${i18nMessages.me2c2d099b9d91261}${h(engine)}${i18nMessages.ma278beeac7ac67f5}${results.map((r) => `<tr><td>${r.round === 0 ? i18nMessages.mdacd6dd2903fa303 : r.round}</td><td>${r.inference.toFixed(1)}</td><td>${r.wall.toFixed(1)}</td><td>${r.recall.toFixed(1)}</td><td>${h(r.count)} / ${h(r.indexed)} / ${h(r.pending)}</td></tr>`).join("")}</tbody></table></div>`;
      } finally {
        $("[data-stop]", section).disabled = true;
      }
    });
  };
  return () => {
    stopped = true;
  };
}
