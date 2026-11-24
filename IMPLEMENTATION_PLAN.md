# ProTrack (`protrack4`) — Phased Implementation Plan

> **Companion Specs**: [Docs Index](./docs/README.md) · [Product Spec](./docs/PRODUCT_SPEC.md) · [Data Model & SQLite](./docs/DATA_MODEL.md) · [Rust & GPUI Architecture](./docs/ARCHITECTURE.md) · [UI & Interaction Spec](./docs/UI_SPEC.md)

---

## 1. Strategy Overview: UI-First Vertical Slices

This plan turns the ProTrack v1 design specification into **12 sequential, single-session implementation phases**. Each phase is scoped (~300–800 LoC across a narrow set of files) so a single coding agent session can read the spec, implement a complete user-visible slice, test it, and leave the repository green.

### Architectural Principles Behind This Ordering

1. **UI Up in Phase 1 — Zero Throwaway Code**:
   Phase 1 boots a genuine 3-pane `gpui-kit` desktop window populated via an in-memory `ProjectSnapshot` fixture using the real `src/domain/types.rs` structures. Phase 2 swaps the data source underneath `ProjectStore` to `rusqlite` + `DbService` without throwing away a single UI layout or domain type.
2. **Every Phase Ships Visible Product Value**:
   Rather than building all database repositories first and all UI last, Phases 3–11 pair each SQLite/domain slice directly with its `gpui-kit` views and controls so you can click, inspect, and test every capability immediately as it lands.
3. **Undo/Redo Designed In Early, Activated Cleanly Last**:
   Phase 3 introduces the thin `UndoableCommand` dispatch boundary (`execute(&mut self, tx)`) for every write operation. UI code dispatches commands from day one, allowing Phase 12 to enable `UndoManager`, inverse snapshots, and `Cmd+Z` / `Cmd+Shift+Z` without refactoring UI callbacks.
4. **Strict Spec Consistency**:
   Every enum (`backlog`/`todo`/`in_progress`/`done`/`cancelled`, `none`/`low`/`medium`/`high`/`urgent`, `active`/`paused`/`completed`/`archived`), invariant (`INV-01`–`INV-15`), `.protrack` lifecycle rule, and module boundary follows `docs/`.

---

## 2. Agent Session Protocol

When starting any phase in a fresh agent session, follow these 5 steps in order:

1. **Read Prerequisites**: Read the phase's **Required Reading** sections in `docs/` plus the files modified by immediately preceding phases.
2. **Verify Clean Baseline**: Run `cargo check && cargo test` (from Phase 1 onward) before making edits so baseline issues are caught early.
3. **Inspect `gpui-kit` `0.7` APIs**: Before calling unfamiliar `gpui-kit` component builders or methods, check the crate source in `~/.cargo/registry/src` (or `cargo doc --no-deps -p gpui-kit`) rather than guessing method names.
4. **Stay Within Scope**: Implement only the listed **Deliverables**. Leave items listed under **Explicitly Out of Scope** for their dedicated phases.
5. **Verify & Mark Progress**: Run every command in **Verification & Definition of Done**, verify the running GUI manually where applicable, and check off `[x]` in the Progress Tracker below.

---

## 3. Progress Tracker

| Done | Phase | Focus | Depends On | User-Visible Outcome |
| :---: | :--- | :--- | :---: | :--- |
| [x] | **Phase 1** | Bootable 3-Pane `gpui-kit` UI Shell (In-Memory Fixture) | — | Clickable themed 3-pane desktop window with sample project, tree, unallocated badge, and inspector |
| [x] | **Phase 2** | SQLite Persistence, Migrations & `.protrack` Lifecycle | Phase 1 | Real `.protrack` workspace open/create/switch, `preferences.json`, and async `DbService` snapshot loading |
| [x] | **Phase 3** | Projects, Nested Features & Editable Inspector | Phase 2 | Create/edit Projects and recursive Features (`PRO-N`), edit Markdown description + Preview, persisted to SQLite |
| [x] | **Phase 4** | Work Items, Multi-Feature Membership & Unallocated Triage | Phase 3 | Brain-dump Work Items into Unallocated, allocate to 0..N Features, unallocate, and inspect memberships |
| [x] | **Phase 5** | Global Local Assignees & Project-Scoped Tags | Phase 4 | Manage global Assignees and per-Project colored Tags; assign and render pills/chips across views |
| [x] | **Phase 6** | Hierarchical Outline View & Deduplicated Progress Roll-up | Phase 5 | Flagship nested Outline tree with deduplicated completion bars, `[Shared: +N]` popovers, and inline add |
| [x] | **Phase 7** | Features-Only Blocking Engine & Transitive Propagation | Phase 6 | Add/remove Feature blockers, enforce DAG acyclicity (`INV-05`), see Direct/Inherited/Partial badges & warnings |
| [x] | **Phase 8** | Strictly Guarded Feature ↔ Work Item Conversion | Phase 7 | Lossless bidirectional conversion preserving `PRO-N`, with live strict-guard tooltips when blocked |
| [x] | **Phase 9** | Filtered Virtualized Table / List View | Phase 8 | Deduplicated `DataTable` with multi-facet filter bar (`Ready` vs `Blocked`, Priority, Assignee, Tags) & sorting |
| [x] | **Phase 10** | Full-Cascade Trash, Restore & Permanent Purge | Phase 8 | Safe cascade deletion of Feature subtrees sparing shared Work Items, plus Trash View & Restore |
| [x] | **Phase 11** | Drag-and-Drop, Quick Search (`Cmd+K`) & Quick Add (`Cmd+N`) | Phase 9, 10 | Full DnD matrix (`Option`-drag to multi-attach), FTS5 `Cmd+K` search palette, and contextual `Cmd+N` modal |
| [ ] | **Phase 12** | Session Undo/Redo (`Cmd+Z`), Hardening & Polish | Phase 11 | Full session Undo/Redo, hardened cloud-sync WAL shutdown, AccessKit audit, and fuzz/round-trip suite |

---

## 4. Cross-Cutting Engineering Guardrails

- **Database Safety**: Always enable `PRAGMA foreign_keys = ON;` on every SQLite connection. Never bypass `INV-01`–`INV-15` triggers or domain checks.
- **UI Thread Hygiene**: From Phase 2 onward, never call `rusqlite` directly on GPUI's foreground render thread; route all reads and writes through `DbService` and GPUI's async executor (`cx.spawn`).
- **`gpui-kit` Bootstrap Pattern**: Initialize via `gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx| { gpui_kit::init(cx); ... })` so icons, theme tokens, and base component behaviors load cleanly.
- **Process Discipline**: Verify builds (`cargo check`, `cargo test`) cleanly without leaving orphaned background app instances running unless explicitly testing GUI launch.

---

## 5. Single-Session Phase Breakdown

---

### Phase 1: Bootable 3-Pane `gpui-kit` UI Shell (In-Memory Fixture)

#### Session Goal & User-Visible Outcome
Get a real, styled, clickable ProTrack window running on screen on day one. Users can toggle the Left Sidebar (`Cmd+B`) and Right Inspector (`Cmd+I`), switch Light/Dark/System theme, switch center view tabs (`Outline`, `Table`, `Unallocated`), expand/collapse a nested Feature tree, and click rows to inspect their details from an in-memory seed `ProjectSnapshot`.

#### Prerequisites & Required Reading
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§1–§3: Theme Tokens, `gpui-kit` Mapping, 3-Pane Shell)
- [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) (§2–§4: `Cargo.toml`, `src/` layout, Core Domain Types)

#### Deliverables & Exact Files
- `Cargo.toml` — Add `gpui-kit = "0.7"`, `uuid` (`v7`, `serde`), `chrono` (`serde`), `serde`, `serde_json`, `thiserror`, `tracing`, `tracing-subscriber`.
- `src/domain/mod.rs`, `src/domain/types.rs`, `src/domain/error.rs` — Core IDs (`ProjectId`, `ItemId`, `AssigneeId`, `TagId`), enums (`ProjectStatus`, `ItemKind`, `ItemStatus`, `Priority`), structs (`Project`, `Item`, `Assignee`, `Tag`, `ProjectSnapshot`), `MAX_FEATURE_DEPTH = 32`, and `DomainError`.
- `src/state/mod.rs`, `src/state/project_store.rs` — `ProjectStore` GPUI `Entity` initialized with a deterministic in-memory sample `ProjectSnapshot` (1 Project `PRO`, 4 nested Features up to depth 3, 2 unallocated Work Items, 3 allocated/shared Work Items, 2 sample Assignees, 2 Tags) + selection & theme state.
- `src/ui/mod.rs`, `src/ui/shell.rs`, `src/ui/sidebar.rs`, `src/ui/inspector.rs` —
  - 3-pane `h_resizable` shell + top toolbar (Workspace pill placeholder, Project dropdown stub, `[Outline | Table | Unallocated]` tab switcher, disabled `Cmd+K`/`+ New` buttons with Phase tooltips, Inspector toggle, Theme toggle).
  - Left Sidebar: Project display, pinned **Unallocated (2)** row, collapsible nested Feature tree with depth clamping past level 8 (`·L9+`), bottom `Trash (0)` / `Assignees` / `Tags` navigation placeholders.
  - Center Workspace: Styled view frames showing active tab header and clickable summary rows for the seed items.
  - Right Inspector Drawer: Read-only display of selected item's `item_key`, `kind` badge, `title`, `status`, `priority`, `assignee`, `tags`, and Markdown preview.
  - Keyboard actions: `Cmd+B` (toggle sidebar), `Cmd+I` (toggle inspector).
- `src/main.rs` — Initialize `tracing`, `gpui_kit::application().with_assets(gpui_kit::assets::Assets)`, `gpui_kit::init(cx)`, and open main window (`WorkspaceShell`).

#### Explicitly Out of Scope
- SQLite / `rusqlite`, `.protrack` file management, `DbService` actor, editing mutations, drag-and-drop, modals, and real CTEs.

#### Verification & Definition of Done
- `cargo check && cargo test` passes with zero warnings in app code.
- `cargo run` launches the `gpui-kit` desktop window cleanly; clicking Features/Work Items/Unallocated updates the Inspector; `Cmd+B` and `Cmd+I` toggle panels smoothly; Light/Dark theme switches cleanly.

---

### Phase 2: SQLite Persistence, Migrations & `.protrack` Workspace Lifecycle

#### Session Goal & User-Visible Outcome
Replace the Phase 1 hardcoded fixture with real portable `.protrack` SQLite workspace files managed off the UI thread. Launching ProTrack opens a Workspace Welcome modal (or auto-opens the last workspace / `--db <path>`), initializes schema `001_initial.sql`, optionally seeds a starter Project, and loads `ProjectSnapshot` via `DbService`.

#### Prerequisites & Required Reading
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§3 DDL `001_initial.sql`, §6 Workspace Lifecycle & Cloud-Sync Hardening)
- [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) (§5 `DbService` Actor, §7 Workspace Management & `preferences.json`)

#### Deliverables & Exact Files
- `Cargo.toml` — Add `rusqlite` (`bundled`, `chrono`, `uuid`), `directories`, `fs4`, `crossbeam-channel`, `oneshot`; `[dev-dependencies]` `tempfile`.
- `migrations/001_initial.sql` — Exact executable SQLite schema (`STRICT` tables, composite FKs, triggers, `items_fts` FTS5) from [docs/DATA_MODEL.md](./docs/DATA_MODEL.md).
- `src/db/mod.rs`, `src/db/migrations.rs`, `src/db/actor.rs` —
  - Schema version inspection (`workspace_meta` / `PRAGMA user_version`), forward-version guard (`file_version > APP_MAX_SCHEMA_VERSION`), pre-migration `.bak` copy, and embedded `001_initial.sql` runner.
  - `DbService` single-writer OS thread receiving typed `DbRequest`s and emitting `DomainEvent`s.
  - Basic read snapshot loader for active Project (`Project`, active `items`, `assignees`, `tags`, `feature_work_items`).
- `src/workspace/mod.rs`, `src/workspace/preferences.rs`, `src/workspace/manager.rs` —
  - `preferences.json` load/save via `directories::ProjectDirs` (`last_workspace_path`, `recent_workspaces`, `theme`, `sidebar_width`, `inspector_width`, `sidebar_collapsed`, `inspector_open`).
  - Create / Open / Switch / Close `.protrack` files with `fs4` advisory lock, session `PRAGMA journal_mode = WAL`, and clean `PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;` on close/switch.
- `src/state/app_state.rs` & update `src/state/project_store.rs` — Connect `ProjectStore` to `DbService` via `cx.spawn` + `DomainEvent` refresh; parse `--db <path>` in `src/main.rs`.
- `src/ui/modals/mod.rs`, `src/ui/modals/workspace_modal.rs` — Workspace Welcome / Switcher dialog (`Create New Workspace…`, `Open Existing .protrack…`, Recent Workspaces list, optional `"Seed Sample Project"` toggle on new workspace).

#### Explicitly Out of Scope
- Editing Projects/Items via UI, fractional indexing mutations, blocker CTEs, FTS5 search modal, and Undo/Redo.

#### Verification & Definition of Done
- `cargo test db:: workspace::` passes:
  - `001_initial.sql` creates all tables, triggers, FTS5 virtual table, and rejects cross-project FKs (`INV-01`).
  - Opening an existing v1 `.protrack` with bumped `schema_version = 999` triggers forward-version error cleanly without mutating the file.
  - Switching/closing a workspace leaves zero lingering `-wal` / `-shm` files on disk (`journal_mode = DELETE`).
- Manual GUI test: create a `.protrack` file with sample project, quit app, relaunch → auto-reopens last workspace and restores sidebar/inspector widths & theme.

---

### Phase 3: Core CRUD — Projects, Nested Features & Editable Inspector

#### Session Goal & User-Visible Outcome
Make Projects and nested Features truly editable and persisted. Users can create/rename/configure Projects (including `key_prefix` cascade rewrites), create root and nested sub-Features (up to `MAX_FEATURE_DEPTH = 32`) with auto-incremented `PRO-N` keys, reparent Features, and edit Title, Status, Priority, and Markdown Description (with live Preview) in the Right Inspector.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (§2.2–2.5, `INV-02`, `INV-10`, `INV-13`)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§4.1 Feature Tree CTE, §5 Fractional Indexing)
- [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) (§6 `UndoableCommand` trait)

#### Deliverables & Exact Files
- `src/domain/fractional_index.rs` — Base-62 lexicographic key generator (`first`, `after`, `before`, `between`, rebalance when length `> 32`).
- `src/commands/mod.rs`, `src/commands/meta_commands.rs`, `src/commands/item_commands.rs` — Introduce `UndoableCommand` trait (`label`, `execute(&mut self, tx)`); implement `CreateProject`, `UpdateProject` (with atomic `key_prefix` item rewrite), `CreateFeature`, `UpdateItemFields`, `ReparentFeature` (depth `<= 31` + non-descendant check), `ReorderSiblingFeatures`. (Note: `undo()` stubbed as `unimplemented!()` or prepared cleanly until Phase 12 wires `UndoManager`.)
- `src/db/project_repo.rs`, `src/db/item_repo.rs` — Project CRUD, atomic `next_item_seq` increment, Feature tree recursive CTE (`§4.1`), Feature insert/update/reparent.
- `src/ui/modals/managers.rs` — `New Project` and `Project Settings` modals (name, color, status, `key_prefix` validation `[A-Z][A-Z0-9]*` + key-rewrite confirmation).
- Update `src/ui/sidebar.rs` & `src/ui/inspector.rs` — Wire Project Switcher (`+ New Project`, `Project Settings`), Sidebar `+ Feature` / `+ Sub-Feature` actions, and live editable Inspector fields (Title input, Status & Priority `Select`s, Parent Feature selector, Markdown Description `Edit | Preview` toggle).

#### Explicitly Out of Scope
- Work Item creation/attachment, Unallocated Triage View, Assignees/Tags, Blockers, and Conversion.

#### Verification & Definition of Done
- `cargo test` passes unit tests for base-62 midpoint ordering (`a < m < b` across 200 sequential splits + rebalance trigger), `key_prefix` atomic rewrite (`OLD-1` → `NEW-1`), and `MAX_FEATURE_DEPTH = 32` rejection (`INV-02`).
- Manual GUI test: create Project, create 3 levels of nested Features, edit Markdown description, switch between Edit & Preview, restart app → all changes persist.

---

### Phase 4: Work Items, Multi-Feature Membership & Unallocated Triage View

#### Session Goal & User-Visible Outcome
Bring Work Items, multi-feature attachment, and the signature **Unallocated** inbox alive. Users can rapidly capture unallocated Work Items, attach any Work Item to `0`, `1`, or `many` Features, unallocate items back to the Unallocated bucket, and inspect/edit feature memberships in the Inspector.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (§2.4–2.6, `INV-03`)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§4.2 Unallocated Query)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§4.3 Unallocated Triage View, §5 Inspector Attached Features)

#### Deliverables & Exact Files
- Extend `src/db/item_repo.rs` — `create_work_item` (optionally attaching initial `feature_ids`), `set_work_item_features` (diffing `feature_work_items` rows with per-feature `sort_order`), Unallocated query (`INV-03`), active feature attachment queries.
- Extend `src/commands/item_commands.rs` — `CreateWorkItem`, `SetWorkItemFeatures`, `AttachWorkItemToFeature`, `DetachWorkItemFromFeature`.
- `src/ui/unallocated_view.rs` — Dedicated Unallocated Triage View:
  - Header with *"Things we need to do but I haven't figured out when yet"* subtitle and live count.
  - Persistent top quick-capture bar (`Enter` creates unallocated Work Item immediately and keeps input focused).
  - Row list displaying `item_key`, `title`, `priority`, `[+ Assign Features v]` searchable multi-select Feature tree popover, and disabled placeholder `[-> Feature]` tooltip ("Available in Phase 8").
- Update `src/ui/inspector.rs` & `src/ui/sidebar.rs` —
  - Inspector renders `Attached Features (N)` list with remove `[x]`, `[+ Attach to Feature...]` popover, and `[Unallocate]` button (when attached to `>= 1` Feature).
  - Sidebar displays accurate live `Unallocated (N)` badge and highlights amber when `N > 0`.

#### Explicitly Out of Scope
- Assignees, Tags, Outline Tree View, Blockers, Drag-and-Drop, and Conversion.

#### Verification & Definition of Done
- `cargo test` confirms:
  - Newly created unallocated Work Item appears in Unallocated query (`INV-03`).
  - Attaching it to 2 Features removes it from Unallocated and preserves independent `feature_work_items.sort_order` per Feature.
  - Detaching all Features returns it automatically to Unallocated.
- Manual GUI test: capture 3 items via `Enter` in Unallocated View, attach 1 item to two Features via popover, watch Unallocated badge decrement live, then click `[Unallocate]` in Inspector and see it reappear in Unallocated.

---

### Phase 5: Global Local Assignees & Project-Scoped Tags

#### Session Goal & User-Visible Outcome
Enable local Assignee management across the whole Workspace and colored Tag management strictly scoped per Project (`INV-01`, `INV-14`, `INV-15`). Assignees and Tags can be attached to both Features and Work Items and displayed across views.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (§2.7–2.8, `INV-14`, `INV-15`)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§5 Inspector, §6 Managers)

#### Deliverables & Exact Files
- `src/db/assignee_repo.rs` — Global Assignees CRUD, case-insensitive unique `handle` check, archive/unarchive (`archived_at`).
- `src/db/tag_repo.rs` — Per-Project Tags CRUD, case-insensitive unique `(project_id, name)` check, `set_item_tags`.
- Extend `src/commands/meta_commands.rs` & `src/commands/item_commands.rs` — `UpsertAssignee`, `ArchiveAssignee`, `UpsertTag`, `DeleteTag`, `SetItemTags`.
- Extend `src/ui/modals/managers.rs` — Global Assignees Manager modal (`name`, `handle`, `color`, `role`, `notes`, Archive toggle) & Project Tags Manager modal (`name`, `color`, Delete).
- Update `src/ui/sidebar.rs`, `src/ui/inspector.rs`, `src/ui/unallocated_view.rs` — Wire `Assignees` & `Tags` sidebar buttons; Inspector single-select Assignee picker (`0..1`, excludes archived except if currently assigned) & multi-select Tags chip editor; display Assignee initials pill and colored Tag chips on rows.

#### Explicitly Out of Scope
- Outline View, Blockers, Conversion, Table View, and Trash.

#### Verification & Definition of Done
- `cargo test` verifies:
  - Assignees created in Project A are immediately selectable in Project B (`INV-14`).
  - Tags created in Project A never appear in Project B, and SQLite composite FK rejects attaching Project A's tag to Project B's item (`INV-01`, `INV-15`).
  - Archiving an Assignee preserves it on existing items while hiding it from new-item pickers.
- Manual GUI test: create 2 Assignees and 3 Project Tags, assign them in Inspector, switch Projects and verify Assignees persist globally while Tags are isolated per Project.

---

### Phase 6: Hierarchical Outline View & Deduplicated Progress Roll-up

#### Session Goal & User-Visible Outcome
Build the flagship **Hierarchical Outline / Tree View** in the center workspace, showing nested Features and their Work Items, real-time deduplicated subtree completion progress bars (`INV-11`), `[Shared: +N features]` indicator popovers, inline item creation, and right-click context menus.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (`INV-10`, `INV-11`)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§4.5 Deduplicated Subtree Progress Roll-up CTE)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§4.1 Hierarchical Outline / Tree View)

#### Deliverables & Exact Files
- `src/domain/progress.rs` — `FeatureProgress` struct and formatting helpers (`done/total`, percentage string, empty fallback).
- Extend `src/db/item_repo.rs` — Execute the deduplicated subtree progress roll-up CTE (`§4.5`) into `ProjectSnapshot::progress_by_feature`; support Outline subtree scoping when a Feature is focused in the Sidebar.
- `src/ui/outline_view.rs` —
  - Renders nested Features + attached Work Items ordered by `sort_order`.
  - Feature rows display completion bar + `done/total (pct%)`, status, priority, assignee pill, and tags.
  - Work Items attached to `> 1` active Feature render under **each** attached Feature with a clickable `[Shared: +N features]` badge opening a quick-jump popover.
  - Inline `+ Add Work Item / Sub-Feature...` row at the bottom of expanded Features.
  - Toolbar controls: `[All Features | Selected Subtree]`, `[Hide Done/Cancelled]` toggle, `[Expand All | Collapse All]`.
  - Right-click `ContextMenu` (`Status →`, `Priority →`, `Assign →`, `Add Sub-Feature`, `Add Work Item`).
- Update `src/ui/shell.rs` — Mount `OutlineView` as the active center pane when `Outline` tab is selected.

#### Explicitly Out of Scope
- Blocker badges/dependencies, Feature ↔ Work Item conversion, Filtered Table View, Drag-and-Drop, and Trash.

#### Verification & Definition of Done
- `cargo test` verifies `INV-11`:
  - A Work Item shared across two sibling sub-features `F2a` and `F2b` under `F1` counts **once** (not twice!) in `F1`'s progress roll-up.
  - `cancelled` Work Items are excluded from denominators.
  - A Feature with 0 non-cancelled Work Items and 2 child Features (1 `done`, 1 `todo`) reports `50%` via child-Feature fallback; marking a Work Item `done` does **not** auto-complete its parent Feature (`INV-10`).
- Manual GUI test: explore Outline View, inline-add a Work Item and sub-Feature, inspect `[Shared: +1 feature]` popover, and watch parent progress percentages update live.

---

### Phase 7: Features-Only Blocking Engine & Transitive Propagation

#### Session Goal & User-Visible Outcome
Implement the complete Features-only dependency engine (`INV-04`–`INV-07`). Users can block Features or Work Items on Features in the same Project, see Direct / Inherited / Partially Blocked badges across Sidebar & Outline, receive a soft warning toast if moving a blocked item to `In Progress`, and have dependency cycles structurally prevented.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (`INV-04`, `INV-05`, `INV-06`, `INV-07`)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§4.3 Cycle Detection CTEs, §4.4 Effective Blocking CTE)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§1 Semantic Badges, §5 Inspector Dependencies)

#### Deliverables & Exact Files
- `src/domain/blockers.rs` — `EffectiveBlockState`, `BlockerWarning`, `BlockerRef` (with `via_ancestor` attribution).
- `src/db/blocker_repo.rs` —
  - Validation before `add_blocker`, `reparent_feature`, and `attach_work_item_to_feature` using `§4.3` combined DAG cycle + ancestor/descendant exclusion CTEs (`INV-05`).
  - Effective blocking + cancelled/trashed warning loader using `§4.4` CTE (`INV-06`, `INV-07`).
- `src/commands/blocker_commands.rs` — `AddBlocker`, `RemoveBlocker`; update `ReparentFeature` & `SetWorkItemFeatures` to enforce `INV-05` in-transaction.
- Update `src/state/project_store.rs` — Cache `effective_block_state` & `blocker_warnings` per active item in `ProjectSnapshot`.
- Update `src/ui/inspector.rs`, `src/ui/outline_view.rs`, `src/ui/sidebar.rs` —
  - Inspector `Dependencies` section: Direct Blockers list with remove `[x]`, searchable `[+ Add Blocker Feature...]` picker pre-filtering invalid cycle/ancestor targets (showing disabled reason if searched), read-only Inherited Blockers list (*"via Auth Core"*), Dependent items list (for Features), and cancelled/trashed blocker warning pills.
  - Display blocker pills in Outline & Sidebar; show non-blocking toast *"Warning: item is currently blocked by PRO-X"* when setting a blocked item to `in_progress`.

#### Explicitly Out of Scope
- Feature ↔ Work Item conversion, Filtered Table View, Trash, and Undo/Redo.

#### Verification & Definition of Done
- `cargo test` (including `proptest` DAG test):
  - Rejects Work Item as blocker (`INV-04`), direct ancestor/descendant block, multi-hop mixed hierarchy+blocker cycle (`F2 -> F1 -> F3 -> F2`), and `reparent_feature` or `attach_work_item_to_feature` that would violate `INV-05`.
  - Correctly distinguishes `DirectlyBlocked`, `InheritedBlocked`, `PartiallyBlocked` (multi-feature Work Item with 1 blocked & 1 unblocked Feature), and `Unblocked`, plus `Cancelled` warning without locking status changes.
- Manual GUI test: block parent Feature `A` on Feature `B`, verify child Feature & exclusive Work Items show `Inherited block`, verify shared Work Item shows `Partial (1/2)`, mark `B` `Done` and watch blocks clear live.

---

### Phase 8: Strictly Guarded Feature ↔ Work Item Conversion

#### Session Goal & User-Visible Outcome
Enable lossless bidirectional conversion between Work Items and Features (`INV-08`, `INV-09`). Users can convert unallocated or single-feature Work Items into Features, and empty leaf Features with zero dependents into Work Items—preserving UUID, `PRO-N` key, tags, assignee, description, and outgoing blockers—while invalid conversions are cleanly blocked with clear explanatory tooltips.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (§4 Conversion Matrix, `INV-08`, `INV-09`)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§3 `trg_items_kind_guard_upd`, §4.6 Atomic Conversion Transactions)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§5 Strict Conversion Guard Tooltips)

#### Deliverables & Exact Files
- `src/domain/conversion.rs` — `ConversionEligibility` and `ConversionBlockReason` computed cleanly for any active item from `ProjectSnapshot` and re-checked inside DB transactions.
- Extend `src/db/item_repo.rs` — Atomic `convert_work_item_to_feature` and `convert_feature_to_work_item` transactions matching `§4.6` (strict guard checks on active items, pruning any stale links to trashed rows *before* `UPDATE items SET kind = ...`, preserving outgoing blockers & tags & `item_key`).
- `src/commands/conversion_commands.rs` — `ConvertWorkItemToFeature` & `ConvertFeatureToWorkItem`.
- Update `src/ui/inspector.rs`, `src/ui/unallocated_view.rs`, `src/ui/outline_view.rs` —
  - Inspector `[Convert to Feature ->]` / `[Convert to Work Item ->]` button with live disabled state and `Tooltip` enumerating exact `ConversionBlockReason` counts.
  - Enable `[-> Feature]` button in Unallocated Triage View and `Convert to…` in Outline `ContextMenu`.

#### Explicitly Out of Scope
- Filtered Table View, Trash/Restore, Drag-and-Drop, and Undo/Redo.

#### Verification & Definition of Done
- `cargo test` covers **every single row** of the 9-case Conversion Matrix (`PRODUCT_SPEC.md` §4):
  - Unallocated WI → Root Feature (`parent = NULL`).
  - 1-Feature WI → Child Feature (`parent = P`) preserving `PRO-N`, tags, assignee, outgoing blockers.
  - 1-Feature WI at depth `31` → Rejected (`ExceedsMaxFeatureDepth`).
  - 2+-Feature WI → Rejected (`AttachedToMultipleFeatures`).
  - Leaf Root Feature → Unallocated Work Item.
  - Leaf Child Feature → Work Item attached to `[P]`.
  - Feature with active child Features, attached Work Items, or active dependents → Rejected with exact counts.
- Manual GUI test: hover disabled Convert buttons to verify exact tooltips, then convert an unallocated item to a Feature and back again preserving `PRO-N`.

---

### Phase 9: Filtered Virtualized Table / List View

#### Session Goal & User-Visible Outcome
Deliver the **Filtered Table / List View** using `gpui-kit`'s virtualized `Table` / `DataTable`, giving users a fast flat slice of their Project (or selected Feature subtree) with deduplicated rows, multi-facet filtering (`Ready to Work` vs `Blocked`, Kind, Status, Priority, Assignee, Tags), and sorting.

#### Prerequisites & Required Reading
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§2 `Table`/`DataTable`, §4.2 Filtered Table / List View)
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (`INV-06` Ready vs Blocked filtering semantics)

#### Deliverables & Exact Files
- `src/ui/table_view.rs` —
  - Deduplicated row projection (`TableFilterState`: text filter, `Kind`, `Status`, `Priority`, `Assignee`, `Tags`, `BlockFilter` = `All | ReadyToWork | BlockedOnly | HasWarnings`, optional active Sidebar Feature subtree scope).
  - Virtualized `gpui-kit` `DataTable` with columns: `Key`, `Kind`, `Title`, `Features` (comma-separated chips or `Unallocated`), `Status`, `Priority`, `Assignee`, `Block State`.
  - Sortable column headers (`Key`/`Number`, `Title`, `Status`, `Priority`, `Block State`).
  - Clicking a row selects it in `ProjectStore` and syncs the Right Inspector; right-click opens item `ContextMenu`.
- Update `src/ui/shell.rs` — Mount `TableView` when the `Table` tab is active.

#### Explicitly Out of Scope
- Trash/Restore, Drag-and-Drop, Quick Search modal (`Cmd+K`), and Undo/Redo.

#### Verification & Definition of Done
- `cargo test` tests filter & deduplication logic:
  - A Work Item attached to 3 Features appears **once** in Table View with 3 Feature badges.
  - `PartiallyBlocked` Work Items appear under `ReadyToWork` (`INV-06`).
  - `HasWarnings` surfaces items with cancelled blockers.
- Manual GUI test: filter 50+ items by `Ready to Work`, `Priority = High`, and `Assignee`, sort by column, click row → opens cleanly in Inspector.

---

### Phase 10: Full-Cascade Trash, Restore & Permanent Purge

#### Session Goal & User-Visible Outcome
Implement safe Trash & Restore across the app (`INV-12`). Moving a Feature to Trash cascades across its descendant Features and **exclusively owned** Work Items while keeping **shared** Work Items safe on their remaining active Features. Users can browse grouped Trash items, restore whole deleted trees atomically, or permanently purge them.

#### Prerequisites & Required Reading
- [docs/PRODUCT_SPEC.md](./docs/PRODUCT_SPEC.md) (§5 Trash & Restore Matrix, `INV-12`)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§4.7 Cascade Trash CTE)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§4.4 Trash View)

#### Deliverables & Exact Files
- Extend `src/db/item_repo.rs` —
  - `trash_item_subtree` using `§4.7` CTE (`deleted_at = now`, `trash_root_id = target_id`).
  - `list_trash_groups` returning root items grouped by `trash_root_id` with descendant Feature and Work Item counts.
  - `restore_trash_group` atomic transaction (clears `deleted_at`/`trash_root_id`, promotes root to `parent_feature_id = NULL` if original parent was purged or converted to Work Item, re-checks DAG safety if external blockers changed while trashed).
  - `purge_trash_group` and `empty_project_trash` (`DELETE FROM items ...` letting `ON DELETE CASCADE` clean up subtree + join rows safely).
- `src/commands/trash_commands.rs` — `TrashItemSubtree`, `RestoreTrashGroup`, `PurgeTrashGroup`, `EmptyTrash`.
- `src/ui/trash_view.rs` — Grouped Trash list showing key, title, deleted timestamp, subtree counts, `[Restore]`, `[Delete Forever]`, and `[Empty Trash]` with confirmation `Dialog`.
- Update `src/ui/sidebar.rs`, `src/ui/inspector.rs`, `src/ui/outline_view.rs`, `src/ui/table_view.rs` — Wire live `Trash (N)` sidebar count & view navigation, `[Move to Trash]` button in Inspector, and `Move to Trash` in row `ContextMenu`s.

#### Explicitly Out of Scope
- Drag-and-Drop, Quick Search (`Cmd+K`), Quick Add (`Cmd+N`), and session Undo/Redo (`Cmd+Z`).

#### Verification & Definition of Done
- `cargo test` thoroughly tests `INV-12`:
  - Trashing Feature `F1` (parent of `F2`, exclusive `W2`, shared `W1` also on `F3`) moves `F1, F2, W2` to Trash and leaves `W1` active on `F3`.
  - Restoring `F1` restores `F1, F2, W2` **and** restores `W1`'s link to `F2`.
  - Permanently purging `F1` deletes `F1, F2, W2` and the `(F2, W1)` link while `W1` survives intact on `F3`.
  - Trashing a blocker Feature clears active blocks on dependents with a `"Blocker in Trash"` warning (`INV-07`).
- Manual GUI test: delete a Feature subtree with a shared Work Item, verify shared Work Item stays active, inspect Trash View, click `[Restore]`, then verify permanent delete confirmation dialog.

---

### Phase 11: Drag-and-Drop, Quick Search (`Cmd+K`) & Quick Add (`Cmd+N`)

#### Session Goal & User-Visible Outcome
Make everyday triage effortless with full mouse Drag-and-Drop across Sidebar, Outline, and Unallocated Views, instant `Cmd+K` FTS5 Quick Search & Command modal, and contextual `Cmd+N` Quick-Add modal.

#### Prerequisites & Required Reading
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§6 Modals & Overlays, §7 Drag-and-Drop Matrix, §8 Shortcuts)
- [docs/DATA_MODEL.md](./docs/DATA_MODEL.md) (§4.8 FTS5 + Key Prefix Search Query)

#### Deliverables & Exact Files
- Drag-and-Drop wiring across `src/ui/sidebar.rs`, `src/ui/outline_view.rs`, `src/ui/unallocated_view.rs` implementing every row of `UI_SPEC.md` §7:
  - Feature sibling reorder (`sort_order`), Feature reparent onto Feature (with live cycle/depth rejection highlight) or Sidebar Root drop zone.
  - Work Item reorder inside a Feature (`feature_work_items.sort_order`) and inside Unallocated (`items.sort_order`).
  - Work Item move between Features (default drag) vs **multi-attach** (`Option`/`Alt`-drag keeps existing Feature attachment and adds target Feature).
  - Drag Unallocated row → Sidebar Feature (allocates immediately) and Feature-attached Work Item → pinned Sidebar **Unallocated** row (detaches).
- Extend `src/db/item_repo.rs` — `search_items_fts` (`§4.8`: BM25 over `items_fts` + exact/prefix `item_key` boost, scoped to active Project or all Projects).
- `src/ui/modals/quick_search.rs` — `Cmd+K` modal with `[Active Project | All Projects]` toggle, instant results, jump-to-item navigation, and quick actions (`Switch Project…`, `Open Workspace…`, `New Project…`, `Go to Unallocated`, `Toggle Theme`).
- `src/ui/modals/quick_add.rs` — `Cmd+N` contextual Quick-Add modal (`Work Item | Feature`, smart placement defaulting to selected Feature or Unallocated/Root, Status/Priority/Assignee/Tags/Description).
- Update `src/ui/shell.rs` — Activate top toolbar `[Search Cmd+K]` and `[+ New]` buttons and global `Cmd+K` / `Cmd+N` keybindings.

#### Explicitly Out of Scope
- Session Undo/Redo (`Cmd+Z` / `Cmd+Shift+Z`) — covered in Phase 12.

#### Verification & Definition of Done
- `cargo test` verifies FTS5 query ranking (`item_key` exact match ranks first, title/body stem search matches, trashed items are excluded) and `ReorderItems` / DnD move command invariants.
- Manual GUI test: reorder Features, `Option`-drag a Work Item onto a second Feature to verify multi-attachment, drag an Unallocated item onto a Sidebar Feature, search via `Cmd+K`, and create items via `Cmd+N`.

---

### Phase 12: Session Undo/Redo (`Cmd+Z`), Hardening & Polish

#### Session Goal & User-Visible Outcome
Complete ProTrack v1 by activating full session Undo/Redo (`Cmd+Z` / `Cmd+Shift+Z`) across every mutation, completing all global keyboard shortcuts, verifying cloud-sync shutdown hardening and AccessKit accessibility, and running the full end-to-end automated verification suite.

#### Prerequisites & Required Reading
- [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) (§6 `UndoManager` & Snapshot Strategy, §8 4-Layer Test Strategy)
- [docs/UI_SPEC.md](./docs/UI_SPEC.md) (§8 Complete Keyboard Shortcuts)

#### Deliverables & Exact Files
- `src/commands/undo_manager.rs` — `UndoManager` (`MAX_UNDO_DEPTH = 100`), `undo()` / `redo()` stack management, automatic stack clear on workspace switch, permanent purge, or Empty Trash.
- Complete `undo(&mut self, tx)` inverse-snapshot implementations across **all** commands in `src/commands/` (`CreateItem`/`WorkItem`/`Feature`, `UpdateItemFields`, `Reorder`, `ReparentFeature`, `SetWorkItemFeatures`, `AddBlocker`/`RemoveBlocker`, `ConvertWorkItemToFeature`, `ConvertFeatureToWorkItem`, `TrashItemSubtree`, `RestoreTrashGroup`, `UpsertTag`/`SetItemTags`, `UpsertAssignee`, `UpdateProject`).
- Update `src/db/actor.rs`, `src/state/app_state.rs`, `src/ui/shell.rs` — Wire `Cmd+Z` / `Cmd+Shift+Z` (only when focus is outside active text inputs so native text undo works inside editors!), toast feedback (`"Undid: Convert PRO-12 to Feature"`), plus remaining shortcuts (`Cmd+O` Open Workspace, `Cmd+1/2/3` Switch Views, `Esc` Close modal/deselect).
- Hardening & polish pass across `src/workspace/manager.rs` and `src/ui/`:
  - Graceful `SIGINT` / window-close flush guaranteeing `PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;`.
  - Clear advisory lock error modal when opening an already-locked `.protrack` file.
  - AccessKit labels & Dark/Light theme visual audit.
- Comprehensive test suite expansion (`tests/` or module tests):
  - Full Layer 2 Undo/Redo round-trip tests ($S_0 \xrightarrow{\text{execute}} S_1 \xrightarrow{\text{undo}} S_0 \xrightarrow{\text{redo}} S_1$) on every single command.
  - Layer 3 `proptest` state-machine fuzzer (500+ randomized operation sequences mixing hierarchy, blockers, multi-feature attachments, conversions, trash/restore, and undo/redo, asserting zero cycles, zero cross-project leaks, and exact invariant satisfaction after every step).
  - Layer 4 `gpui-kit` headless UI/store state integration tests.

#### Explicitly Out of Scope
- Post-v1 features (Kanban board, visual DAG graph canvas, cross-project dashboard, due dates, effort estimates, activity logs, export/CLI).

#### Verification & Definition of Done
- `cargo test --all-targets` passes 100% (SQLite invariants, Undo/Redo round-trips, `proptest` state machine, headless GPUI tests).
- `cargo clippy --all-targets -- -D warnings` passes cleanly.
- Manual end-to-end walkthrough of creating a workspace, building nested Features & shared Work Items, blocking, converting, trashing, restoring, undoing/redoing via `Cmd+Z` / `Cmd+Shift+Z`, and cleanly closing the app with zero `-wal`/`-shm` files left beside the `.protrack` file.
