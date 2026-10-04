-- Rename the development template without changing saved working instructions,
-- issue inputs, plugin bindings, approvals, or execution history.
UPDATE tracks SET template_id = 'dev' WHERE template_id = 'issue-development';
UPDATE areas SET default_template_id = 'dev'
 WHERE default_template_id = 'issue-development';
