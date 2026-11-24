# ProTrack4

*A calm, fast, local-first desktop project manager for solo builders, engineers, and technical leads.*

**ProTrack4** is a native desktop application built in Rust (`gpui-kit`) that stores your entire planning workspace in portable, single-file `.protrack` SQLite databases. There are no cloud accounts, no user authentication, and no remote servers—just instant offline speed, clear structural planning, and data you own completely on disk.

---

## Why ProTrack4?

Most project tools force an awkward compromise:

- **Flat to-do lists** feel fast on day one, then lose architectural structure as soon as a codebase or product grows.
- **Team sprint and board trackers** bury individual builders under ceremonies, rigid cycles, and multi-tenant cloud overhead.

Solo builders and technical leads plan differently:

- You break large ideas down into **nested Features** recursively as clarity emerges.
- You capture concrete **Work Items**—some clearly tied to one Feature, some spanning **multiple Features** at once, and many sitting in an **Unallocated** pile until you decide where they fit.
- You track real readiness through **Feature dependencies** (*"We can't build this until Auth ships"*) rather than arbitrary sprint dates.
- You reshape plans fluidly: a quick task grows into a full Feature, or an over-scoped Feature simplifies back down to a single task.

ProTrack4 makes those exact mental operations native, lossless, and instantaneous.

---

## Core Capabilities & How They Help You

### Nested Features with Deduplicated Progress Roll-Ups

Model your product, architecture, or milestones naturally with **recursively nested Features** (up to 32 levels deep).

- **Automatic progress roll-up**: Every Feature displays its live subtree completion percentage across all nested Work Items. If a shared Work Item belongs to two sibling sub-features under the same parent, it is counted **only once**—so your progress bar never inflates.
- **Manual Feature completion**: Features and Work Items share the same five statuses (`Backlog`, `Todo`, `In Progress`, `Done`, `Cancelled`). Completing all child tasks never auto-flips a Feature to `Done`; you decide explicitly when a Feature is ready and ships, which in turn unblocks dependent work.

### Multi-Feature Work Items

Real engineering tasks rarely respect neat single-folder silos. A task like *"Add OAuth token refresh"* might be required by both *Mobile Sync* and *CLI Auth*.

- Attach any Work Item to **0, 1, or multiple Features** within its Project.
- Maintain **independent ordering** for that Work Item inside each Feature it belongs to, while keeping a single source of truth for its status, priority, assignee, tags, and Markdown description.

### First-Class Unallocated Inbox & Triage

Any active Work Item attached to zero Features automatically lives in your Project's pinned **Unallocated** bucket (*"Things we need to do but I haven't figured out when yet"*), complete with a live item counter badge in the sidebar.

- Capture ideas rapidly without being forced to categorize them upfront.
- Triage when you're ready: assign one or more Features via a searchable popover, drag rows directly onto Features in the sidebar, or promote an item into a root Feature in one click.

### Lossless Feature ↔ Work Item Conversion

Plans evolve. Because Features and Work Items share a unified identity—preserving their immutable key (such as `PRO-12`), title, Markdown description, status, priority, assignee, tags, and outgoing blockers—converting between them is **100% lossless**:

- **Work Item → Feature**: Allowed whenever a Work Item is Unallocated (becomes a root Feature) or attached to a single Feature (becomes a child Feature).
- **Feature → Work Item**: Allowed whenever a Feature has no active child Features, no active attached Work Items, and no active items blocked on it.
- **Transparent safety guards**: When a conversion isn't allowed yet, ProTrack4 disables the action and explains the exact reason in a tooltip (for example, listing the attached Features or dependents to resolve first).

### Feature-Level Dependency Blocking (Soft & Cycle-Safe)

Keep track of what is actually ready to build without rigid process lock-ins:

- **Features-only blockers**: Any Feature or Work Item can be marked as blocked by one or more **Features** in the same Project.
- **Transitive & partial awareness**: Blocking propagates automatically down Feature subtrees and distinguishes between **Directly Blocked**, **Inherited Blocked**, and **Partially Blocked** (when a multi-feature Work Item has one blocked path and one clear path, so it stays visible as ready to work).
- **Cycle prevention**: ProTrack4 prevents dependency loops across both parent-child hierarchies and blocker links.
- **Soft visual warnings**: Blocked items display clear badges and show a gentle toast warning if moved to `In Progress`, without ever locking you out of updating status. Cancelled or trashed blockers also surface non-blocking review notices.

### Local Workspaces, Strict Project Isolation & Lightweight Assignees

- **Portable `.protrack` workspaces**: Create, open, and switch between multiple workspace files stored anywhere on disk—including **iCloud Drive, Dropbox, or Google Drive** folders—protected by advisory file locking and clean single-file flushing on exit.
- **Strictly isolated Projects**: Keep multiple Projects inside one workspace (`PRO`, `HOME2`, etc.), each with its own immutable key sequence, accent color, status (`Active`, `Paused`, `Completed`, `Archived`), and project-scoped colored tags.
- **Local Assignees without accounts**: Define workspace-wide assignees for yourself, collaborators, contractors, or AI agents with custom handles, roles, and colors—and archive past assignees without losing attribution on historical items.

---

## Built for Everyday Desktop Flow

ProTrack4 uses a calm, high-density **3-pane desktop layout** with **Light**, **Dark**, and **System** themes:

- **Left Sidebar**: Workspace and Project switchers, pinned **Unallocated** inbox, collapsible nested Feature tree with progress pills and drop targets, plus quick links to **Trash**, **Assignees**, **Tags**, and **Settings**.
- **Main Workspace Views**:
  - **Hierarchical Outline (`Cmd+1` / `Ctrl+1`)**: Browse nested Features and Work Items with completion bars, blocker badges, `+N features` shared-item indicators, and inline child creation.
  - **Filtered Table (`Cmd+2` / `Ctrl+2`)**: Scan a flat, deduplicated, virtualized table across your Project or selected subtree, with sortable columns and filter pills for kind, status, priority, assignee, tags, and blocker readiness (*All*, *Ready to Work*, *Blocked Only*, *Blocker Warnings*).
  - **Unallocated Triage (`Cmd+3` / `Ctrl+3`)**: Rapidly capture tasks (`Enter` keeps focus in the quick-capture bar) and allocate or promote them in seconds.
  - **Safe Trash & Atomic Restore**: Trashing a Feature groups the Feature, its sub-features, and its exclusively owned Work Items into one recoverable group—while automatically protecting **shared Work Items** that are still attached to other active Features.
- **Right Inspector Drawer (`Cmd+I` / `Ctrl+I`)**: One-click key copy, lossless Convert button, inline title/status/priority/assignee/tag editing, Feature membership manager, direct and inherited dependencies, and a Markdown description editor with live preview.

### Fluid Interactions & Keyboard Shortcuts

- **Drag-and-Drop**: Reorder sibling Features and Work Items, reparent Features in the sidebar or outline (with live cycle feedback), drop Unallocated items onto Features to allocate them, or hold `Option` / `Alt` while dragging a Work Item to attach it to an additional Feature.
- **Full Session Undo/Redo**: Revert edits, reordering, reparents, feature attachments, blockers, conversions, and Trash actions cleanly with `Cmd+Z` / `Cmd+Shift+Z` (`Ctrl+Z` / `Ctrl+Shift+Z`).

| Shortcut | Action |
| :--- | :--- |
| `Cmd+K` / `Ctrl+K` | Quick Search (instant SQLite FTS5 + key lookup) & command palette |
| `Cmd+N` / `Ctrl+N` | Contextual Quick-Add modal (defaults to active Feature or Unallocated) |
| `Cmd+1` / `Cmd+2` / `Cmd+3` (`Ctrl+1`/`2`/`3`) | Switch between Outline, Table, and Unallocated views |
| `Cmd+B` / `Ctrl+B` | Toggle Left Sidebar |
| `Cmd+I` / `Ctrl+I` | Toggle Right Inspector Drawer |
| `Cmd+O` / `Ctrl+O` | Open a `.protrack` workspace file |
| `Cmd+Z` / `Cmd+Shift+Z` (`Ctrl+Z` / `Ctrl+Shift+Z`) | Undo / Redo last workspace action |
| `Esc` | Close active overlay or clear selection |

---

## Who ProTrack4 Is For

- **Solo founders and indie builders** who want deep structural clarity across multiple projects without paying for—or maintaining—cloud project management SaaS.
- **Staff engineers and technical leads** breaking down complex architectures where tasks cut across multiple features and readiness depends on upstream deliverables.
- **Builders collaborating with contractors or AI coding agents** who want lightweight assignee tracking (`MB`, `AI`, etc.) and self-contained project files that live right alongside their documents.

For detailed specifications, see [Product Spec](docs/PRODUCT_SPEC.md), [UI & Interaction Spec](docs/UI_SPEC.md), [Data Model Spec](docs/DATA_MODEL.md), and [Architecture](docs/ARCHITECTURE.md).

---

## Requirements & Setup

### Prerequisites

- **Rust toolchain** supporting **Edition 2024** (`rustc 1.85+`, installed via [rustup](https://rustup.rs))
- **Platform build tools**: macOS Xcode Command Line Tools, or Windows `x86_64` MSVC build tools (SQLite with FTS5 is bundled automatically—no separate database install needed)

### Build & Run

```bash
git clone https://github.com/thealternator89/protrack4.git
cd protrack4
cargo run --release
```

### Useful Commands

```bash
# Open a specific .protrack workspace directly
cargo run -- --db path/to/workspace.protrack

# Run the test suite (SQLite invariants, Undo/Redo round-trips, proptest DAG fuzzing, UI smoke tests)
cargo test
```

Windows release binaries and NSIS installers (`ProTrack4-Setup.exe`) are also built via [`.github/workflows/build-windows.yml`](.github/workflows/build-windows.yml).
