-- #585: collapse CardRole::Plain into Worker. All user-facing card
-- creation paths now mint Worker (matching the +claude flow that
-- already did so). Legacy rows with role='plain' become 'worker'.
UPDATE cards SET role = 'worker' WHERE role = 'plain';
