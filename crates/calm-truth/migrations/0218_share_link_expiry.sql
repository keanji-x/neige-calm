-- Share access has a fixed lifetime; delegation remains durable after expiry.
ALTER TABLE share_links ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0;
UPDATE share_links SET expires_at = created_at + 86400000 WHERE expires_at = 0;
CREATE TRIGGER share_links_expiry BEFORE INSERT ON share_links
WHEN NOT (typeof(NEW.expires_at) = 'integer' AND NEW.expires_at > NEW.created_at)
BEGIN SELECT RAISE(ABORT,'share expiry must be an integer after creation'); END;
DROP TRIGGER share_links_immutable;
CREATE TRIGGER share_links_immutable BEFORE UPDATE ON share_links
WHEN NEW.id IS NOT OLD.id OR NEW.track_id IS NOT OLD.track_id OR NEW.area_id IS NOT OLD.area_id
  OR NEW.approver_user_id IS NOT OLD.approver_user_id OR NEW.purpose IS NOT OLD.purpose
  OR NEW.resource_policy IS NOT OLD.resource_policy OR NEW.created_at IS NOT OLD.created_at
  OR NEW.token_hash IS NOT OLD.token_hash OR NEW.expires_at IS NOT OLD.expires_at
  OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS NOT OLD.revoked_at)
BEGIN SELECT RAISE(ABORT,'share delegation is immutable and access revocation is permanent'); END;
