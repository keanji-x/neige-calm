//! Card announcement facts belong to the same statement as the selected rows.
//! Materialize this Track's publications once, rather than searching its history per card.

pub(super) const CARD_COLUMNS: &str =
    "id, track_id, kind, sort, payload, title, role, deletable, created_at, updated_at";

/// Both fragments are static SQL owned by the VCS readers. `?1` is always the Track id.
pub(super) fn card_query(columns: &str, selection: &str) -> String {
    format!(
        "WITH announcements AS MATERIALIZED (
             SELECT json_extract(payload, '$.id') AS card_id
             FROM events WHERE scope_track=?1 AND kind='card.added'
         )
         SELECT {columns},
                EXISTS(SELECT 1 FROM announcements WHERE announcements.card_id=cards.id)
                    AS vcs_announced
         FROM cards WHERE track_id=?1 AND ({selection}) ORDER BY sort ASC, id ASC"
    )
}
