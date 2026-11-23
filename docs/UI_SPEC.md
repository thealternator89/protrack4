# ProTrack — UI & Interaction Specification (`UI_SPEC.md`)

> **Companion Specs**: [README](./README.md) · [Product Specification](./PRODUCT_SPEC.md) · [Data Model & SQLite Spec](./DATA_MODEL.md) · [Rust & GPUI Architecture](./ARCHITECTURE.md)

---

## 1. Visual Design Philosophy & Theme Tokens

ProTrack follows a **calm, high-density desktop productivity aesthetic** inspired by Linear and Things 3:
- **Clarity over chrome**: Minimal borders, crisp typography, generous clickable targets, and subtle status/priority iconography.
- **Balanced desktop interaction**: Fast mouse, toolbar, context menu, and drag-and-drop workflows paired with clean desktop shortcuts (`Cmd+K`, `Cmd+N`, `Cmd+Z`)—intentionally avoiding complex single-key vim bindings.
- **Light / Dark / System Themes**: Built atop `gpui-kit` theme tokens (`cx.theme()`), respecting macOS/system appearance by default with an explicit toggle in Settings.

### Semantic Color & Iconography

| Concept | Values & Visual Treatment |
| :--- | :--- |
| **Item Kind** | **Feature**: Folder/Diamond outline icon + progress arc · **Work Item**: Checkbox/Circle status icon |
| **Status** | `backlog` (Dotted gray circle) · `todo` (Empty slate circle) · `in_progress` (Half-filled blue/amber ring) · `done` (Filled green check) · `cancelled` (Strikethrough gray circle) |
| **Priority** | `none` (`—` muted) · `low` (1 bar slate) · `medium` (2 bars amber) · `high` (3 bars orange) · `urgent` (Red exclamation badge) |
| **Blocking State** | **Direct Block**: Red shield/lock pill (`Blocked by 2`) · **Inherited Block**: Amber/soft-red pill (`Inherited block`) · **Partial Block**: Dashed amber pill (`1/2 features blocked`) · **Blocker Warning**: Amber (`Cancelled blocker`) or Muted (`In Trash`) |
| **Unallocated Bucket** | Warm amber inbox dot + live numeric pill badge whenever count `> 0` |

---

## 2. `gpui-kit` Component Mapping

| ProTrack UI Region | `gpui-kit` (`gpui_kit::component` / `gpui_kit::base`) Primitive | Notes |
| :--- | :--- | :--- |
| **App Window & 3-Pane Split** | `h_resizable` / `v_resizable` (`ResizableState`) + `TitleBar` | Persists `sidebar_width` & `inspector_width` to `preferences.json` |
| **Left Navigation Rail** | `Sidebar`, `SidebarGroup`, `SidebarMenu`, `SidebarMenuItem` | Collapsible (`Cmd+B`); houses Project switcher, Unallocated, Feature Tree, Trash |
| **Feature Tree (Sidebar)** | `Tree` / `TreeItem` + GPUI `on_drag` / `on_drop` | Indentation clamped visually after depth 8 with `L9+` depth badge |
| **Filtered Table View** | `Table` / `DataTable` (`TableDelegate`) | Virtualized rows, sortable column headers, fixed key/title columns |
| **Filter Controls & Pickers** | `Select`, `Popover`, `Button`, `Tag` / `Badge` | Multi-select popovers for Tags and Work Item → Features attachment |
| **Inspector Description** | `InputState` (multiline) + `TextView::markdown` (`Markdown`) | Toggleable `Edit` / `Preview` (or split view on wide inspector) |
| **Modals & Dialogs** | `Dialog` / `Modal` | `Cmd+K` Quick Search, `Cmd+N` Quick Add, Project/Assignee/Tag managers |
| **Feedback & Safety** | `Tooltip`, `ContextMenu`, `Notification` / `Toast` | Disabled Convert tooltips, soft-block warning toasts, Undo confirmation |

---

## 3. Classic 3-Pane Application Shell

```text
+---------------------------------------------------------------------------------------------------+
| [protrack4.protrack v] | Project: [ProTrack v1 v] | [Outline] [Table] [Unallocated (4)] | [Search Cmd+K] [+ New] [Inspector] |
+------------------------+---------------------------------------------------------+----------------+
| SIDEBAR (260px)        | MAIN CENTER WORKSPACE                                   | INSPECTOR      |
|                        |                                                         | (380px)        |
| [Project: ProTrack v1] |  (Switches between:                                   |                |
|                        |   1. Hierarchical Outline / Tree View                 | PRO-12  [Copy] |
| * Unallocated      (4) |   2. Filtered Table / List View                       | [Work Item]    |
|                        |   3. Dedicated Unallocated / Triage View              | [Convert -> F] |
| FEATURES               |   4. Trash View)                                      |                |
| v Core Data       62%  |                                                         | Title: ...     |
|   > SQLite Sync   100% |                                                         | Status / Prio  |
|   v DAG Engine    25%  |                                                         | Assignee / Tag |
|     o Cycle Check      |                                                         | Features / Dep |
| > UI Shell        10%  |                                                         | Description MD |
|                        |                                                         |                |
| ---------------------- |                                                         |                |
| Trash (3)              |                                                         |                |
| Assignees              |                                                         |                |
| Tags                   |                                                         |                |
| Settings & Theme       |                                                         |                |
+------------------------+---------------------------------------------------------+----------------+
```

### Sidebar Rules
- **Project Switcher**: Displays active Project color dot, `key_prefix`, and name; clicking opens dropdown listing active/paused/completed/archived Projects plus `+ New Project…` and `Project Settings…`.
- **Pinned Unallocated Row**: Always pinned directly beneath the Project switcher so unallocated work is never hidden inside a folder. Acts as a live drop target (dropping a single-feature Work Item here unallocates it!).
- **Nested Feature Tree**: Shows each Feature's expand chevron, status dot, title, tiny progress pill/percentage, and subtle red/amber dot if blocked. Every Feature row acts as a drop target for reordering, reparenting Features, or allocating Work Items.
- **Depth Clamping**: Data supports `MAX_FEATURE_DEPTH = 32`, while left padding caps visually at depth 8 (`8 * 14px`) with a subtle `·L11` pill so deep trees remain readable.

---

## 4. Center Workspace Views (v1)

### 4.1 Hierarchical Outline / Tree View
Scoped either to the whole Project (`All Features`) or to a selected Feature subtree in the Sidebar.

```text
ProTrack v1 › Outline                              [Filter: Hide Done/Cancelled v] [Expand All]
------------------------------------------------------------------------------------------------
v [F] PRO-1  Core Data & SQLite                            [=====>---] 5/8 (63%)   [MB]  (Todo)
  v [F] PRO-2  Schema & Migrations                         [========] 3/3 (100%)   [MB]  (Done)
      [W] PRO-3  Write 001_initial.sql                     [bug] [db]              [MB]  (Done)
      [W] PRO-4  Add FTS5 sync triggers                    [db]                    [MB]  (Done)
  v [F] PRO-5  DAG Blocker Engine        [Blocked: PRO-2]  [==>-----] 1/4 (25%)    [AI]  (In Prog)
      [W] PRO-6  Implement cycle CTE     [! Inherited]     [Shared: +1 feature]    [AI]  (Todo)
      [W] PRO-7  Write proptest suite    [! Inherited]                             [MB]  (Backlog)
    + Add Work Item or Sub-Feature to PRO-5...
------------------------------------------------------------------------------------------------
```

- **Shared Work Item Indicator**: When Work Item `PRO-6` is attached to both `PRO-5` and `PRO-9`, it renders under **both** Features in the Outline with a clickable `[Shared: +1 feature]` badge (clicking shows a quick popover listing all attached Features).
- **Inline Creation**: Every expanded Feature ends with a subtle `+ Add Work Item or Sub-Feature...` row.
- **Context Menu**: Right-click any row for `Status →`, `Priority →`, `Assign →`, `Add Sub-Feature`, `Add Work Item`, `Convert to Feature / Work Item`, `Duplicate`, `Move to Trash`.

### 4.2 Filtered Table / List View
Flat, deduplicated virtualized `DataTable` across the active Project (or selected Feature subtree).

```text
ProTrack v1 › Table                                                     42 items (showing 18)
[Search filter...] [Kind: All v] [Status: Active v] [Priority: All v] [Assignee: All v] [Block: Ready v] [Tag: All v]
------------------------------------------------------------------------------------------------
Key     Kind      Title                        Features              Status       Prio   Block
------------------------------------------------------------------------------------------------
PRO-12  WorkItem  Add OAuth refresh hook       Auth, CLI Sync        Todo         High   Partial (1/2)
PRO-14  WorkItem  Benchmark FTS5 query speed   Unallocated           In Progress  Med    Ready
PRO-5   Feature   DAG Blocker Engine           Core Data > …         In Progress  High   Direct (1)
------------------------------------------------------------------------------------------------
```

- **Deduplication**: Unlike Outline, every Work Item appears **exactly once** in Table View, with its `Features` column showing comma-separated Feature chips (or `Unallocated`).
- **Blocker Filter Modes**: `All`, `Ready to Work` (includes `Unblocked` and `PartiallyBlocked`), `Blocked Only` (`DirectlyBlocked` + `InheritedBlocked`), `Has Blocker Warnings` (cancelled/trashed blockers).

### 4.3 Dedicated Unallocated / Triage View
Prominent inbox dedicated to active Work Items attached to `0` Features.

```text
Unallocated — "Things we need to do but I haven't figured out when yet"            4 items
+---------------------------------------------------------------------------------------------+
| + Capture unallocated work item... (Press Enter to add, Cmd+Enter to add & assign features) |
+---------------------------------------------------------------------------------------------+
Key     Title                              Prio   Assignee  Assign Features        Convert
---------------------------------------------------------------------------------------------
PRO-14  Benchmark FTS5 query speed         Med    [MB]      [+ Assign Features v]  [-> Feature]
PRO-15  Design app icon                    None   [Unass.]  [+ Assign Features v]  [-> Feature]
PRO-18  Investigate iCloud conflict copy   High   [MB]      [+ Assign Features v]  [-> Feature]
```

- **Fast Triage**:
  1. Top input bar stays focused across `Enter` submissions for brain-dumping multiple items rapidly.
  2. Clicking `[+ Assign Features v]` opens a searchable multi-select Feature tree popover; checking `>= 1` Feature allocates the item out of Unallocated (with an Undo toast).
  3. Dragging any row directly onto a Feature in the left Sidebar allocates it there immediately.
  4. Clicking `[-> Feature]` converts the unallocated Work Item directly into a root Feature (`INV-08`).

### 4.4 Trash View
Accessed via `Trash (N)` at the bottom of the Left Sidebar.
- Displays grouped Trash entries by `trash_root_id`, e.g.:
  `PRO-4 Auth Overhaul · Deleted 2 hours ago · Includes 3 sub-features & 7 work items` `[Restore]` `[Delete Forever]`
- Header bar includes `[Empty Trash]` button guarded by a confirmation `Dialog`.

---

## 5. Right Inspector Drawer

Collapsible (`Cmd+I`), showing full editable details for the currently selected Feature, Work Item, or Project.

```text
+------------------------------------------+
| PRO-6  [Copy Key]        [Work Item]     |
| [Convert to Feature ->]                  |  <-- Disabled w/ tooltip if 2+ features or non-leaf Feature
+------------------------------------------+
| Title:                                   |
| [Implement cycle CTE                   ] |
+------------------------------------------+
| Status:   [Todo        v]                |
| Priority: [High        v]                |
| Assignee: [AI - Claude v]                |
| Tags:     [db x] [core x] [+ Add Tag]    |
+------------------------------------------+
| ATTACHED FEATURES (2)                    |  <-- For Work Item (or Parent Feature picker for Feature)
| • Core Data > DAG Engine       [x]       |
| • Core Data > Migrations       [x]       |
| [+ Attach to Feature...] [Unallocate]    |
+------------------------------------------+
| DEPENDENCIES                             |
| Direct Blockers:                         |
|   • PRO-2 Schema & Migrations  [Done] [x]|
|   [+ Add Blocker Feature...]             |  <-- Picker filters out invalid cycle/ancestor targets
| Inherited Blockers (via Parent):         |
|   • PRO-9 Auth Core (via DAG Engine)     |  <-- Read-only with link to jump to parent
| (If Feature) Blocking Dependents:        |
|   • PRO-11 UI Tree View                  |
+------------------------------------------+
| DESCRIPTION             [Edit | Preview] |
| Markdown editor / gpui-kit Markdown view |
+------------------------------------------+
| Created Nov 23, 2026 · Updated 2m ago    |
|                        [Move to Trash]   |
+------------------------------------------+
```

### Strict Conversion Guard Tooltips (`INV-08` & `INV-09`)
When `[Convert to Feature]` or `[Convert to Work Item]` is disabled, hovering displays the exact reasons from `ConversionBlockReason`:
- *"Cannot convert to Work Item: has 2 child Features and 3 attached Work Items."*
- *"Cannot convert to Work Item: 1 item (PRO-11) is blocked on this Feature."*
- *"Cannot convert to Feature: attached to 2 Features (DAG Engine, Migrations). Remove extra Features until 0 or 1 remains."*

---

## 6. Modals & Overlays

1. **`Cmd+K` Quick Search & Command Modal**:
   - Instant search across active Project (with toggle pill `[Active Project | All Projects]`) combining `item_key` exact/prefix match and `items_fts` BM25 search.
   - Also surfaces quick commands (`Switch Project…`, `Open Workspace…`, `New Project…`, `Go to Unallocated`, `Toggle Theme`).
2. **`Cmd+N` Quick-Add Modal**:
   - Segmented `[Work Item | Feature]` selector.
   - Contextual smart default: if a Feature is currently selected/viewed, defaults placement to that Feature; if in Unallocated view or project root, defaults to **Unallocated** (for Work Item) or **Root** (for Feature).
   - Optional fields inline: Status, Priority, Assignee, Tags, Description.
3. **Workspace Welcome & Switcher Dialog**:
   - Create new `.protrack` file (native OS save dialog), open existing `.protrack` file (native OS open dialog), or click from Recent Workspaces.
4. **Managers (Modal Dialogs)**:
   - **Project Settings**: Rename Project, edit `key_prefix` (shows confirmation warning that existing keys e.g. `OLD-12` will update to `NEW-12`), status (`active`/`paused`/`completed`/`archived`), color.
   - **Global Assignees Manager**: Create, edit (`name`, `handle`, `color`, `role`, `notes`), archive/unarchive local assignees.
   - **Project Tags Manager**: Create, recolor, rename, or delete tags scoped to active Project.

---

## 7. Drag-and-Drop Interaction Matrix

| Drag Source | Drop Target | Default Behavior | Modifier Behavior (`Option / Alt`) |
| :--- | :--- | :--- | :--- |
| **Feature** | Sibling gap under same parent | Reorder sibling Feature (`sort_order`) | — |
| **Feature** | Another Feature row (Sidebar/Outline) | Reparent as child of target Feature (rejected w/ red highlight if creates cycle or depth `>= 32`) | — |
| **Feature** | Root drop zone (Sidebar top) | Promote to root Feature (`parent_feature_id = NULL`) | — |
| **Work Item** | Gap inside same Feature | Reorder Work Item inside that Feature (`feature_work_items.sort_order`) | — |
| **Work Item (in Feature `A`)** | Feature `B` (Sidebar/Outline) | **Move**: detach from `A`, attach to `B` | **Also Attach**: keep `A` and add `B` (multi-feature membership!) |
| **Work Item (in Feature `A`)** | Pinned **Unallocated** row (Sidebar) | Detach from `A` (or all Features if confirmed) → enters Unallocated | — |
| **Work Item (Unallocated)** | Any Feature `F` (Sidebar/Outline) | Attach to `F` (leaves Unallocated) | — |
| **Work Item (Unallocated)** | Gap inside Unallocated View | Reorder within Unallocated (`items.sort_order`) | — |

---

## 8. Keyboard Shortcuts (Balanced Desktop Set)

| Shortcut | Scope | Action |
| :--- | :--- | :--- |
| `Cmd+K` (`Ctrl+K`) | Global | Open Quick Search & Command Modal |
| `Cmd+N` (`Ctrl+N`) | Global | Open Contextual Quick-Add Modal |
| `Cmd+Z` (`Ctrl+Z`) | Global (outside text input) | Undo last database command |
| `Cmd+Shift+Z` (`Ctrl+Shift+Z`) | Global (outside text input) | Redo last undone command |
| `Cmd+O` (`Ctrl+O`) | Global | Open `.protrack` Workspace file |
| `Cmd+B` (`Ctrl+B`) | Global | Toggle Left Sidebar |
| `Cmd+I` (`Ctrl+I`) | Global | Toggle Right Inspector Drawer |
| `Cmd+1` / `Cmd+2` / `Cmd+3` | Workspace | Switch to Outline / Table / Unallocated view |
| `Enter` | Unallocated quick input | Create Work Item and keep focus in input |
| `Esc` | Modal / Popover / Selection | Close active overlay or clear row selection |
