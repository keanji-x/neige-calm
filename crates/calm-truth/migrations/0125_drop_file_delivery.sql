-- #1893 S2 — isolated file delivery and candidate verify/review/repair are deleted with their
-- code. Their tables go, children before parents; each table's triggers go with it. Leftover
-- `task-file-publication` / `candidate-verify` operations and their settled events stay as
-- inert history: nothing reads them.
DROP TABLE task_candidate_decision_bindings;
DROP TABLE task_candidate_decisions;
DROP TABLE task_candidate_input_bindings;
DROP TABLE task_candidate_verification_allocations;
DROP TABLE task_file_candidates;
DROP TABLE task_file_input_bindings;
DROP TABLE task_file_publications;
DROP TABLE task_candidate_repairs;
