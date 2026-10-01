-- Active readers share the Track checkout; historical leases remain exclusive writers.
ALTER TABLE workspace_leases ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'read_write'
  CHECK(access_mode IN ('read_only','read_write'));
ALTER TABLE workspace_leases ADD COLUMN read_stop_confirmed_at_ms INTEGER NULL;
DROP INDEX workspace_leases_active_path_idx;
CREATE INDEX workspace_leases_active_path_idx ON workspace_leases(path)
  WHERE state IN ('held','releasing');
ALTER TABLE workspace_leases ADD COLUMN write_root_id TEXT NULL;
UPDATE workspace_leases SET write_root_id=lease_id WHERE access_mode='read_write';
CREATE TRIGGER workspace_access_insert BEFORE INSERT ON workspace_leases
BEGIN
 SELECT RAISE(ABORT,'workspace access conflict') WHERE NEW.state IN ('held','releasing')
  AND EXISTS(SELECT 1 FROM workspace_leases l WHERE l.state IN ('held','releasing')
    AND (COALESCE(l.canonical_path,l.path)=COALESCE(NEW.canonical_path,NEW.path)
      OR COALESCE(l.canonical_path,l.path)='/' OR COALESCE(NEW.canonical_path,NEW.path)='/'
      OR substr(COALESCE(l.canonical_path,l.path),1,length(COALESCE(NEW.canonical_path,NEW.path))+1)=COALESCE(NEW.canonical_path,NEW.path)||'/'
      OR substr(COALESCE(NEW.canonical_path,NEW.path),1,length(COALESCE(l.canonical_path,l.path))+1)=COALESCE(l.canonical_path,l.path)||'/')
    AND (NEW.access_mode='read_write' OR l.access_mode='read_write')
    AND (NEW.access_mode='read_only' OR l.access_mode='read_only' OR NEW.write_root_id IS NULL OR l.write_root_id IS NULL OR NEW.write_root_id<>l.write_root_id));
 SELECT RAISE(ABORT,'read workspace cannot deliver code')
  WHERE NEW.access_mode='read_only' AND NEW.delivery_policy IS NOT NULL;
END;
CREATE TRIGGER workspace_access_update BEFORE UPDATE ON workspace_leases
BEGIN
 SELECT RAISE(ABORT,'workspace access conflict') WHERE NEW.state IN ('held','releasing')
  AND EXISTS(SELECT 1 FROM workspace_leases l WHERE l.lease_id<>NEW.lease_id
    AND l.state IN ('held','releasing')
    AND (COALESCE(l.canonical_path,l.path)=COALESCE(NEW.canonical_path,NEW.path)
      OR COALESCE(l.canonical_path,l.path)='/' OR COALESCE(NEW.canonical_path,NEW.path)='/'
      OR substr(COALESCE(l.canonical_path,l.path),1,length(COALESCE(NEW.canonical_path,NEW.path))+1)=COALESCE(NEW.canonical_path,NEW.path)||'/'
      OR substr(COALESCE(NEW.canonical_path,NEW.path),1,length(COALESCE(l.canonical_path,l.path))+1)=COALESCE(l.canonical_path,l.path)||'/')
    AND (NEW.access_mode='read_write' OR l.access_mode='read_write')
    AND (NEW.access_mode='read_only' OR l.access_mode='read_only' OR NEW.write_root_id IS NULL OR l.write_root_id IS NULL OR NEW.write_root_id<>l.write_root_id));
 SELECT RAISE(ABORT,'workspace access is immutable') WHERE NEW.access_mode<>OLD.access_mode
  OR (OLD.access_mode='read_only' AND NEW.delivery_policy IS NOT NULL);
END;
-- Execution references retain access beyond a model's final response.
ALTER TABLE workspace_leases ADD COLUMN holder_kind TEXT NOT NULL DEFAULT 'task'
  CHECK(holder_kind IN ('task','native','terminal','forge'));
ALTER TABLE workspace_leases ADD COLUMN holder_id TEXT NULL;
ALTER TABLE workspace_leases ADD COLUMN holder_phase TEXT NULL
  CHECK(holder_phase IS NULL OR holder_phase IN ('issuing','running','stopping','stopped'));
CREATE INDEX workspace_leases_execution_holder_idx
  ON workspace_leases(holder_kind,holder_id,state);
ALTER TABLE workspace_leases ADD COLUMN native_provider TEXT NULL
  CHECK(native_provider IS NULL OR native_provider IN ('codex','claude'));
CREATE TABLE workspace_execution_bindings (
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude')),
 holder_id TEXT NOT NULL,
 card_id TEXT NOT NULL,
 cwd TEXT NOT NULL,
 scope_phase TEXT NOT NULL CHECK(scope_phase IN ('new','recovering','ready')),
 PRIMARY KEY(provider,holder_id)
);

ALTER TABLE workspace_leases ADD COLUMN execution_artifacts_json TEXT NULL;

ALTER TABLE workspace_leases ADD COLUMN execution_parent_holder_id TEXT NULL;

-- New native issuance persists its exact client nonce; historical executions have none.
ALTER TABLE workspace_leases ADD COLUMN native_client_id TEXT NULL;

-- Positive provider identity of an observed native turn, independent of issuance phase.
ALTER TABLE workspace_leases ADD COLUMN native_observed_turn_id TEXT NULL;
