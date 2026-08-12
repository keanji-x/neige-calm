CREATE TABLE _guard_0073(n INTEGER NOT NULL CHECK(n=0));
INSERT INTO _guard_0073 SELECT count(*) FROM tasks WHERE origin!='block' AND status NOT IN ('done','failed','canceled');
DROP TABLE _guard_0073;

ALTER TABLE tasks DROP COLUMN origin;
