//! Card announcement facts belong to the same statement as the selected rows.
//! Bulk reads materialize this Track's publications; point reads retain early-exit probes.

pub(super) const CARD_COLUMNS: &str =
    "id, track_id, kind, sort, payload, title, role, deletable, created_at, updated_at";

const PUBLICATION_SCOPE: &str = "scope_track=?1 AND kind='card.added'";

pub(super) fn cards_query() -> String {
    format!(
        "WITH announcements AS MATERIALIZED (
             SELECT json_extract(payload, '$.id') AS card_id
             FROM events WHERE {PUBLICATION_SCOPE}
         )
         SELECT {CARD_COLUMNS},
                EXISTS(SELECT 1 FROM announcements WHERE announcements.card_id=cards.id)
                    AS vcs_announced
         FROM cards WHERE track_id=?1 ORDER BY sort ASC, id ASC"
    )
}

/// Static fragments owned by the VCS readers; `?1` is always the Track id.
/// These selective readers must not rebuild the whole announcement set per card path.
pub(super) fn card_query(columns: &str, selection: &str) -> String {
    format!(
        "SELECT {columns}, EXISTS(
             SELECT 1 FROM events WHERE {PUBLICATION_SCOPE}
                 AND json_extract(events.payload, '$.id')=cards.id
         ) AS vcs_announced
         FROM cards WHERE track_id=?1 AND ({selection}) ORDER BY sort ASC, id ASC"
    )
}
