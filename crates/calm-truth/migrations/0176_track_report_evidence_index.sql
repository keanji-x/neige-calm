-- Track activity E7 reads one track's newest non-user report edit.
-- Keep the calendar's report-day index for its cross-track time ranges.
CREATE INDEX idx_events_report_track_at ON events(scope_track, at)
    WHERE kind = 'track.report_edited';
