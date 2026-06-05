-- Stable fixture identity for non-destructive event placement seeding.
--
-- The Porchfest seed fixture has duplicate public labels, so label/category
-- cannot safely identify a row. `source_key` gives importer-owned rows a
-- durable key while user-created planner rows stay NULL.

ALTER TABLE event_placements
    ADD COLUMN source_key text;

CREATE UNIQUE INDEX idx_event_placements_source_key
    ON event_placements (tenant_id, event_layer_id, source_key)
    WHERE source_key IS NOT NULL;
