/** User-facing consequences of the server's `none` workspace contract (§4.2, #2666). */
export const NO_ISOLATION_LABEL = 'Not isolated · no guarantees';
export const NO_ISOLATION_EXPLANATION = 'The Planner works directly in this folder, and Planner commits land on its current branch. Neige does not lock, monitor, record evidence for, or deliver this work. Neige never creates, cleans up or deletes this folder. In a Git folder, dispatching a task worker automatically upgrades this Track to an isolated worktree while preserving the existing Planner folder. Non-Git folders remain unisolated and cannot start task workers. Exception: a deletion process already started may keep running after a server crash and can still remove a folder chosen after restart (#2862).';

/** Composer labels also cover Neige's managed private folder, without implying an attached workspace. */
export const WORKSPACE_ISOLATION_LABEL = 'Workspace isolation';
export const WORKTREE_LABEL = 'Worktree';
export const NONE_LABEL = 'None';
export const PRIVATE_FOLDER_ISOLATION_REASON = 'Private Neige folder: no worktree. Choose a Git folder to use one.';
