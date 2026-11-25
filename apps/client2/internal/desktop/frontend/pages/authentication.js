import { messages as i18nMessages } from "../ui/i18n/messages.js";
import {
  $,
  h,
  raw,
  input,
  panel,
  dialog,
  execute,
  api,
  waitForOperation,
  select,
  action,
  confirm,
  notify,
} from "../ui/core.js";

// Shared account operations and recovery-material handling. The accounts
// page and the first-run wizard both use these so the confirmation rules
// cannot drift apart.

// issuedRecoveryMaterial extracts recovery material a server or offline
// keygen actually issued; null means no new material exists and no
// save-confirmation step may be faked.
export function issuedRecoveryMaterial(envelope) {
  const detail = envelope?.details || {},
    s = envelope?.summary || {};
  const issued =
    detail.super ||
    detail.super_key ||
    detail.super_issued ||
    s.super ||
    s.super_key ||
    s.super_issued ||
    s.super_password ||
    s.login?.super_issued;
  if (!issued) return null;
  return { material: issued, user: detail.user || s.user || "" };
}

export function recoveryMaterialJson(issued) {
  return raw({ super: issued.material, user: issued.user });
}

const RECOVERY_NOTE =
  i18nMessages.mef29b05210c1035a;

// recoveryMaterialDialog shows issued material behind a mandatory
// save-acknowledgement checkbox; the close buttons and Escape are removed so
// the receipt cannot be dismissed without the acknowledgement.
export function recoveryMaterialDialog(envelope, { note = RECOVERY_NOTE, requestId = envelope?.recoveryRequestId } = {}) {
  const issued = issuedRecoveryMaterial(envelope);
  if (!issued) return null;
  const element = dialog({
    title: i18nMessages.m7136fc12e1ae20be,
    wide: true,
    body:
      `<p class="warning-text">${h(note)}${i18nMessages.m4b11469f902b3e73}${h(
        recoveryMaterialJson(issued),
      )}${i18nMessages.m4ecf6814330b810e}`,
    submit: i18nMessages.m62ac5270415c446f,
    onSubmit: async () => {
      if (!$("[data-saved]", element).checked) return false;
      if (requestId) await api("recovery-material-ack", { id: requestId, confirmed: true });
      notify(i18nMessages.mc93e754283d1f95d);
      document.dispatchEvent(new CustomEvent("respire-operation"));
    },
  });
  element.querySelectorAll("[data-close]").forEach((button) => button.remove());
  element.addEventListener("cancel", (event) => event.preventDefault());
  const submit = $("[type=submit]", element),
    checkbox = $("[data-saved]", element);
  submit.disabled = true;
  checkbox.addEventListener("change", () => {
    submit.disabled = !checkbox.checked;
  });
  return element;
}

export async function restorePendingRecoveryMaterials() {
  const jobs = await api("operations");
  for (const pending of jobs.filter((job) => job.materialPending)) {
    const job = await api("operation-recover", { id: pending.id });
    if (job.state !== "done") {
      notify(job.error || i18nMessages.ma5e995778f27d199, true);
      continue;
    }
    if (!job.materialPending) continue;
    const element = recoveryMaterialDialog(job.envelope, { requestId: job.id });
    if (!element) throw Error(i18nMessages.m8afa145bb2bfb64d);
    await new Promise((resolve) => element.addEventListener("close", resolve, { once: true }));
  }
}

export function validateAuthentication(kind, values) {
  if (
    kind === "register" &&
    (values.password.length < 8 || values.password !== values.confirmation)
  )
    throw Error(i18nMessages.mcafcf8babfb57fc5);
}

// Credentials stay out of arguments and client settings.
export async function runAuthentication(kind, values) {
  const credentials = { password: values.password };
  if (values.super) credentials.super = values.super;
  if (values.totp) credentials.totp = values.totp.trim();
  return executeAuthentication(
    "auth_" + kind,
    { addr: values.addr.trim(), user: values.user.trim() },
    { secret: JSON.stringify(credentials) },
  );
}

// Keep only request IDs in memory, so cancelling an input dialog never starts
// another authentication when the original form is submitted again.
const pendingAuthentication = new Map();
export async function executeAuthentication(name, args = {}, options = {}) {
  const key = JSON.stringify([name, args]);
  const previous = pendingAuthentication.get(key);
  try {
    let result;
    if (previous) {
      result = await continueAuthentication(previous);
    } else {
      try {
        result = await execute(name, args, options);
      } catch (error) {
        if (!error.requestId || error.operation?.state === "failed") throw error;
        pendingAuthentication.set(key, error.requestId);
        result = await continueAuthentication(error.requestId);
      }
    }
    pendingAuthentication.delete(key);
    return result;
  } catch (error) {
    if (error.operation?.state === "failed") pendingAuthentication.delete(key);
    throw error;
  }
}

const supplementaryLabels = {
  totp: i18nMessages.m0f8abdaa854f5c9c,
  legacy_password: i18nMessages.m30fd27e584429262,
  account_secret: "Account Secret",
  source_secret: i18nMessages.mef1ef33cb00d30fb,
  secret_key: "Secret Key",
  new_super: i18nMessages.m1f3a443bdac8636b,
  super_password: i18nMessages.ma351c6a1726c9a2a,
  password: i18nMessages.m39551af7e9aa6b80,
};

function supplementalInput(job) {
  const request = job.input;
  const allowed = request.method === "auth.login.continue"
    ? ["totp", "legacy_password", "account_secret", "source_secret", "secret_key", "new_super", "super_password", "merge_local", "host_credentials"]
    : request.method === "auth.key.continue"
      ? ["password", "super_password", "account_secret", "totp"] : [];
  const required = request.required_inputs;
  if (!Array.isArray(required) || !required.length || new Set(required).size !== required.length || required.some((field) => !allowed.includes(field)))
    throw Error(i18nMessages.m0041ba0cdaf49a89);
  return new Promise((resolve) => {
    let accepted = false;
    const element = dialog({
      title: i18nMessages.m5af2a2bfccfea455,
      body: `${i18nMessages.mcfec1e8dc530b14c}${h(request.user || "")}${request.addr ? " · " + h(request.addr) : ""}${i18nMessages.m28ea38d82d17f1bd}${h(job.transaction.operation_id)}</code></p>` + required.map((field) => {
        if (field === "host_credentials") return i18nMessages.m9daf9400f416c2e2;
        if (field === "merge_local") return select(field, i18nMessages.mea5cb9b4c5d099f5, [["", i18nMessages.mda590a8fe3ce4de0], ["true", i18nMessages.m06669a1ab0c76a98], ["false", i18nMessages.m0a555512d5148abf]], "");
        return input(field, supplementaryLabels[field], "", { type: "password", required: true });
      }).join(""),
      submit: i18nMessages.m729155cc21de362e,
      onSubmit: (values) => {
        if (required.includes("merge_local")) {
          if (!["true", "false"].includes(values.merge_local)) throw Error(i18nMessages.m57510a1361d976a6);
          values.merge_local = values.merge_local === "true";
        }
        accepted = true;
        resolve(values);
      },
    });
    element.addEventListener("close", () => { if (!accepted) resolve(null); }, { once: true });
  });
}

export async function continueAuthentication(id) {
  let job = await api("operation-recover", { id });
  for (;;) {
    if (job.state === "done") return job.materialPending
      ? { ...job.envelope, recoveryRequestId: id }
      : job.envelope;
    if (job.state !== "unconfirmed" || !job.input || !job.transaction) {
      const error = Error(job.error || i18nMessages.m0fbecd2c58a3f76d);
      error.requestId = id;
      error.operation = job;
      throw error;
    }
    const args = await supplementalInput(job);
    if (args === null) {
      const error = Error(i18nMessages.m086988e0cfc63fb3);
      error.requestId = id;
      throw error;
    }
    const transportId = "desktop-" + crypto.randomUUID();
    let submitted;
    try {
      submitted = await api("operation-continue", { id, transportId, args, confirmed: true });
    } catch (error) {
      error.requestId = id;
      throw error; // A lost response must be reconciled, never retransmitted.
    }
    try {
      return await waitForOperation(submitted);
    } catch (error) {
      if (error.operation?.state !== "unconfirmed" || !error.operation.input) throw error;
      job = error.operation;
    }
  }
}

export function authenticationPanel(root, ctx, refresh) {
  const section = document.createElement("section");
  section.className = "section-gap";
  section.innerHTML = panel(
    i18nMessages.me56b3b56c487fadc,
    i18nMessages.mbe4d15e061f8d6dc,
  );
  root.insertBefore(section, root.children[1]);
  const done = async (result, label) => {
    await ctx.refresh();
    await refresh();
    if (!recoveryMaterialDialog(result)) notify(label + i18nMessages.mf28461bb49c85647);
  };
  section.onclick = (event) => {
    const button = event.target.closest("[data-auth]");
    if (!button) return;
    const kind = button.dataset.auth;
    if (kind === "forget") {
      action(button, async () => {
        if (
          !(await confirm(
            i18nMessages.m83b27cb035aacdec,
            i18nMessages.mc8f60ede0404799f,
            i18nMessages.mffe81393b53ccdb5,
          ))
        )
          return;
        const result = await execute("logout_full");
        await ctx.refresh();
        await refresh();
        notify(result?.summary?.session_found ? i18nMessages.m49f83370c8c06600 : i18nMessages.mdeeedbb9031cf44e);
      });
      return;
    }
    if (kind === "login" || kind === "register") {
      if (ctx.desktop?.serverAddressError) {
        notify(ctx.desktop.serverAddressError, true);
        return;
      }
      const title = kind === "login" ? i18nMessages.m7bc96f0ded6264ed : i18nMessages.m223713a2813dd96d;
      dialog({
        title,
        body:
          input(
            "addr",
            i18nMessages.m2388cbc6a486af9f,
            ctx.desktop?.serverAddress || "",
            { required: true },
          ) +
          input("user", i18nMessages.m9c8a86c042690469, "", { required: true }) +
          input("password", i18nMessages.mfb5bbea8d049c01d, "", {
            type: "password",
            required: true,
          }) +
          (kind === "register"
            ? input("confirmation", i18nMessages.md0c9a0e81079a7b2, "", {
                type: "password",
                required: true,
              })
            : input("super", i18nMessages.mf154ba4a94e32fcb, "", {
                type: "password",
              }) + input("totp",i18nMessages.m3d4f12962a12d043,"",{type:"password"})),
        submit: title,
        onSubmit: async (values) => {
          validateAuthentication(kind, values);
          const result = await runAuthentication(kind, values);
          await done(result, title);
        },
      });
      return;
    }
    if (kind === "reset") {
      dialog({
        title: i18nMessages.m3f54af44006e9616,
        body:
          i18nMessages.m59a6af2abd9f3cb2 +
          input("super", i18nMessages.me958e8f3dd4968c7, "", {
            type: "password",
          }) +
          input("password", i18nMessages.m22436d1f48ae7ff0, "", {
            type: "password",
          }),
        submit: i18nMessages.m9c9f88e4444bc980,
        onSubmit: async (values) => {
          if (
            !(await confirm(
              i18nMessages.m3f54af44006e9616,
              i18nMessages.mfbaaa92acbbf40bc,
              i18nMessages.m916afcd85322d732,
            ))
          )
            return false;
          const result = await executeAuthentication(
            "auth_reset",
            {},
            {
              secret: JSON.stringify(
                { ...(values.super && { super: values.super }), ...(values.password && { password: values.password }) },
              ),
            },
          );
          await done(result, i18nMessages.mbd9438aefe6a5161);
        },
      });
      return;
    }
    dialog({
      title: i18nMessages.m291040f417ab14ce,
      wide: true,
      body:
        i18nMessages.m4d07b3b5167af5cb +
        input("addr", i18nMessages.m5fc148ba14963dba) +
        input("user", i18nMessages.m962cbddaf9a4c24a) +
        input("password", i18nMessages.m0256341c20fb23ad, "", {
          type: "password",
        }) +
        input("super", i18nMessages.ma351c6a1726c9a2a, "", { type: "password" }) +
        input("secret", i18nMessages.m9a8e8345e54c5c42, "", {
          type: "password",
        }) +
        input("kdf_salt", "kdf_salt", "", { required: true }) +
        input("wrapped_urk", "wrapped_urk", "", { required: true }) +
        input("urk_nonce", "urk_nonce", "", { required: true }),
      submit: i18nMessages.me7eb07075f22c45f,
      onSubmit: async (values) => {
        const { addr, user, ...credentials } = values;
        if (!credentials.super && !(credentials.password && credentials.secret))
          throw Error(i18nMessages.m06c0a14887e39712);
        if (
          !(await confirm(
            i18nMessages.me1be73b629e5527b,
            i18nMessages.mbefcf07229bc45b0,
            i18nMessages.me0534b8a4e46a0cb,
          ))
        )
          return false;
        const result = await executeAuthentication(
          "auth_recover",
          {
            ...(addr.trim() ? { addr: addr.trim() } : {}),
            ...(user.trim() ? { user: user.trim() } : {}),
          },
          { secret: JSON.stringify(credentials) },
        );
        await done(result, i18nMessages.m13951b6a5a5cd4db);
      },
    });
  };
}
