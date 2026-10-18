SELECT {columns}, lease_owner,
       (SELECT phase FROM operations WHERE id = workspace_leases.lease_owner) AS owner_phase
FROM workspace_leases
WHERE track_id = ?1 AND state IN ('held', 'releasing')
ORDER BY lease_id
