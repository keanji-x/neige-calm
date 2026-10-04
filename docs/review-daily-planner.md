# Daily Planner L2 review

The reviewed outcome is one hidden daily Track on the desktop homepage, with report-only Planner authority and report history reads. The final diff preserves existing phone navigation and released migrations.

## Channel A: complete source and call-path audit

- **Abstraction boundaries:** `daily_planner` owns calendar dates, reconciliation and the hidden Area choice. The generic Track factory owns cards/workspaces; `managed_track` owns declared creation metadata and authorization. Report persistence owns the task and closed-report write fences. The frontend app composes the ordinary Track body, while its feature owns date navigation and core owns API/date/link contracts.
- **Duplicate logic:** the existing System Area ensure, Track structure factory, registered Planner classifier, report reader and VCS text-patch formatter are reused. HTTP and MCP report changes call the same projection. Tool listing and calls share the same report-planning allowlist. The report URL builder stays beside its parser.
- **Hardcoded assumptions:** generic code never selects a daily identity, application or template by name. The daily owner key and default time zone live in the daily feature. Tool names, task attribution and User/System visibility are owning-layer protocol contracts; storage spelling is read from the owning enum/constant. Area names confer no privilege.

The audit covered boot/restart, transaction rollback, duplicate creation, metadata replay mismatch, source visibility, event ordering, pagination, response exports, all tool-entry paths, report writes, date navigation, legacy access and the complete generated diff. Findings fixed during implementation include premature mobile redirection (then removed from scope), an initially copied role spelling, invalid write arguments that made an authority assertion vacuous, and ambiguous replay tuple fields. Their replacements use declared owner contracts and production-entry assertions.

## Channel B: independent contract and adversarial verification

- **Abstraction boundaries:** production factories, HTTP router and actual tool registry handlers are exercised, not copies of lifecycle/read behavior. Dependency, ownership and style audits check the full frontend graph. Public responses are checked against the generated wire/OpenAPI contracts.
- **Duplicate logic:** red/green checks depend on production guards; they do not reimplement those guards in fixtures. Report changes use the same service from both transports, and pagination assertions compare complete observed memberships with independently prepared event fixtures.
- **Hardcoded assumptions:** an ungranted Planner, Worker, Assistant and mismatched binding are refused. A public template cannot grant workspace access. A display title cannot change the date identity. Typed role storage avoids an adapter guessing protocol spelling.

Two single-factor authority mutations were applied to production code in an exclusive, recoverable worktree. Removing the workspace read-grant predicate produced exactly `ungranted_planner_cannot_read_workspace_reports` in the selected red set. Removing the report-planning tool fence produced exactly `report_planning_profile_refuses_worker_terminal_lifecycle_and_plugin_writes`. Both sources were restored byte-for-byte and both tests returned green. No test or fixture was mutated.

Fresh checks after the CI contract sweep retained the same owner boundaries. The public template projection now lives in `TemplateRoster::user_entries`; admission still rejects kernel-only templates. The migration inventory includes the new migration without modifying a released migration. The report projection uses the repository transaction entry point, and closed-report authorization preserves the existing writer syntax boundary. All four new MCP contracts appear in role/registry expectations; their concise descriptions fit the unchanged Planner tool budget. Desktop route and accessibility checks use the new homepage navigation, while the legacy recovery check exercises its retained route.

The final targeted run passed 74 tests, plus the boot-time picker test. Both review channels were refreshed against these corrections: source/caller inspection found no unresolved boundary, duplicate-policy or application-identity issue; production-entry contract checks found no unresolved failure. Text gates passed. Compile/lint/OpenAPI preflight and remote integrated checks must finish green before marking the PR ready. No real Codex E2E is part of local verification.
