-- Raw objects retain their exact bytes; conversion is explicit and resumable.
ALTER TABLE track_vcs_objects ADD COLUMN encoding TEXT NOT NULL DEFAULT 'raw';
ALTER TABLE track_vcs_objects ADD COLUMN decoded_size INTEGER
CHECK (
    (encoding = 'raw' AND decoded_size IS NULL)
    OR (encoding = 'zlib-v1' AND typeof(decoded_size) = 'integer'
        AND decoded_size BETWEEN 0 AND 67108864)
);

-- Both full reachability and incremental-frontier GC budget decoded work.
CREATE VIEW track_vcs_object_sizes AS
SELECT hash, kind,
       CASE WHEN encoding='raw' THEN length(bytes)
            WHEN encoding='zlib-v1' THEN decoded_size END AS decoded_size
FROM track_vcs_objects;
