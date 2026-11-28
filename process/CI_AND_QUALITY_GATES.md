# CI and Regression Gates

Run local checks from the `risense-ai/respire` worktree root.

---

## 1. Engineering Practices

| Practice | Implementation |
| --- | --- |
| Spec approval and card dependencies | `openspec/` and `<!-- respire-card -->` Issues with `depends_on` |
| Dispatch and review | `node process/scripts/respire-process.mjs` and [role guides](roles/README.md) |
| Independent verdict and merge evidence | `verdict-gate`, evidence registration, and the `.github/agents.yml` allowlist |
| Worktree isolation and path-filtered CI | `../.worktrees/<card>/respire`, `detect-changes`, and `ci-gate` |
| PR evidence and UI preview | [PR template](templates/PULL_REQUEST_TEMPLATE.md), `prototypes/mock-api/`, and UI preview artifacts |
| Three-strike regression policy | [LESSONS.md](../LESSONS.md), `guardrail` Issues, and route, CLI, and i18n parity tests |

---

## 2. Validation Gates

```sh
node process/scripts/validate-openspec.mjs
node scripts/check-guardrails.mjs
node --test process/scripts/respire-process.test.mjs process/scripts/lessons-audit.test.mjs
node --test prototypes/mock-api/*.test.mjs
node test-cases/runner.test.mjs
```

The OpenSpec validator checks governance files, relative links, line limits,
and the OpenAPI baseline. Monorepo guardrails enforce dependency boundaries,
build isolation, and product code rules. CI verifies affected components,
black-box E2E cases, UI previews, and release artifacts as applicable.

A PR that completes a card must put one directive such as `Closes #5` or `Fixes risense-ai/respire#6` on the first body line, followed by a blank line. Use `Part of #5` for prerequisite PRs. Review can associate a PR with a card name or branch name. Automated completion requires the single closing directive.

## 3. Anti-Flake & Anti-Regression Discipline

1. **No skipped/relaxed assertions**: Failing tests MUST NOT be muted or relaxed to make CI green; real environment gaps require explicit `UNTESTED` reporting by the Verifier.
2. **Route & CLI parity guard**: Runtime unit tests verify registered HTTP routes match `openapi.yaml` 1:1; CLI tests verify capability negotiation rather than version checks.
3. **Commit & PR hygiene**: `git show --stat HEAD` MUST contain only files belonging to the claimed card—never carry edits from another worktree or session.
