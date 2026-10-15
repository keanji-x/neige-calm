-- Person-granted authority, with storage-level shape fences.
ALTER TABLE tracks ADD COLUMN admin_granted_at INTEGER
  CONSTRAINT track_admin_is_top_level
  CHECK (admin_granted_at IS NULL OR
    (parent_track_id IS NULL AND creator_track_id IS NULL AND COALESCE(purpose,'') <> 'area-chat'));

UPDATE tracks SET admin_granted_at =
  (SELECT MIN(a.created_at) FROM tracks a WHERE a.creator_track_id = tracks.id)
WHERE id IN (SELECT creator_track_id FROM tracks WHERE creator_track_id IS NOT NULL)
  AND parent_track_id IS NULL AND creator_track_id IS NULL
  AND COALESCE(purpose,'') <> 'area-chat'
  AND NOT EXISTS (SELECT 1 FROM managed_track_identities m WHERE m.track_id = tracks.id)
  AND plugin_scope IS NULL
  AND (SELECT kind FROM areas WHERE areas.id = tracks.area_id) <> 'system';
