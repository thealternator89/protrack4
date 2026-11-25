-- ============================================================================
-- ProTrack Migration 002: External Item Links
-- ============================================================================

ALTER TABLE items ADD COLUMN link_url TEXT;
ALTER TABLE items ADD COLUMN link_label TEXT;

DROP TRIGGER IF EXISTS trg_items_fts_ai;
DROP TRIGGER IF EXISTS trg_items_fts_ad;
DROP TRIGGER IF EXISTS trg_items_fts_au;
DROP TABLE IF EXISTS items_fts;

CREATE VIRTUAL TABLE items_fts USING fts5(
    item_key,
    title,
    description,
    link_label,
    content='items',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER trg_items_fts_ai AFTER INSERT ON items BEGIN
    INSERT INTO items_fts(rowid, item_key, title, description, link_label)
    VALUES (NEW.rowid, NEW.item_key, NEW.title, NEW.description, NEW.link_label);
END;

CREATE TRIGGER trg_items_fts_ad AFTER DELETE ON items BEGIN
    INSERT INTO items_fts(items_fts, rowid, item_key, title, description, link_label)
    VALUES ('delete', OLD.rowid, OLD.item_key, OLD.title, OLD.description, OLD.link_label);
END;

CREATE TRIGGER trg_items_fts_au AFTER UPDATE OF item_key, title, description, link_label ON items BEGIN
    INSERT INTO items_fts(items_fts, rowid, item_key, title, description, link_label)
    VALUES ('delete', OLD.rowid, OLD.item_key, OLD.title, OLD.description, OLD.link_label);
    INSERT INTO items_fts(rowid, item_key, title, description, link_label)
    VALUES (NEW.rowid, NEW.item_key, NEW.title, NEW.description, NEW.link_label);
END;

INSERT INTO items_fts(items_fts) VALUES ('rebuild');
