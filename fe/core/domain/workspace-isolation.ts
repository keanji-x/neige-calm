/** User-facing consequences of the server's `none` workspace contract (§4.2, #2666). */
export const NO_ISOLATION_LABEL = '源文件夹模式 · 未隔离';
export const NO_ISOLATION_EXPLANATION = 'The Planner works directly in this source folder, and Planner commits land on its current branch. Source-folder work runs without Neige locking, monitoring, evidence recording, or delivery. Neige never creates, cleans up, or deletes this folder. Task workers require an isolated worktree. Tracks created in bare-folder mode need an explicit upgrade. Other attached Git tracks may create a worktree on first worker dispatch. Exception: a deletion process already started may keep running after a server crash and can still remove a folder chosen after restart (#2862).';

/** Composer labels also cover Neige's managed private folder, without implying an attached workspace. */
export const WORKSPACE_ISOLATION_LABEL = 'Workspace isolation';
export const WORKTREE_LABEL = 'Worktree';
export const NONE_LABEL = 'None';
export const PRIVATE_FOLDER_ISOLATION_REASON = 'Private Neige folder: no worktree. Choose a Git folder to use one.';
