# ProTrack — Product Specification (`PRODUCT_SPEC.md`)

> **Companion Specs**: [README](./README.md) · [Data Model & SQLite Spec](./DATA_MODEL.md) · [Rust & GPUI Architecture](./ARCHITECTURE.md) · [UI & Interaction Spec](./UI_SPEC.md)

---

## 1. Product Vision & Target User

**ProTrack** is a calm, fast, personal desktop project management application written in Rust with `gpui-kit` and backed by portable local SQLite files.

Most project trackers either force flat to-do lists (losing architectural structure) or rigid sprint/board ceremonies designed for large cloud teams. Solo builders and technical leads plan differently:
- You break big ideas into **nested Features** recursively as clarity emerges.
- You capture concrete tasks as **Work Items**—some clearly belonging to one Feature, some spanning **multiple Features** (e.g., *"Add OAuth token refresh"* serving both *Mobile Sync* and *CLI Auth*), and many sitting in an **Unallocated** pile (*"Things we need to do but I haven't figured out when yet"*).
- You think about readiness through **Feature dependencies**: *"Work Item X can't start until Feature Y ships."*
- Sometimes a small Work Item grows into a full Feature, or an over-scoped Feature simplifies down to a single Work Item.

ProTrack makes these exact mental operations native, lossless, and instantaneous—offline, without accounts, stored in single-file `.protrack` SQLite databases you own completely.

---

## 2. Domain Glossary & Entity Specifications

### 2.1 Workspace File (`.protrack`)
A self-contained SQLite database file on disk (e.g., `~/Documents/Workspaces/personal.protrack` or inside iCloud Drive / Dropbox).
- Users can create, open, and switch between multiple `.protrack` files.
- The app stores recent workspace paths and UI preferences locally in OS app-data (`preferences.json`), remembering the last-opened workspace on launch.
- A workspace contains **Projects** and **Global Assignees**.

### 2.2 Project
A strictly isolated top-level container inside a Workspace. Features, Work Items, Blocker edges, and Tags never cross Project boundaries.

| Field | Type | Default / Constraint | Notes |
| :--- | :--- | :--- | :--- |
| `id` | UUIDv7 | Required, PK | Generated at creation |
| `name` | String | Required, non-empty trimmed | Display name (e.g., `"ProTrack v1"`) |
| `key_prefix` | String | Required, unique in workspace | `2–10` chars matching `[A-Z][A-Z0-9]*` (e.g., `"PRO"`, `"HOME2"`) |
| `next_item_seq` | Integer | Starts at `1`, monotonically increasing | Generates immutable `item_key` (`PRO-1`, `PRO-2`, …) |
| `description` | Markdown | `""` | Project summary/notes |
| `color` | Hex Color | `"#6366F1"` | Visual badge accent |
| `status` | Enum | `'active'` | `'active' \| 'paused' \| 'completed' \| 'archived'` |
| `sort_order` | Fractional String | Required | Sidebar project ordering |
| `created_at` / `updated_at` | RFC 3339 UTC | Required | Auto-maintained |

### 2.3 Feature
A hierarchical capability, milestone, or deliverable container inside a single Project. Features can nest recursively (`parent_feature_id` nullable for root Features).

### 2.4 Work Item
An actionable task or unit of execution inside a single Project. A Work Item attaches to **`0`, `1`, or many** Features inside its Project.

### 2.5 Unified Item Fields (Features & Work Items)
Features and Work Items share a single identity and field schema (`kind: 'feature' | 'work_item'`), guaranteeing lossless conversion in both directions.

| Field | Type | Default / Constraint | Notes |
| :--- | :--- | :--- | :--- |
| `id` | UUIDv7 | Required, PK | Preserved across Feature ↔ Work Item conversion |
| `project_id` | UUIDv7 | Required, FK → `projects` | Immutable after creation |
| `item_number` | Integer | Unique per `project_id` | Assigned from `projects.next_item_seq` |
| `item_key` | String | Unique per `project_id` | Formatted `{key_prefix}-{item_number}` (e.g., `PRO-12`) |
| `kind` | Enum | `'feature' \| 'work_item'` | Determines role & allowed relations |
| `parent_feature_id` | UUIDv7? | Nullable; only valid when `kind = 'feature'` | `NULL` = root Feature; always `NULL` for Work Items |
| `title` | String | Required, non-empty trimmed | Single-line summary |
| `description` | Markdown | `""` | Rich Markdown body rendered via `gpui-kit` |
| `status` | Enum | `'todo'` | `'backlog' \| 'todo' \| 'in_progress' \| 'done' \| 'cancelled'` |
| `priority` | Enum | `'none'` | `'none' \| 'low' \| 'medium' \| 'high' \| 'urgent'` |
| `assignee_id` | UUIDv7? | Nullable, FK → `assignees` | Single optional local assignee |
| `sort_order` | Fractional String | Required | Order among sibling Features (if Feature) or inside Unallocated bucket (if Work Item) |
| `deleted_at` | RFC 3339 UTC? | `NULL` when active | Populated when moved to Trash |
| `trash_root_id` | UUIDv7? | `NULL` when active; non-null when trashed | Points to the root Item of the Trash action for atomic Restore |
| `created_at` / `updated_at` | RFC 3339 UTC | Required | Auto-maintained |

> **Per-Feature Work Item Ordering**: When a Work Item is attached to Feature `F`, its position inside `F` is stored on the `feature_work_items` link (`sort_order`) rather than on `items.sort_order`. That allows a shared Work Item to sit at position #1 inside Feature `A` and position #5 inside Feature `B` independently, while `items.sort_order` preserves its position in the Unallocated bucket.

### 2.6 Unallocated Bucket
Not a separate database table or fake Feature, but a **first-class computed view** per Project:
> **Definition**: Every active (`deleted_at IS NULL`) Work Item in Project `P` that has **zero active attached Features** sits in `P`'s **Unallocated** bucket (*"Things we need to do but I haven't figured out when yet"*).

- Pinned permanently at the top of Project `P`'s left sidebar with a live item counter badge.
- Also accessible as a dedicated Triage center view for rapid capture, multi-feature assignment, drag-to-Feature allocation, or conversion into new root Features.

### 2.7 Local Assignee (Global to Workspace)
Represents a local person, role, or agent—never tied to accounts or auth. Shared across all Projects in a `.protrack` file.

| Field | Type | Default / Constraint | Notes |
| :--- | :--- | :--- | :--- |
| `id` | UUIDv7 | Required, PK | |
| `name` | String | Required, non-empty trimmed | e.g., `"Mark Benson"`, `"Claude Agent"`, `"Contractor"` |
| `handle` | String | Required, unique (case-insensitive), `1–8` chars | Short pill / initials, e.g., `"MB"`, `"AI"`, `"JS"` |
| `color` | Hex Color | `"#64748B"` | Avatar/pill color |
| `role` | String? | Optional | e.g., `"Founder"`, `"Backend"`, `"QA"` |
| `notes` | String | `""` | Freeform local notes |
| `archived_at` | RFC 3339 UTC? | `NULL` when active | Archived assignees stay visible on historical items, hidden from new-assignment pickers |
| `created_at` / `updated_at` | RFC 3339 UTC | Required | |

### 2.8 Project Tag (Per-Project)
Colored labels scoped strictly to a single Project for cross-cutting categorization (e.g., `bug`, `tech-debt`, `research`).

| Field | Type | Default / Constraint | Notes |
| :--- | :--- | :--- | :--- |
| `id` | UUIDv7 | Required, PK | |
| `project_id` | UUIDv7 | Required, FK → `projects` | Strictly isolated to one Project |
| `name` | String | Required, unique case-insensitively per Project | e.g., `"bug"`, `"tech-debt"` |
| `color` | Hex Color | `"#8B5CF6"` | Tag chip color |
| `created_at` | RFC 3339 UTC | Required | |

### 2.9 Blocker Dependency (`item_blockers`)
A directed edge `(blocked_item_id, blocker_feature_id)` within a single Project meaning:
*"Item `blocked_item_id` (Feature or Work Item) is blocked until Feature `blocker_feature_id` reaches `done`."*

---

## 3. Core Invariants (`INV-01` – `INV-15`)

Every layer of ProTrack (SQLite schema/triggers, Rust domain logic, Undo commands, and UI controls) MUST uphold these 15 invariants at all times:

- **`INV-01` — Strict Project Isolation**: Every Feature, Work Item, Tag, `feature_work_items` link, `item_tags` link, and `item_blockers` link belongs to exactly one `project_id`. Cross-project parent links, feature attachments, tags, or blockers are impossible at both the database and domain layers. Only `assignees` are shared globally across Projects.
- **`INV-02` — Recursive Feature Nesting with Depth Cap**: Features form a forest of rooted trees per Project (`parent_feature_id` → Feature in same Project). Nesting is effectively unbounded via recursive CTEs with a generous safety limit of **`MAX_FEATURE_DEPTH = 32`** (root Features are depth `0`, maximum allowed child depth is `31`). Attempting to create, move, or convert a Feature at depth `>= 32` is rejected cleanly.
- **`INV-03` — Multi-Feature Membership & Unallocated Rule**: A Work Item may attach to `0..N` active Features in its Project (at most once per Feature). Any active Work Item with `0` active attached Features belongs automatically to its Project's **Unallocated** bucket.
- **`INV-04` — Features-Only Blockers**: Only active or existing **Features** (`kind = 'feature'`) may be referenced as `blocker_feature_id`. Work Items can *be blocked* on Features, but Work Items can *never* block anything.
- **`INV-05` — Combined Graph Acyclicity (DAG) & Ancestor/Descendant Exclusion**: Within each Project:
  1. Let directed graph $G = (V, E)$ have vertices = Features and edges $E = \{ (c \to p) \mid c.\text{parent\_feature\_id} = p \} \cup \{ (b \to r) \mid (b, r) \in \text{item\_blockers} \}$. **$G$ MUST remain acyclic (a DAG)** across all blocker additions, Feature reparents, and Work Item → Feature conversions.
  2. **Direct Ancestor/Descendant Exclusion**: A Feature $F$ cannot be directly blocked on any ancestor or descendant of $F$. A Work Item $W$ cannot be directly blocked on any Feature $F'$ that $W$ is attached to, nor any ancestor of such $F'$. (Checked on `add_blocker`, `attach_work_item_to_feature`, and `reparent_feature`.)
- **`INV-06` — Transitive & Partial Blocking Semantics (Soft Visual Blocking)**:
  - Blocking is **soft/advisory**: blocked items display clear visual badges, filter status, and a subtle non-blocking toast warning if moved to `in_progress`, but status changes are never hard-locked.
  - Let `is_active_blocker(B)` be `B.deleted_at IS NULL AND B.status NOT IN ('done', 'cancelled')`.
  - **For a Feature `F`**: `F` is **Directly Blocked** if any `(F, B)` satisfies `is_active_blocker(B)`, and **Inherited Blocked** if any strict ancestor `A` of `F` is Directly Blocked (even if `F` has no direct blockers).
  - **For a Work Item `W`**:
    - If `W` has `>= 1` active direct blocker $\implies$ **`DirectlyBlocked`**.
    - Else if `W` is Unallocated (`0` active attached Features) $\implies$ **`Unblocked`**.
    - Else (`W` is attached to active Features $AF(W) \neq \emptyset$ with no active direct blockers):
      - If **all** $F \in AF(W)$ are Blocked $\implies$ **`InheritedBlocked`**.
      - If **some** ($1 \le k < |AF(W)|$) $F \in AF(W)$ are Blocked $\implies$ **`PartiallyBlocked`** (treated as *Ready/Unblocked* in global Ready filters because at least one parent Feature path is clear, while displaying a warning badge in blocked Feature contexts and the Inspector).
      - If **none** of $F \in AF(W)$ are Blocked $\implies$ **`Unblocked`**.
- **`INV-07` — Blocker Resolution Status Rules**:
  - When blocker Feature `B` moves to `done`, all active blocks caused by `B` are satisfied (`resolved`). The `item_blockers` link remains as historical record (and reactivates if `B` is reopened).
  - When blocker Feature `B` moves to `cancelled`, `B` stops actively blocking dependents, and dependents surface a non-blocking amber warning: *"Blocker `PRO-X` was cancelled — review or remove"*.
  - When blocker Feature `B` is moved to Trash (`deleted_at IS NOT NULL`), `B` stops actively blocking active dependents, and active dependents surface a muted warning: *"Blocker `PRO-X` is in Trash"*.
- **`INV-08` — Strict Work Item → Feature Conversion**: A Work Item `W` may convert to a Feature **strictly iff** `W` is attached to **`0` or `1` active Feature** (and if attached to `1` active Feature `P`, `depth(P) + 1 < MAX_FEATURE_DEPTH`). Converting a Work Item attached to `2+` active Features is disabled until the user removes extra feature attachments down to `0` or `1`. Any stale links to trashed Features are pruned cleanly in the conversion transaction (and captured in Undo).
- **`INV-09` — Strict Feature → Work Item Conversion**: A Feature `F` may convert to a Work Item **strictly iff** `F` has:
  1. `0` active child Features (`deleted_at IS NULL AND parent_feature_id = F.id`), **AND**
  2. `0` active attached Work Items (`feature_work_items` where `wi.deleted_at IS NULL`), **AND**
  3. `0` active dependents (`item_blockers` where `blocked.deleted_at IS NULL AND blocker_feature_id = F.id`).
  Stale references from already-trashed items are pruned cleanly inside the conversion transaction (and captured in Undo).
- **`INV-10` — Shared Statuses & Manual Feature Completion**: Features and Work Items use the exact same 5-value `status` enum (`backlog`, `todo`, `in_progress`, `done`, `cancelled`). Feature status is **always set manually** by the user (never auto-flipped when children complete), because completing a Feature explicitly signals readiness and unblocks dependent items.
- **`INV-11` — Deduplicated Subtree Progress Roll-up**: A Feature's displayed completion percentage counts **distinct active Work Items** across the Feature and all its active descendant Features:
  - Denominator = distinct active Work Items in subtree with `status != 'cancelled'`.
  - Numerator = distinct active Work Items in subtree with `status = 'done'`.
  - A Work Item attached to two sibling sub-features under `F` is counted **only once** in `F`'s roll-up.
  - **Empty-subtree fallback**: If a Feature's active subtree has `0` non-cancelled Work Items but has `>= 1` non-cancelled active descendant Features, progress falls back to `(done descendant Features) / (non-cancelled descendant Features)`. If both are `0`, progress shows `—` (`0/0`).
- **`INV-12` — Full-Cascade Trash with Shared Work Item Protection**:
  - Deleting a single **Work Item** moves only that Work Item to Trash (`deleted_at = now`, `trash_root_id = W.id`), preserving its `feature_work_items` rows for Restore while hiding it from active views.
  - Deleting a **Feature `F`** computes the active descendant Feature subtree $S(F) = \{F\} \cup \text{active descendants of } F$, and sets `deleted_at = now`, `trash_root_id = F.id` on:
    1. Every Feature in $S(F)$, **AND**
    2. Every active Work Item $W$ attached to $\ge 1$ Feature in $S(F)$ that has **zero** active attached Features *outside* $S(F)$ (exclusively owned by $S(F)$).
  - **Shared Work Item Protection**: Any Work Item $W$ attached to at least one active Feature *outside* $S(F)$ remains **active** (`deleted_at IS NULL`) and stays attached to its remaining active Features! Its `feature_work_items` link(s) inside $S(F)$ remain inert/hidden (`f.deleted_at IS NULL` filter) so restoring `F` restores the association, while permanently purging `F` cascades only the join row without deleting $W$.
- **`INV-13` — Immutable Human-Readable Item Keys**: Every created Feature or Work Item receives `{project.key_prefix}-{item_number}` using a strictly monotonic counter (`next_item_seq`). Deleted/purged numbers are never reused, and `item_key` stays identical across Feature ↔ Work Item conversions. (If a Project's `key_prefix` is renamed in Project Settings, existing `item_key`s in that Project are batch-rewritten atomically in the same transaction.)
- **`INV-14` — Single Optional Global Assignee**: Every Feature and Work Item has `assignee_id: Option<AssigneeId>` pointing to a workspace `assignees` row (`ON DELETE SET NULL`). `assignee_id` is preserved losslessly across conversions.
- **`INV-15` — Project-Scoped Unique Tags**: Tags belong to a single `project_id`, have unique case-insensitive names per Project, and attach via `item_tags` (`0..N` per item). Tag attachments are preserved losslessly across conversions.

---

## 4. Feature ↔ Work Item Conversion Matrix

Because Features and Work Items share `id`, `item_key`, `title`, `description`, `status`, `priority`, `assignee_id`, `tags`, and outgoing `blocked_by` feature links, every allowed conversion is **100% lossless**.

| Source Kind | Current Relations (Active Items) | Allowed? | Resulting State After Conversion |
| :--- | :--- | :---: | :--- |
| **Work Item** | `0` Features (**Unallocated**) | **Yes** | Root **Feature** (`parent_feature_id = NULL`), preserves `sort_order` at root level (or moved to end of root features). |
| **Work Item** | `1` Feature `P` (where `depth(P) + 1 < 32`) | **Yes** | Child **Feature** (`parent_feature_id = P`), `feature_work_items(P, W)` removed; takes last/converted position under `P`. |
| **Work Item** | `1` Feature `P` (where `depth(P) = 31`) | **No** | Blocked: `"Cannot convert: parent Feature is already at max nesting depth (32)."` |
| **Work Item** | `2+` Features (`[A, B, ...]`) | **No** | Blocked: `"Attached to 2 Features (Auth, Billing). Remove extra Features until 0 or 1 remains before converting."` |
| **Feature** | `0` child Features, `0` WIs, `0` dependents, `parent = NULL` | **Yes** | **Work Item** in **Unallocated** (`parent_feature_id = NULL`, `0` entries in `feature_work_items`). |
| **Feature** | `0` child Features, `0` WIs, `0` dependents, `parent = P` | **Yes** | **Work Item** attached to **`[P]`** (`parent_feature_id = NULL`, 1 row `(P, F)` in `feature_work_items`). |
| **Feature** | `>= 1` active child Feature | **No** | Blocked: `"Cannot convert: has N child Feature(s). Move or delete child Features first."` |
| **Feature** | `>= 1` active attached Work Item | **No** | Blocked: `"Cannot convert: has N attached Work Item(s). Move or unallocate them first."` |
| **Feature** | `>= 1` active item blocked on it | **No** | Blocked: `"Cannot convert: N item(s) are blocked on this Feature. Remove those blocker links first."` |

> **Note on Outgoing Blockers**: In every allowed conversion above, any outgoing blockers (`blocked_item_id = X` in `item_blockers`) remain intact—because both Features and Work Items can be blocked on Features (`INV-04`). Wait: could a Work Item `W` have a blocker edge that becomes invalid when `W` becomes a Feature? By `INV-05`, `W` already cannot be blocked on `P` or any ancestor of `P`, and `W` had no children yet, so `W → F` cannot introduce a cycle or ancestor/descendant violation!

---

## 5. Trash & Restore Behavioral Matrix

| Action | Database Effect (`deleted_at`, `trash_root_id`, Links) | Active Views Effect |
| :--- | :--- | :--- |
| **Trash Work Item `W`** | `W.deleted_at = now`, `W.trash_root_id = W.id`. `feature_work_items`, `item_tags`, `item_blockers` stay intact. | `W` disappears from Outline/Table/Unallocated; appears as 1 entry in Trash. |
| **Trash Feature `F`** | Compute subtree `S(F)` + exclusive WIs `E(F)`. Set `deleted_at = now`, `trash_root_id = F.id` on all items in `S(F) ∪ E(F)`. Shared WIs attached outside `S(F)` are **untouched**. | `S(F)` and `E(F)` vanish from active views; Trash shows 1 grouped card for `F` (`+X sub-features, +Y work items`). Shared WIs stay visible on their remaining active Features. Active items blocked on `F` unblock with muted `"Blocker in Trash"` notice. |
| **Restore Trash Group `R`** | Inside transaction: clear `deleted_at = NULL`, `trash_root_id = NULL` on all rows where `trash_root_id = R.id`. If `R` is a Feature whose `parent_feature_id` no longer exists (or was permanently purged / isn't a Feature anymore), set `R.parent_feature_id = NULL` (restores at root). Also check DAG cycle guard if any external blockers changed while trashed (warn/prune stale conflicting blocker if needed). | Entire subtree + exclusive WIs + surviving `feature_work_items` links (including shared WI associations back to `S(F)`) reappear in place! |
| **Permanent Purge `R`** | `DELETE FROM items WHERE id = R.id`. Foreign keys `ON DELETE CASCADE` delete all rows with `trash_root_id = R.id`, plus their `feature_work_items`, `item_tags`, and `item_blockers` rows. | Permanently erased; shared WIs remain completely safe on their remaining active Features. `UndoManager` clears entries referencing purged IDs (or clears stack). |
| **Empty Trash** | `DELETE FROM items WHERE deleted_at IS NOT NULL AND project_id = :pid`. | All trashed items in active Project permanently erased; `UndoManager` cleared. |

---

## 6. v1 Scope vs. Post-v1 Non-Goals

| Capability | v1 (Included) | Post-v1 (Explicitly Deferred) |
| :--- | :---: | :---: |
| **Projects (Strict Isolation), Nested Features (`<=32`), Multi-Feature WIs, Unallocated Bucket** | ✅ | |
| **Features-Only Soft Blockers + Transitive/Partial Propagation + Cycle Prevention** | ✅ | |
| **Strict-Guarded Feature ↔ Work Item Conversion (lossless key/identity)** | ✅ | |
| **Shared Statuses (`backlog`..`cancelled`), Manual Feature Status, Deduplicated Progress** | ✅ | |
| **Local Global Assignees (0..1 per item), Per-Project Colored Tags, Priorities, Markdown Body** | ✅ | |
| **Document-Based `.protrack` SQLite Workspaces + Recent Files + iCloud/Dropbox Safe Flush** | ✅ | |
| **Classic 3-Pane UI (`gpui-kit`), Outline View, Filtered Table View, Unallocated Triage, Trash** | ✅ | |
| **Drag-and-Drop, `Cmd+K` FTS5 Search, `Cmd+N` Quick-Add, `Cmd+Z` Session Undo/Redo** | ✅ | |
| **Kanban Board View & Visual Blocker DAG Graph Canvas** | | 🔜 Post-v1 |
| **Cross-Project "My Work" / Workload Overview** | | 🔜 Post-v1 |
| **Due/Target Dates, Effort Estimates, Activity/Decision Log** | | 🔜 Post-v1 |
| **JSON/Markdown Export, CLI/MCP Companion, Real-Time Multi-User Sync** | | 🔜 Post-v1 |
