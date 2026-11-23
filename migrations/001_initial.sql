-- ============================================================================
-- ProTrack Migration 001: Initial Schema
-- Target: SQLite 3.45+ (bundled via rusqlite)
-- ============================================================================

PRAGMA foreign_keys = ON;

-- ----------------------------------------------------------------------------
-- 1. Workspace Metadata
-- ----------------------------------------------------------------------------
CREATE TABLE workspace_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    app_min_version TEXT NOT NULL DEFAULT '0.1.0',
    workspace_name TEXT NOT NULL DEFAULT 'Personal Workspace' CHECK (length(trim(workspace_name)) > 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- ----------------------------------------------------------------------------
-- 2. Projects (Strict Isolation Boundary)
-- ----------------------------------------------------------------------------
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    key_prefix TEXT NOT NULL CHECK (
        length(key_prefix) BETWEEN 2 AND 10
        AND key_prefix GLOB '[A-Z][A-Z0-9]*'
    ),
    next_item_seq INTEGER NOT NULL DEFAULT 1 CHECK (next_item_seq >= 1),
    description TEXT NOT NULL DEFAULT '',
    color TEXT NOT NULL DEFAULT '#6366F1',
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'completed', 'archived')),
    sort_order TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX idx_projects_key_prefix ON projects(key_prefix);
CREATE INDEX idx_projects_status_sort ON projects(status, sort_order);

-- ----------------------------------------------------------------------------
-- 3. Local Assignees (Global Across Workspace)
-- ----------------------------------------------------------------------------
CREATE TABLE assignees (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    handle TEXT NOT NULL CHECK (length(trim(handle)) BETWEEN 1 AND 8),
    color TEXT NOT NULL DEFAULT '#64748B',
    role TEXT,
    notes TEXT NOT NULL DEFAULT '',
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX idx_assignees_handle ON assignees(handle COLLATE NOCASE);
CREATE INDEX idx_assignees_active ON assignees(archived_at, name);

-- ----------------------------------------------------------------------------
-- 4. Project-Scoped Tags
-- ----------------------------------------------------------------------------
CREATE TABLE tags (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    color TEXT NOT NULL DEFAULT '#8B5CF6',
    created_at TEXT NOT NULL,
    UNIQUE (id, project_id)
) STRICT;

CREATE UNIQUE INDEX idx_tags_project_name ON tags(project_id, name COLLATE NOCASE);

-- ----------------------------------------------------------------------------
-- 5. Unified Items Table (Features & Work Items)
-- Note: Must keep implicit rowid for FTS5 external content binding.
-- ----------------------------------------------------------------------------
CREATE TABLE items (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    item_number INTEGER NOT NULL CHECK (item_number >= 1),
    item_key TEXT NOT NULL CHECK (length(trim(item_key)) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('feature', 'work_item')),
    parent_feature_id TEXT,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'todo' CHECK (
        status IN ('backlog', 'todo', 'in_progress', 'done', 'cancelled')
    ),
    priority TEXT NOT NULL DEFAULT 'none' CHECK (
        priority IN ('none', 'low', 'medium', 'high', 'urgent')
    ),
    assignee_id TEXT REFERENCES assignees(id) ON DELETE SET NULL,
    sort_order TEXT NOT NULL,
    deleted_at TEXT,
    trash_root_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    UNIQUE (id, project_id),
    UNIQUE (project_id, item_number),
    UNIQUE (project_id, item_key),

    -- Composite FKs guarantee same-project containment (MATCH SIMPLE skips NULLs)
    FOREIGN KEY (parent_feature_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (trash_root_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE,

    CHECK (
        (kind = 'work_item' AND parent_feature_id IS NULL)
        OR (kind = 'feature' AND (parent_feature_id IS NULL OR parent_feature_id != id))
    ),
    CHECK (
        (deleted_at IS NULL AND trash_root_id IS NULL)
        OR (deleted_at IS NOT NULL AND trash_root_id IS NOT NULL)
    )
) STRICT;

CREATE INDEX idx_items_project_active
    ON items(project_id, deleted_at, kind, parent_feature_id, sort_order);
CREATE INDEX idx_items_parent_feature
    ON items(parent_feature_id) WHERE parent_feature_id IS NOT NULL;
CREATE INDEX idx_items_trash_root
    ON items(trash_root_id) WHERE trash_root_id IS NOT NULL;
CREATE INDEX idx_items_assignee
    ON items(assignee_id) WHERE assignee_id IS NOT NULL;
CREATE INDEX idx_items_project_status
    ON items(project_id, status) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- 6. Feature <-> Work Item Many-to-Many Attachment Table
-- Active work items with 0 rows here belong to the Project's Unallocated bucket.
-- ----------------------------------------------------------------------------
CREATE TABLE feature_work_items (
    project_id TEXT NOT NULL,
    feature_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    sort_order TEXT NOT NULL,
    created_at TEXT NOT NULL,

    PRIMARY KEY (feature_id, work_item_id),
    FOREIGN KEY (feature_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (work_item_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_fwi_work_item ON feature_work_items(work_item_id);
CREATE INDEX idx_fwi_feature_order ON feature_work_items(feature_id, sort_order);

-- ----------------------------------------------------------------------------
-- 7. Item <-> Tag Attachment Table (Same-Project Enforced)
-- ----------------------------------------------------------------------------
CREATE TABLE item_tags (
    project_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    tag_id TEXT NOT NULL,
    created_at TEXT NOT NULL,

    PRIMARY KEY (item_id, tag_id),
    FOREIGN KEY (item_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id, project_id)
        REFERENCES tags(id, project_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_item_tags_tag ON item_tags(tag_id);

-- ----------------------------------------------------------------------------
-- 8. Blocking Dependencies Table (Features-Only Targets, Same-Project Enforced)
-- ----------------------------------------------------------------------------
CREATE TABLE item_blockers (
    project_id TEXT NOT NULL,
    blocked_item_id TEXT NOT NULL,
    blocker_feature_id TEXT NOT NULL,
    created_at TEXT NOT NULL,

    PRIMARY KEY (blocked_item_id, blocker_feature_id),
    CHECK (blocked_item_id != blocker_feature_id),
    FOREIGN KEY (blocked_item_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (blocker_feature_id, project_id)
        REFERENCES items(id, project_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX idx_blockers_blocker ON item_blockers(blocker_feature_id);

-- ----------------------------------------------------------------------------
-- 9. Structural Integrity Triggers (Kind & Conversion Enforcement)
-- ----------------------------------------------------------------------------

-- Parent of a feature must be a feature
CREATE TRIGGER trg_items_parent_is_feature_ins
BEFORE INSERT ON items
WHEN NEW.parent_feature_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'INV-02 violation: parent_feature_id must reference a feature')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.parent_feature_id AND kind = 'feature'
    );
END;

CREATE TRIGGER trg_items_parent_is_feature_upd
BEFORE UPDATE OF parent_feature_id ON items
WHEN NEW.parent_feature_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'INV-02 violation: parent_feature_id must reference a feature')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.parent_feature_id AND kind = 'feature'
    );
END;

-- feature_work_items must connect a feature to a work_item
CREATE TRIGGER trg_fwi_kinds_ins
BEFORE INSERT ON feature_work_items
BEGIN
    SELECT RAISE(ABORT, 'INV-03 violation: feature_id must have kind=feature')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.feature_id AND kind = 'feature'
    );
    SELECT RAISE(ABORT, 'INV-03 violation: work_item_id must have kind=work_item')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.work_item_id AND kind = 'work_item'
    );
END;

CREATE TRIGGER trg_fwi_kinds_upd
BEFORE UPDATE OF feature_id, work_item_id ON feature_work_items
BEGIN
    SELECT RAISE(ABORT, 'INV-03 violation: feature_id must have kind=feature')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.feature_id AND kind = 'feature'
    );
    SELECT RAISE(ABORT, 'INV-03 violation: work_item_id must have kind=work_item')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.work_item_id AND kind = 'work_item'
    );
END;

-- item_blockers target must always be a feature (INV-04)
CREATE TRIGGER trg_blockers_kind_ins
BEFORE INSERT ON item_blockers
BEGIN
    SELECT RAISE(ABORT, 'INV-04 violation: blocker_feature_id must have kind=feature')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.blocker_feature_id AND kind = 'feature'
    );
END;

CREATE TRIGGER trg_blockers_kind_upd
BEFORE UPDATE OF blocker_feature_id ON item_blockers
BEGIN
    SELECT RAISE(ABORT, 'INV-04 violation: blocker_feature_id must have kind=feature')
    WHERE NOT EXISTS (
        SELECT 1 FROM items WHERE id = NEW.blocker_feature_id AND kind = 'feature'
    );
END;

-- Guard kind conversions unless referencing structural links were resolved first
CREATE TRIGGER trg_items_kind_guard_upd
BEFORE UPDATE OF kind ON items
WHEN OLD.kind != NEW.kind
BEGIN
    -- Feature -> Work Item requires 0 child features, 0 attached work items, 0 dependents
    SELECT RAISE(ABORT, 'INV-09 violation: cannot convert feature with child features')
    WHERE OLD.kind = 'feature' AND NEW.kind = 'work_item'
      AND EXISTS (SELECT 1 FROM items WHERE parent_feature_id = OLD.id);

    SELECT RAISE(ABORT, 'INV-09 violation: cannot convert feature with attached work items')
    WHERE OLD.kind = 'feature' AND NEW.kind = 'work_item'
      AND EXISTS (SELECT 1 FROM feature_work_items WHERE feature_id = OLD.id);

    SELECT RAISE(ABORT, 'INV-09 violation: cannot convert feature referenced as a blocker')
    WHERE OLD.kind = 'feature' AND NEW.kind = 'work_item'
      AND EXISTS (SELECT 1 FROM item_blockers WHERE blocker_feature_id = OLD.id);

    -- Work Item -> Feature requires clearing feature_work_items links first
    SELECT RAISE(ABORT, 'INV-08 violation: must migrate/clear feature_work_items before converting to feature')
    WHERE OLD.kind = 'work_item' AND NEW.kind = 'feature'
      AND EXISTS (SELECT 1 FROM feature_work_items WHERE work_item_id = OLD.id);
END;

-- ----------------------------------------------------------------------------
-- 10. Full-Text Search (FTS5 External Content)
-- ----------------------------------------------------------------------------
CREATE VIRTUAL TABLE items_fts USING fts5(
    item_key,
    title,
    description,
    content='items',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER trg_items_fts_ai AFTER INSERT ON items BEGIN
    INSERT INTO items_fts(rowid, item_key, title, description)
    VALUES (NEW.rowid, NEW.item_key, NEW.title, NEW.description);
END;

CREATE TRIGGER trg_items_fts_ad AFTER DELETE ON items BEGIN
    INSERT INTO items_fts(items_fts, rowid, item_key, title, description)
    VALUES ('delete', OLD.rowid, OLD.item_key, OLD.title, OLD.description);
END;

CREATE TRIGGER trg_items_fts_au AFTER UPDATE OF item_key, title, description ON items BEGIN
    INSERT INTO items_fts(items_fts, rowid, item_key, title, description)
    VALUES ('delete', OLD.rowid, OLD.item_key, OLD.title, OLD.description);
    INSERT INTO items_fts(rowid, item_key, title, description)
    VALUES (NEW.rowid, NEW.item_key, NEW.title, NEW.description);
END;
