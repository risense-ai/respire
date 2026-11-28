"use strict";
async function storageModels({el,notice,run,api,result}) {
  async function draw() {
    const [catalog,library] = await Promise.all([api("/v1/models/catalog"),api("/preview/library")]);
    result(rows(catalog.models.filter(m=>library.installed.includes(m.id)).map(m=>{
      const active = m.id === catalog.active_model_id;
      const actions = active ? "" : '<button class="secondary" data-id="'+html(m.id)+'">Uninstall</button>';
      return '<div><strong>'+html(m.display_name)+'</strong><p>'+m.engines.map(e=>html(e.engine.toUpperCase()+(e.available ? " ready" : " unavailable"))).join(" / ")+'</p></div><div class="actions"><span class="pill">'+(active ? "Active" : "Installed")+'</span>'+actions+'</div>';
    })));
  }
  el("results").addEventListener("click",event=>run(async()=>{
    const btn = event.target.closest("button");
    if (!btn) return;
    await api("/preview/uninstall","POST",{model_id:btn.dataset.id});
    notice("Model removed. Runtime remains online.");
    await draw();
  }));
  return draw();
}

const modelSummary = catalog => [catalog.active_model_id,catalog.active_variant,catalog.active_engine].filter(Boolean).join(" \u00b7 ");
const indexCoverage = status => status ? `${status.indexed_count} / ${status.memory_count} indexed` : "No active space";
const mirrorAliases = {"hf-mirror":"https://hf-mirror.com",huggingface:"https://huggingface.co"};
const mirrorPresets = ["auto",...Object.values(mirrorAliases),"https://hf-mirror.net"];

async function modelScreen(health,{el,notice,run,api,bind,result,stream,scope}) {
  const capable = flag => health.capabilities.includes(flag);
  let catalog, families, modelID, variantID, operation, snapshot;
  let starting = false;
  const activity = [];
  const selectedModel = () => families.find(model=>model.id === modelID);
  const selection = () => ({model_id:modelID,variant:variantID,engine:el("engine").value});
  const card = (id,label,detail,selected) => `<button class="secondary" data-id="${html(id)}" aria-pressed="${selected}"><strong>${html(label)}</strong><small>${html(detail)}</small></button>`;
  function draw() {
    result(families.map(model=>{
      const specs = [
        Number.isFinite(model.dimensions) ? `${model.dimensions} dims` : null,
        Number.isFinite(model.max_tokens) ? `${model.max_tokens.toLocaleString("en-US")} tokens` : null,
      ].filter(Boolean).join(" / ");
      return card(model.id,model.display_name,specs || "Legacy model",model.id === modelID);
    }).join(""));
    const variants = selectedModel()?.variants ?? [];
    el("variants").innerHTML = variants.map(variant=>{
      const state = variant.active ? "Active" : variant.installed ? "Installed" : "Available";
      const bytes = variant.total_size_bytes ?? variant.size_bytes;
      const size = bytes === undefined ? "" : `${(bytes / 1e6).toFixed(1)} MB / `;
      return card(variant.id,variant.id,`${size}${state}${variant.recommended ? " / Default" : ""}`,variant.id === variantID);
    }).join("");
    const variant = variants.find(entry=>entry.id === variantID);
    const canStream = variant && capable("sse.operations.v1") && !starting && !operation;
    el("download").disabled = !canStream || !capable("model.managed_download") || variant.installed;
    el("enable").disabled = !canStream || !variant.installed || (variant.active && el("engine").value === catalog.active_engine);
    el("probe").disabled = !variant?.installed;
    el("delete-weight").disabled = !variant?.installed || variant.active;
  }
  function showPipeline() {
    const {assets,operations,inference} = snapshot;
    const running = operations.filter(status=>status.state === "running").length;
    const verified = families.flatMap(model=>model.variants).find(variant=>variant.active)?.verified;
    el("pipeline-state").textContent = `Integrity: ${verified ? "Verified" : "Unknown"} / Workers: ${running} running / Active weights: ${assets.locked ? "Locked" : "Unlocked"}`;
    const indexing = operations.filter(status=>status.state === "running" && status.kind === "model_switch").length;
    el("inference-queues").textContent = `Preview lanes: Foreground ${inference.foreground_queued} queued / Background ${indexing} index builds`;
  }
  async function refresh() {
    const [next,assets,library,status] = await Promise.all([api("/v1/models/catalog"),api("/v1/models/active-assets"),
      api("/preview/library"),scope.activeSpaceId ? api("/v1/status/light") : null]);
    catalog = next;
    families = catalog.models.filter(model=>!model.legacy).map(model=>{
      if (Array.isArray(model.variants) && model.variants.length) return model;
      const id = model.default_variant || "default";
      return {...model,default_variant:id,variants:[{id,file_name:model.id,
        installed:library.installed.includes(model.id),active:model.id === catalog.active_model_id}]};
    });
    const activeFamily = families.find(model=>model.id === catalog.active_model_id);
    if (!selectedModel()) modelID = (activeFamily ?? families[0])?.id;
    const selected = selectedModel();
    if (!selected?.variants.some(variant=>variant.id === variantID)) {
      variantID = selected?.variants.find(variant=>variant.active)?.id ?? selected?.default_variant ?? selected?.variants[0].id;
    }
    const operations = library.operations.filter(status=>status.kind.startsWith("model_"));
    snapshot = {catalog,assets,index:status,operations,inference:library.inference};
    const latency = library.inference.latency_ms;
    el("inference-latency").textContent = latency ? `Preview latency: p50 ${latency.p50} ms / p95 ${latency.p95} ms` : "Preview latency: No samples";
    el("active-model").textContent = modelSummary(catalog);
    el("legacy-active").hidden = !!activeFamily || !catalog.active_model_id;
    el("index-coverage").textContent = indexCoverage(status);
    showPipeline();
    draw();
    return operations.find(status=>status.state === "running");
  }
  async function monitor(id,target) {
    operation = id;
    el("operation").hidden = false;
    el("cancel").disabled = false;
    el("op-target").textContent = target ? [target.model_id,target.variant,target.engine].join(" / ") : "Target unavailable";
    draw();
    try {
      const end = await stream(id,status=>{
        const index = snapshot.operations.findIndex(entry=>entry.operation_id === id);
        if (index === -1) snapshot.operations.push({...status,target});
        else snapshot.operations[index] = {...status,target};
        showPipeline();
        const entry = `${status.kind.replaceAll("_"," ")} / ${status.state} / ${status.progress_percent}%`;
        el("op-progress").value = status.progress_percent;
        const stage = Math.floor(status.progress_percent / 25);
        const switching = status.kind === "model_switch";
        const stages = switching ? ["Verify","Probe","Shadow Index","Switch"] : ["Prepare","Download","Verify","Ready"];
        const steps = el("pipeline-steps");
        steps.setAttribute("aria-label",switching ? "Shadow-switch pipeline" : "Download pipeline");
        steps.innerHTML = stages.map((label,index)=>
          `<li${status.state === "running" && index === stage ? ' aria-current="step"' : ""}>${label}</li>`).join("");
        el("op-state").textContent = entry;
        el("cancel").disabled = status.state !== "running";
        activity.unshift(entry);
        el("activity-log").innerHTML = activity.map(message=>`<li>${html(message)}</li>`).join("");
      });
      notice(end.error?.detail ?? `Operation ${end.state}.`,!!end.error);
      const next = await refresh();
      if (next) await monitor(next.operation_id,next.target);
    } finally {
      operation = null;
      el("cancel").disabled = true;
      draw();
    }
  }
  const {model_mirror:savedMirror} = await api("/v1/host/config");
  const mirror = Object.hasOwn(mirrorAliases,savedMirror) ? mirrorAliases[savedMirror] : savedMirror;
  const custom = !mirrorPresets.includes(mirror);
  el("mirror").value = custom ? "custom" : mirror;
  el("mirror-url").value = custom ? mirror : "";
  el("custom-mirror").hidden = !custom;
  for (const id of ["results","variants"]) el(id).addEventListener("click",event=>{
    const button = event.target.closest("button[data-id]");
    if (!button) return;
    if (id === "results") {
      modelID = button.dataset.id;
      variantID = selectedModel().default_variant ?? selectedModel().variants[0].id;
    } else variantID = button.dataset.id;
    draw();
    el(id).querySelector(`[data-id="${button.dataset.id}"]`).focus();
  });
  bind("mirror",async()=>{
    const value = el("mirror").value;
    el("custom-mirror").hidden = value !== "custom";
    if (value === "custom") el("mirror-url").focus();
    else await api("/v1/host/config","PATCH",{model_mirror:value});
  },"change");
  function downloadMirror() {
    const mirror = el("mirror").value;
    if (mirror !== "custom") return mirror;
    const url = el("mirror-url").value.trim();
    if (!URL.canParse(url) || !["http:","https:"].includes(new URL(url).protocol)) throw new Error("Enter an HTTP or HTTPS mirror URL.");
    return url;
  }
  el("custom-mirror").addEventListener("submit",event=>{
    event.preventDefault();
    return run(async()=>{
      await api("/v1/host/config","PATCH",{model_mirror:downloadMirror()});
      notice("Download mirror saved.");
    });
  });
  bind("engine",draw,"change");
  bind("cancel",()=>api("/v1/operations/"+operation,"DELETE"));
  bind("copy-snapshot",async()=>{
    await navigator.clipboard.writeText(JSON.stringify({...snapshot,activity},null,2));
    notice("Snapshot copied.");
  });
  for (const [button,action] of [["download","downloads"],["enable","switch"]]) bind(button,async()=>{
    if (starting || operation) return;
    const body = selection();
    if (action === "downloads") body.mirror = downloadMirror();
    starting = true;
    draw();
    try {
      await monitor((await api("/v1/models/"+action,"POST",body)).operation_id,body);
    } finally {
      starting = false;
      draw();
    }
  });
  bind("probe",async()=>{
    const probe = await api("/v1/models/probe","POST",selection());
    el("probe-metrics").textContent = `${probe.error?.title ?? "Inference completed"} / ${probe.provider} / Load ${probe.load_ms} ms / Inference ${probe.inference_ms} ms / ${probe.dimensions} dims / L2 ${probe.norm}`;
    el("probe-log").textContent = probe.diagnostic_log;
    el("probe-modal").showModal();
  });
  el("report-probe").href = "https://github.com/risense-ai/respire/issues/new";
  bind("copy-probe",()=>navigator.clipboard.writeText(el("probe-log").textContent));
  bind("delete-weight",async()=>{
    const {model_id,variant} = selection();
    if (!confirm(`Delete ${model_id} / ${variant} weights?`)) return;
    await api("/v1/models/delete","POST",{model_id,variant});
    await refresh();
    notice("Weights deleted.");
  });
  const current = await refresh();
  if (current) await monitor(current.operation_id,current.target);
}
async function setup(view) {
  const {el,notice,run,api,bind,result,metrics,scope,stream,authorizeOwner,ownerApi} = view;
  const health = await api("/v1/health");
  if (!health.capabilities.includes("openapi.v1")) throw new Error("Runtime API unavailable.");
  const list = async(query="") => api("/v1/memories?limit=12"+query);
  const light = () => api("/v1/status/light");
  const library = () => api("/preview/library");
  const showMetrics = s => metrics({"Memories":s.memory_count,"Indexed":s.indexed_count,"Diary entries":s.diary_count});
  const input = () => el("input").value;
  if (page === "models" || page === "storage") {
    if (!health.capabilities.includes("model.catalog.v2")) throw new Error("Model catalog is unavailable.");
    if (page === "storage") {
      let archive;
      bind("export",async()=>{
        archive = await api("/v1/backups/export");
        const url = URL.createObjectURL(new Blob([archive],{type:"application/octet-stream"}));
        const link = document.createElement("a"); link.href = url; link.download = "synthetic-notebook.fixture";
        link.click(); setTimeout(()=>URL.revokeObjectURL(url),1000);
        el("restore").disabled = false;
        notice("Exported "+archive.byteLength+" synthetic bytes. Restore uses this in-memory export only.");
      });
      bind("restore",async()=>{
        if (!confirm("Restore the last synthetic export in this preview session?")) return;
        const op = await api("/v1/backups/import","POST",archive);
        await stream(op.operation_id,s=>notice("Synthetic restore "+s.state+" / "+s.progress_percent+"%"));
      });
    }
    return page === "models" ? modelScreen(health,view) : storageModels(view);
  }
  if (page === "wizard") {
    const render = async()=>{
      const acc = await api("/v1/host/account");
      result(acc.signed_in ? '<span class="pill">Profile ready</span> <a href="'+html(scenarioURL("overview.html").href)+'">Open overview &rarr;</a>' : "<p>Ready to create a local profile.</p>");
      el("action").disabled = acc.signed_in;
    };
    bind("action",async()=>{await api("/v1/host/transactions/login","POST",{mode:"offline_keygen"}); await render();});
    return render();
  }
  if (page === "overview") {
    const [catalog,status,lib] = await Promise.all([api("/v1/models/catalog"),
      scope.activeSpaceId ? light() : null,scope.activeSpaceId ? library() : null]);
    const switches = lib?.operations.filter(operation=>operation.kind === "model_switch") ?? [];
    const running = switches.some(operation=>operation.state === "running");
    el("index-state").textContent = running ? "Shadow Indexing" : switches.at(-1)?.state === "failed" ? "Error" : status ? "Ready" : "No active space";
    const percent = status?.memory_count ? Math.round(100 * status.indexed_count / status.memory_count) : 0;
    el("index-coverage").textContent = status ? `${indexCoverage(status)} (${percent}%)` : "";
    el("index-progress").value = percent;
    el("index-progress").hidden = !status;
    el("model-status").textContent = status ? modelSummary(catalog) : `${modelSummary(catalog)} / No active space`;
    if (!status) return result("<p>No active space. Sign in or create a demo space in Accounts.</p>");
    showMetrics(status);
    return result(notes((await list()).items.slice(0,6)));
  }
  if (page === "workspace") {
    let batch = await list();
    function draw() {
      const items = batch.items;
      const layout = el("mode").value;
      el("results").className = layout === "Cards" ? "cards" : "";
      if (items.length && layout === "Graph") result(items.map(m=>'<div class="graph"><span>'+html(m.parent_id ?? "Notebook")+'</span><i></i><span>'+html(m.title)+'</span></div>').join(""));
      else if (items.length && layout === "Tree") result([...new Set(items.map(m=>m.parent_id))].map(parent=>'<details open><summary>'+html(parent ?? "Notebook roots")+'</summary>'+notes(items.filter(m=>m.parent_id === parent))+'</details>').join(""));
      else result(notes(items));
      el("action").disabled = !batch.next_cursor;
      el("memory").replaceChildren(...items.map(m=>new Option(m.title,m.id)));
      el("edit").disabled = !items.length;
      el("delete").disabled = !items.length;
    }
    bind("edit",async()=>{
      const item = await api("/v1/memories/"+encodeURIComponent(el("memory").value));
      el("editor").hidden = false; el("title").value = item.title; el("content").value = item.content;
      el("editor").dataset.id = item.id; el("title").focus();
    });
    bind("save",async()=>{
      await api("/v1/memories/"+encodeURIComponent(el("editor").dataset.id),"PATCH",{title:el("title").value,content:el("content").value});
      el("editor").hidden = true; batch = await list(); draw(); notice("Memory updated.");
    });
    bind("dismiss",()=>{el("editor").hidden = true;});
    bind("delete",async()=>{
      if (!confirm("Soft-delete the selected synthetic memory?")) return;
      await api("/v1/memories/"+encodeURIComponent(el("memory").value),"DELETE");
      el("editor").hidden = true; batch = await list(); draw(); notice("Memory soft-deleted.");
    });
    bind("first",async()=>{batch = await list(); draw();});
    bind("action",async()=>{batch = await list("&cursor="+batch.next_cursor); draw();});
    bind("mode",draw,"change");
    return draw();
  }
  if (page === "recall" || page === "quickpanel") {
    const search = async()=>{
      const {hits} = await api("/v1/memories/recall","POST",{query:input()});
      result(notes(hits.map(hit=>hit.memory))); notice(hits.length+" matching memories.");
    };
    el("input").addEventListener("keydown",event=>{
      if (event.key === "Enter" && (page === "recall" || event.ctrlKey || event.metaKey)) { event.preventDefault(); run(search); }
    });
    if (page === "recall") { bind("action",search); return search(); }
    bind("search",search);
    bind("action",async()=>{const m = await api("/v1/memories","POST",{content:input()}); result(notes([m])); el("input").value = ""; notice("Note saved.");});
  }
  if (page === "diary") {
    const load = async()=>{
      const {entries} = await api("/v1/diary?date="+el("date").value);
      el("input").value = entries[0]?.content ?? "";
      result(entries.length ? "<small>Saved journal entry</small>" : "<small>No entry for this date.</small>");
    };
    bind("date",load,"change");
    bind("action",async()=>{await api("/v1/diary","POST",{date:el("date").value,content:input()}); await load(); notice("Entry saved.");});
    return load();
  }
  if (page === "organize") {
    let plan;
    bind("suggest",async()=>{
      const {suggestions} = await api("/v1/organize/suggestions?top=8");
      el("suggestions").innerHTML = rows(suggestions.map(s=>'<div><strong>'+html(s.target_id)+'</strong><p>'+html(s.summary)+'</p></div><button data-id="'+html(s.target_id)+'">Split note</button>'),"No suggestions in this scenario.");
    });
    el("suggestions").onclick = e=>run(async()=>{
      const button = e.target.closest("button[data-id]");
      if (!button || !confirm("Split this synthetic note into two linked parts?")) return;
      button.disabled = true;
      try {
        const op = await api("/v1/organize/split","POST",{memory_id:button.dataset.id});
        await stream(op.operation_id,s=>notice("Split "+s.state+" / "+s.progress_percent+"%"));
      } finally { button.disabled = false; }
    });
    result("<p>Preview the proposed moves to review their destinations.</p>");
    bind("action",async()=>{
      plan = await api("/v1/organize/deepen/plan","POST",{});
      result(rows(plan.moves.map(m=>html(m.memory_id)+" &rarr; "+html(m.new_parent_id)),"No moves suggested."));
      el("apply").disabled = !plan.moves.length;
    });
    bind("apply",async()=>{
      el("apply").disabled = true;
      const op = await api("/v1/organize/deepen/apply","POST",{plan_id:plan.plan_id});
      await stream(op.operation_id,s=>notice("Organization "+s.state+" / "+s.progress_percent+"%"));
    });
  }
  if (page === "session-import") {
    const lines = ["Review interview notes","Compare observation methods","Schedule the next field session"];
    result(lines.map((line,i)=>'<label><span><input type="checkbox" value="'+i+'" checked> '+line+'</span></label>').join(""));
    bind("action",async()=>{
      const selected = [...el("results").querySelectorAll("input:checked")];
      if (!selected.length) throw new Error("Select a session note.");
      for (const item of selected) {
        await api("/v1/memories","POST",{title:"Field session",content:lines[item.value]});
        item.checked = false; item.disabled = true;
      }
      el("action").disabled = !el("results").querySelector("input:not(:disabled)");
      notice(selected.length+" session notes imported.");
    });
  }
  if (page === "stats") {
    el("authorize").disabled = !scope.signedIn || !scope.activeSpaceId;
    if (!scope.activeSpaceId) {
      el("mode").disabled = true;
      return result("<p>No active space. Statistics appear after you select a space.</p>");
    }
    let all = [];
    async function reloadDistribution() {
      const items = [];
      let cursor = "0";
      do {
        const batch = await api("/v1/memories?limit=200&cursor="+cursor);
        items.push(...batch.items);
        cursor = batch.next_cursor;
      } while (cursor);
      all = items;
      draw();
    }
    bind("authorize",async()=>{
      await authorizeOwner();
      const [stats,log,conflicts] = await Promise.all([ownerApi("/v1/stats"),ownerApi("/v1/query-log?limit=5"),ownerApi("/v1/sync/conflicts")]);
      metrics({"Total records":stats.local_total,"Alive records":stats.local_alive,"Recorded queries":log.stats.total});
      el("audit").innerHTML = rows(conflicts.items.map(c=>'<strong>'+html(c.memory_id)+'</strong><span>'+html(c.state+" / revision "+c.revision)+'</span>'),"No pending sync conflicts.");
      const first = (await list()).items[0];
      if (first) {
        const reviews = await ownerApi("/v1/memories/"+encodeURIComponent(first.id)+"/reviews?limit=5");
        el("audit").innerHTML += '<h3>Recent review history</h3>'+rows(reviews.items.map(r=>html(r.action+" / "+r.happened_at)),"No review events for the first memory.");
      }
      await reloadDistribution();
    });
    function draw() {
      const counts = new Map();
      for (const m of all) {
        const group = el("mode").value === "Topic" ? m.category : m.parent_id ? "Child notes" : "Root notes";
        counts.set(group,(counts.get(group) ?? 0)+1);
      }
      result(rows([...counts].map(([label,count])=>'<strong>'+html(label)+'</strong><meter max="'+all.length+'" value="'+count+'"></meter><span>'+count+'</span>')));
    }
    bind("mode",draw,"change");
    showMetrics(await light());
    return reloadDistribution();
  }
  if (page === "accounts") {
    bind("sync",async()=>{
      const op = await api("/v1/sync/trigger","POST");
      await stream(op.operation_id,s=>notice("Synthetic sync "+s.state+" / "+s.progress_percent+"%"));
      await render();
    });
    const render = async()=>{
      const [acc,lib,sync] = await Promise.all([api("/v1/host/account"),library(),api("/v1/sync/status")]);
      result(rows(lib.accounts.map(id=>'<strong>'+html(id)+'</strong>'+(acc.account_id === id ? '<span class="pill">Active</span>' : '<button data-id="'+html(id)+'">Switch profile</button>')))+'<p>Pending sync items: '+sync.pending_outbox+'</p>');
    };
    el("results").onclick = e=>run(async()=>{if (e.target.dataset.id) {await api("/preview/account","POST",{id:e.target.dataset.id}); await render(); await drawSpaces();}});
    const drawSpaces = async()=>{
      const {spaces} = await api("/v1/spaces");
      el("spaces").innerHTML = rows(spaces.map(s=>'<strong>'+html(s.name)+'</strong><div class="actions"><button data-action="rename" data-id="'+html(s.id)+'">Rename</button>'+(s.active ? '<span class="pill">Active space</span>' : '<button data-action="activate" data-id="'+html(s.id)+'">Activate</button><button class="secondary" data-action="delete" data-id="'+html(s.id)+'">Delete</button>')+'</div>'));
    };
    bind("create",async()=>{await api("/v1/spaces","POST",{name:el("space-name").value}); el("space-name").value = ""; await drawSpaces(); await render();});
    el("spaces").onclick = e=>run(async()=>{
      const button = e.target.closest("button[data-action]"); if (!button) return;
      const path = "/v1/spaces/"+encodeURIComponent(button.dataset.id);
      if (button.dataset.action === "delete") {
        if (!confirm("Delete this synthetic space and its notes?")) return;
        await api(path,"DELETE");
      } else if (button.dataset.action === "rename") {
        const name = el("space-name").value;
        if (!name.trim()) throw new Error("Enter a space name before renaming.");
        await api(path,"PATCH",{name});
      } else await api(path,"PATCH",{active:true});
      await drawSpaces(); await render();
    });
    await drawSpaces();
    return render();
  }
  if (page === "grants") {
    const draw = async()=>result(rows((await ownerApi("/v1/grants")).items.map(g=>'<strong>'+html(g.label)+'</strong>'+(g.revoked ? '<span class="pill">Revoked</span>' : '<button data-id="'+html(g.id)+'">Revoke grant</button>')),"No grants issued."));
    bind("authorize",async()=>{await authorizeOwner(); await draw();});
    el("results").onclick = e=>run(async()=>{
      const button = e.target.closest("button[data-id]");
      if (button && confirm("Revoke this synthetic grant? Shared memories will be retained.")) {
        await ownerApi("/v1/grants/"+button.dataset.id,"DELETE"); await draw(); notice("Grant revoked. Shared memories retained.");
      }
    });
    return result("<p>Enable the synthetic owner session to load grant metadata.</p>");
  }
  if (page === "space-members") {
    if (!scope.activeSpaceId) {
      el("space").disabled = true;
      el("action").disabled = true;
      return result("<p>No active space. Select a space in Accounts to see its members.</p>");
    }
    const {spaces} = await api("/v1/spaces");
    for (const s of spaces) el("space").add(new Option(s.name,s.id,false,s.active));
    const path = () => "/v1/spaces/"+encodeURIComponent(el("space").value)+"/members";
    const draw = async()=>result(rows((await api(path())).members.map(m=>'<strong>'+html(m.display_name ?? m.member_id)+'</strong><span>'+html(m.role+" / "+m.status)+'</span>')));
    bind("space",draw,"change");
    bind("action",async()=>{await api(path(),"POST",{identifier:input(),role:el("role").value}); await draw(); notice("Synthetic invitation added to the selected space.");});
    return draw();
  }
  if (page === "injection") {
    const draw = ()=>{el("results").textContent = (el("mode").value === "Zed" ? ".rules" : ".github/copilot-instructions.md")+"\n\nRecall related notebook entries before starting a task."; notice("");};
    bind("mode",draw,"change");
    bind("action",()=>{el("results").textContent += "\n\nApplied in this synthetic session."; el("action").disabled = true;});
    el("mode").addEventListener("change",()=>{el("action").disabled = false;});
    return draw();
  }
  if (page === "settings") {
    const draw = async()=>{const config = await api("/v1/host/config"); el("mode").value = config.diary_mode; result("<p>Current mode: "+html(config.diary_mode)+"</p>");};
    bind("action",async()=>{await api("/v1/host/config","PATCH",{diary_mode:el("mode").value}); await draw(); notice("Settings saved.");});
    return draw();
  }
}
startPreview();
