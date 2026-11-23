# ProTrack (`protrack4`) — Design Specification Suite

> **Status**: Baseline v1 Specification
> **Application**: ProTrack (`protrack4` Rust crate)
> **UI Framework**: [`gpui-kit`](https://github.com/longbridge/gpui-kit) (`0.7.x` — `gpui`, `gpui-base`, `gpui-component`)
> **Storage**: Local document-based `.protrack` SQLite files via `rusqlite` (`bundled`, FTS5, `STRICT` tables)

---

## Executive Summary

**ProTrack** is a fast, local-first personal project management desktop application built in Rust. It is designed for individual makers, engineers, and technical leads who want Linear-level speed and clarity without cloud accounts, user authentication, or remote servers—while still supporting lightweight local **Assignees** (for yourself, collaborators, contractors, or AI agents) and strict structural planning across projects.

### Core Product Bets

1. **Nested Features + Multi-Feature Work Items**: Features organize a Project hierarchically to arbitrary depth (`MAX_FEATURE_DEPTH = 32`), while concrete Work Items can belong to **0, 1, or many** Features inside the same Project.
2. **First-Class Unallocated Bucket**: Active Work Items attached to 0 Features sit in a prominent, pinned per-project **Unallocated** inbox—*"Things we need to do but I haven't figured out when yet"*—making triage effortless without forcing premature categorization.
3. **Lossless, Strictly Guarded Conversion**: Work Items and Features share a unified identity (`UUIDv7` + `PRO-12` key), statuses, priorities, tags, and assignee model. Leaf Features can convert into Work Items and unattached/single-feature Work Items can convert into Features under strict, transparent safety guards.
4. **Features-Only Dependency Graph**: Both Features and Work Items can be blocked on **Features** within the same Project. Blocking propagates transitively down Feature subtrees, handles multi-feature Work Items cleanly (Inherited vs. Partially Blocked), and strictly forbids cycles.
5. **Document-Based SQLite Workspaces**: Workspaces are portable `.protrack` SQLite files you can store anywhere on disk (including iCloud Drive or Dropbox folders), backed by `rusqlite` with FTS5 instant search, cascade Trash/Restore, and full session Undo/Redo (`Cmd+Z`).

---

## Specification Index

| Document | Scope & Contents |
| :--- | :--- |
| **[Product Specification](./PRODUCT_SPEC.md)** | Vision, domain glossary, entity fields, numbered invariants (`INV-01`–`INV-15`), Feature ↔ Work Item conversion matrix, Trash/Restore behavior, and v1 vs. post-v1 scope boundaries. |
| **[Data Model & SQLite Spec](./DATA_MODEL.md)** | Unified `items` schema design, ER diagram, complete executable `STRICT` SQLite DDL + triggers + FTS5 (`001_initial.sql`), canonical recursive CTEs, fractional indexing, and `.protrack` cloud-sync lifecycle. |
| **[Rust & GPUI Architecture](./ARCHITECTURE.md)** | Crate stack, `src/` module layout, core Rust domain types, single-writer `DbService` actor on GPUI's executor, reactive store invalidation, `UndoManager` command snapshots, and 4-layer test strategy. |
| **[UI & Interaction Spec](./UI_SPEC.md)** | `gpui-kit` component mapping, 3-pane desktop layout, ASCII wireframes for Outline, Filtered Table, Unallocated Triage, Trash, and Inspector Drawer, drag-and-drop matrix, and keyboard shortcuts. |
| **[Phased Implementation Plan](../IMPLEMENTATION_PLAN.md)** | UI-first, 12-phase vertical-slice implementation roadmap sized for one phase per agent session, with exact file deliverables and verification gates. |

---

## Locked Design Decisions (Quick Reference)

### Scope & Cardinality Matrix

| Entity | Scope | Cardinality / Ownership | Survives Conversion? |
| :--- | :--- | :--- | :--- |
| **Workspace** (`.protrack`) | Disk file | Contains `0..N` Projects + global Assignees | N/A |
| **Project** | Workspace | Strictly isolates Features, Work Items, Blockers, Tags | N/A |
| **Assignee** | Global (Workspace) | `0..1` optional single assignee per Feature or Work Item | Yes (100% preserved) |
| **Tag** | Per-Project | `0..N` per Feature or Work Item; unique case-insensitive name per Project | Yes (100% preserved) |
| **Feature** | Per-Project | `0..1` parent Feature (`MAX_FEATURE_DEPTH = 32`); `0..N` child Features & Work Items | Converts to Work Item iff `0` active children, `0` active WIs, `0` active dependents |
| **Work Item** | Per-Project | `0..N` attached Features (`0` = **Unallocated** bucket) | Converts to Feature iff attached to `0` or `1` active Feature |
| **Blocker Edge** | Per-Project | Source: Feature or Work Item; Target: **Feature only**; Soft visual block + DAG | Outgoing `blocked_by` edges preserved 100% |

### Shared Enums

- **Project Status**: `active`, `paused`, `completed`, `archived`
- **Item Kind**: `feature`, `work_item`
- **Item Status** (shared): `backlog`, `todo`, `in_progress`, `done`, `cancelled`
- **Priority** (shared): `none`, `low`, `medium`, `high`, `urgent`

---

## Phased Implementation Roadmap

> For the detailed, UI-first **12-phase single-session execution plan** (with exact file deliverables, out-of-scope boundaries, and verification criteria per phase), see **[IMPLEMENTATION_PLAN.md](../IMPLEMENTATION_PLAN.md)**. Below is the high-level milestone grouping:

### Milestone M0 — Workspace & App Shell Bootstrap
- Configure [Cargo.toml](../Cargo.toml) with `gpui-kit = "0.7"`, `rusqlite` (`bundled`), `uuid` (`v7`), `serde`, `serde_json`, `directories`, `thiserror`, `tracing`, `chrono`, and `fs4`.
- Implement `.protrack` workspace open/create/recent-files management (`preferences.json`), advisory file lock, WAL activation, and clean `wal_checkpoint(TRUNCATE)` -> `DELETE` journal teardown on exit.
- Implement schema migration runner (`001_initial.sql`) with pre-migration `.bak` copy and forward-version protection.
- Boot minimal `gpui-kit` desktop window with theme support (Light / Dark / System).

### Milestone M1 — Domain & Persistence Core
- Implement `DbService` single-writer actor and synchronous transaction repositories inside `rusqlite`.
- Implement all CRUD operations for Projects, Assignees, Tags, Features, Work Items, `feature_work_items`, and `item_blockers`.
- Implement base-62 fractional indexing (`sort_order`) with rebalance helpers.
- Implement recursive CTE queries: Feature tree, DAG cycle & ancestor-descendant validation (`INV-05`), transitive & partial effective blockers (`INV-06`), deduplicated subtree progress roll-up (`INV-11`).
- Implement strict Feature <-> Work Item conversion (`INV-08`, `INV-09`) and full-cascade Trash/Restore preserving shared Work Items (`INV-12`).
- Achieve 100% pass rate on SQLite invariant tests and randomized `proptest` DAG state-machine tests.

### Milestone M2 — 3-Pane Desktop Shell, Sidebar & Inspector Drawer
- Build the classic 3-pane shell using `gpui-kit` `Resizable`, `Sidebar`, and drawer panels.
- Left Sidebar: Workspace switcher, Project switcher + Create/Manage Project dialog, pinned **Unallocated** bucket with live count badge, collapsible nested Feature `Tree`, bottom links (**Trash**, **Assignees**, **Tags**, **Settings**).
- Right Inspector Drawer: Key copy badge (`PRO-12`), Kind badge + **Convert** action with live strict-guard disabled tooltips, inline Title editing, Status/Priority/Assignee/Tags pickers, Parent Feature or Multi-Feature attachment manager, Dependencies (Direct + Inherited + Dependents), and Markdown description editor/preview.

### Milestone M3 — Core Center Views
- **Hierarchical Outline / Tree View**: Nested Features and Work Items, subtree completion bars, direct/inherited/partial blocker indicators, `+N features` shared badges, inline child/item creation, completed/cancelled visibility toggle.
- **Dedicated Unallocated / Triage View**: Persistent top quick-capture bar, inline multi-feature popover, 1-click **Convert to Feature**, and drag-to-sidebar allocation.
- **Filtered Table / List View**: Virtualized `gpui-kit` `DataTable`, deduplicated project/subtree item list, filter pills (Kind, Status, Priority, Assignee, Tags, Blocker state: `All` / `Ready` / `Blocked`), and column sorting.
- **Trash View**: Grouped by `trash_root_id`, showing subtree summary counts, **Restore**, **Delete Permanently**, and **Empty Trash**.

### Milestone M4 — Drag-and-Drop, Quick Search, Quick Add & Undo/Redo
- Full drag-and-drop matrix: reorder siblings, reparent Features (with live cycle feedback), allocate/move Work Items (`Option`-drag to multi-attach), drag Unallocated items into Sidebar Features.
- `Cmd+K` Quick Search modal powered by FTS5 + key prefix lookup + workspace/project quick commands.
- `Cmd+N` Quick-Add modal defaulting to active context (selected Feature or Unallocated).
- Full `UndoManager` session command stack (`Cmd+Z` / `Cmd+Shift+Z`) covering edits, reparents, attachments, blockers, conversions, Trash/Restore, and reordering, with toast notifications.

### Milestone M5 — Hardening, Accessibility & Verification
- Cloud-sync stress testing (graceful lock error messaging, `SIGINT`/window-close WAL flush verification).
- Keyboard navigation & focus management polish, AccessKit labels via `gpui-kit`, and dark/light theme visual audit.
- End-to-end Undo/Redo round-trip test suite and `gpui-kit` headless UI tests.

### Post-v1 Roadmap (Explicitly Deferred)
- Kanban Board view (status-grouped columns for Project or Feature subtree).
- Visual Blocker DAG Graph view (interactive node/edge canvas highlighting critical paths).
- Cross-Project "My Work" / Workload Dashboard across all Projects in a workspace.
- Optional Target/Due Dates, T-shirt/Numeric Effort Estimates, and Item Activity/Decision Logs.
- JSON & Markdown tree export/import, plus optional local CLI/MCP server companion reading `.protrack` files.
