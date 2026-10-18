CREATE TRIGGER reject_transcript_refresh
BEFORE INSERT ON track_vcs_commits
WHEN NEW.message = 'transcript refresh'
BEGIN
    SELECT RAISE(ABORT, 'reject transcript refresh');
END;
