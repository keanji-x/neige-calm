-- The aggregate id is the immutable delegation identity. Access revocation is
-- independent; only deleting the actual target through its lifecycle cascades.
CREATE TABLE share_links (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)>0),
    track_id TEXT REFERENCES tracks(id) ON DELETE CASCADE,
    area_id TEXT REFERENCES areas(id) ON DELETE CASCADE,
    approver_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    purpose TEXT NOT NULL CHECK(purpose='conversation'),
    resource_policy TEXT NOT NULL CHECK(resource_policy='current_deployment'),
    created_at INTEGER NOT NULL,
    token_hash TEXT NOT NULL UNIQUE CHECK(length(token_hash)=64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
    revoked_at INTEGER CHECK(revoked_at IS NULL OR (typeof(revoked_at)='integer' AND revoked_at>=created_at)),
    CHECK((track_id IS NOT NULL AND area_id IS NULL) OR (track_id IS NULL AND area_id IS NOT NULL))
);
CREATE INDEX share_links_track ON share_links(track_id) WHERE track_id IS NOT NULL;
CREATE INDEX share_links_area ON share_links(area_id) WHERE area_id IS NOT NULL;
CREATE TRIGGER share_links_no_replace BEFORE INSERT ON share_links
WHEN EXISTS(SELECT 1 FROM share_links WHERE id=NEW.id OR token_hash=NEW.token_hash)
BEGIN SELECT RAISE(ABORT,'share delegation cannot be replaced'); END;
CREATE TRIGGER share_links_immutable BEFORE UPDATE ON share_links
WHEN NEW.id IS NOT OLD.id OR NEW.track_id IS NOT OLD.track_id OR NEW.area_id IS NOT OLD.area_id
  OR NEW.approver_user_id IS NOT OLD.approver_user_id OR NEW.purpose IS NOT OLD.purpose
  OR NEW.resource_policy IS NOT OLD.resource_policy OR NEW.created_at IS NOT OLD.created_at
  OR NEW.token_hash IS NOT OLD.token_hash
  OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS NOT OLD.revoked_at)
BEGIN SELECT RAISE(ABORT,'share delegation is immutable and access revocation is permanent'); END;
CREATE TRIGGER share_links_no_delete BEFORE DELETE ON share_links
WHEN (OLD.track_id IS NOT NULL AND EXISTS(SELECT 1 FROM tracks WHERE id=OLD.track_id))
  OR (OLD.area_id IS NOT NULL AND EXISTS(SELECT 1 FROM areas WHERE id=OLD.area_id))
BEGIN SELECT RAISE(ABORT,'revoke share access instead of deleting delegation'); END;
