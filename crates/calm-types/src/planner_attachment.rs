//! Planner conversation attachments — the shared wire vocabulary.
//!
//! Everything about an attachment's identity lives here so there is exactly one
//! truth table for "which four image formats exist". [`AttachmentId`] carries
//! its own extension, and [`AttachmentFormat::parse_ext`] is the only reader of
//! that extension, so the on-disk file name, the sniffed magic number and the
//! `Content-Type` a read-back sends can never disagree.
//!
//! # Why the id carries the extension
//!
//! `<uuid>.<ext>` makes an attachment locatable from its id alone: the server
//! joins it onto a directory it derived itself and is done. A bare UUID would
//! force a second, separate lookup of "which extension did this one get",
//! duplicated at every site that needs a path. The grammar admits no `/`, no
//! `..` and no second `.`, so a caller-supplied id cannot name anything but a
//! direct child of the directory the server chose.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::CardId;
use ts_rs::TS;
use utoipa::ToSchema;

/// The image formats a planner attachment may have.
///
/// Deliberately a strict subset of what codex will decode: these four are the
/// formats codex keeps the *source bytes* of. Anything else it re-encodes or
/// silently replaces with placeholder text, and neither outcome is visible to
/// the user, so the upload endpoint refuses it instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl AttachmentFormat {
    /// Every format, for exhaustiveness tests.
    pub const ALL: [AttachmentFormat; 4] = [
        AttachmentFormat::Png,
        AttachmentFormat::Jpeg,
        AttachmentFormat::Gif,
        AttachmentFormat::Webp,
    ];

    /// The file-name extension, without the dot.
    pub fn ext(self) -> &'static str {
        match self {
            AttachmentFormat::Png => "png",
            AttachmentFormat::Jpeg => "jpg",
            AttachmentFormat::Gif => "gif",
            AttachmentFormat::Webp => "webp",
        }
    }

    /// The `Content-Type` a read-back must send.
    pub fn mime(self) -> &'static str {
        match self {
            AttachmentFormat::Png => "image/png",
            AttachmentFormat::Jpeg => "image/jpeg",
            AttachmentFormat::Gif => "image/gif",
            AttachmentFormat::Webp => "image/webp",
        }
    }

    /// Inverse of [`AttachmentFormat::ext`]. Exhaustive over the four; every
    /// other spelling — including `jpeg`, `svg` and any uppercase form — is
    /// `None`, because the extension is minted by this module and never by a
    /// caller.
    pub fn parse_ext(ext: &str) -> Option<Self> {
        match ext {
            "png" => Some(AttachmentFormat::Png),
            "jpg" => Some(AttachmentFormat::Jpeg),
            "gif" => Some(AttachmentFormat::Gif),
            "webp" => Some(AttachmentFormat::Webp),
            _ => None,
        }
    }
}

/// Why a string is not a valid [`AttachmentId`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid attachment id `{value}`: {reason}")]
pub struct AttachmentIdError {
    pub value: String,
    pub reason: &'static str,
}

/// `<uuid-v4>.<ext>` — an attachment's id *and* its file name.
///
/// The inner string is private and the only constructor is
/// [`AttachmentId::parse`], so a value of this type is always a single path
/// segment matching
/// `[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}.(png|jpg|gif|webp)`.
/// `Deserialize` goes through the same gate, so an id off the wire is checked
/// before it can reach a `join`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[schema(value_type = String)]
pub struct AttachmentId(String);

impl AttachmentId {
    /// The single gate. Rejects anything that is not the exact grammar.
    pub fn parse(value: &str) -> Result<Self, AttachmentIdError> {
        let invalid = |reason: &'static str| AttachmentIdError {
            value: value.to_string(),
            reason,
        };
        let (uuid, ext) = value
            .split_once('.')
            .ok_or_else(|| invalid("expected `<uuid>.<ext>`"))?;
        if AttachmentFormat::parse_ext(ext).is_none() {
            return Err(invalid("extension must be one of png, jpg, gif, webp"));
        }
        check_uuid_v4(uuid).map_err(invalid)?;
        Ok(AttachmentId(value.to_string()))
    }

    /// The id as it appears on the wire and on disk.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The extension's format.
    pub fn format(&self) -> AttachmentFormat {
        let ext = self
            .0
            .rsplit_once('.')
            .expect("AttachmentId is only constructible through parse")
            .1;
        AttachmentFormat::parse_ext(ext).expect("AttachmentId is only constructible through parse")
    }
}

/// Validate the `<uuid>` half: 36 chars, dashes at 8/13/18/23, lowercase hex
/// elsewhere, version nibble `4`, variant nibble in `[89ab]`.
fn check_uuid_v4(uuid: &str) -> Result<(), &'static str> {
    const DASHES: [usize; 4] = [8, 13, 18, 23];
    let bytes = uuid.as_bytes();
    if bytes.len() != 36 {
        return Err("uuid half must be 36 characters");
    }
    for (index, byte) in bytes.iter().enumerate() {
        if DASHES.contains(&index) {
            if *byte != b'-' {
                return Err("uuid half must be dash-separated 8-4-4-4-12");
            }
            continue;
        }
        if !byte.is_ascii_digit() && !(b'a'..=b'f').contains(byte) {
            return Err("uuid half must be lowercase hexadecimal");
        }
    }
    if bytes[14] != b'4' {
        return Err("uuid half must be version 4");
    }
    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return Err("uuid half must carry the RFC 4122 variant");
    }
    Ok(())
}

impl std::fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl Serialize for AttachmentId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AttachmentId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        AttachmentId::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// The REST path an attachment's bytes are read back from.
///
/// The single builder. Every place a client is handed a way to reach these
/// bytes — the upload response, a queued message, a transcript segment — goes
/// through this function, so no client has to compose a path of its own and
/// there is no second spelling of the route to keep in step with the router.
pub fn attachment_url(card_id: &CardId, id: &AttachmentId) -> String {
    format!(
        "/api/cards/{}/planner/attachments/{}",
        card_id.as_str(),
        id.as_str()
    )
}

/// One attachment as the frontend sees it in a queue entry or a transcript
/// segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct PlannerAttachment {
    pub id: AttachmentId,
    /// Derived from `id`, never stored separately — see
    /// [`PlannerAttachment::new`].
    pub content_type: String,
    pub size: u64,
    /// Where to read the bytes. Also derived, by [`attachment_url`].
    ///
    /// #1505 S6. Carried rather than left for the client to build: the
    /// transcript and the pending-queue read both need a way to reach these
    /// bytes, and a client that assembles `/api/cards/{card}/planner/
    /// attachments/{id}` for itself is a second spelling of a route only the
    /// router should own. The host path the server holds beside this is NOT
    /// here and must not be.
    pub url: String,
}

impl PlannerAttachment {
    /// `content_type` and `url` are computed here rather than accepted, so the
    /// only way to build one is with a `Content-Type` and a path that agree
    /// with the id.
    pub fn new(card_id: &CardId, id: AttachmentId, size: u64) -> Self {
        let content_type = id.format().mime().to_string();
        let url = attachment_url(card_id, &id);
        PlannerAttachment {
            id,
            content_type,
            size,
            url,
        }
    }
}

/// `201` body of `POST /api/cards/{id}/planner/attachments`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct UploadAttachmentResponse {
    pub attachment_id: AttachmentId,
    pub content_type: String,
    pub size: u64,
    /// Absolute REST path the browser reads the bytes back from. Server-built:
    /// the client never composes a path of its own.
    ///
    /// Durable only once the attachment is bound. An upload lands in the
    /// server's `staging/` directory, and a staged attachment is swept once it
    /// is older than the 24h orphan TTL, after which this path answers 400.
    /// Sending or queueing a message that names the id binds it — the bytes
    /// move into `bound/`, which nothing sweeps — and from that moment this
    /// path is stable for the life of the card. So the window in which this
    /// url can stop working is exactly "uploaded, never sent, 24 hours".
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png";

    #[test]
    fn ext_and_parse_ext_are_inverse_over_all_four() {
        for format in AttachmentFormat::ALL {
            assert_eq!(AttachmentFormat::parse_ext(format.ext()), Some(format));
        }
    }

    #[test]
    fn parse_accepts_the_grammar_and_reads_back_its_format() {
        let id = AttachmentId::parse(OK).unwrap();
        assert_eq!(id.as_str(), OK);
        assert_eq!(id.format(), AttachmentFormat::Png);
        assert_eq!(id.format().mime(), "image/png");
    }

    #[test]
    fn parse_rejects_every_traversal_and_extension_shape() {
        // Each of these is a distinct way a caller-supplied id could name
        // something other than a direct child of the server's directory, or
        // could name a format we do not serve.
        let bad = [
            "../0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png",
            "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png/../../etc/passwd",
            "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png.svg",
            "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.svg",
            "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.jpeg",
            "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.PNG",
            "0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f",
            "0189BC3F-2B1A-4C7D-9E4F-1A2B3C4D5E6F.png",
            "0189bc3f-2b1a-1c7d-9e4f-1a2b3c4d5e6f.png",
            "0189bc3f-2b1a-4c7d-1e4f-1a2b3c4d5e6f.png",
            "0189bc3f2b1a4c7d9e4f1a2b3c4d5e6f.png",
            "",
            ".png",
            "/etc/passwd.png",
        ];
        for value in bad {
            assert!(AttachmentId::parse(value).is_err(), "must reject `{value}`");
        }
    }

    #[test]
    fn deserialize_runs_the_same_gate_as_parse() {
        let good: AttachmentId = serde_json::from_str(&format!("\"{OK}\"")).unwrap();
        assert_eq!(good.as_str(), OK);
        assert_eq!(serde_json::to_string(&good).unwrap(), format!("\"{OK}\""));
        assert!(
            serde_json::from_str::<AttachmentId>("\"../secret.png\"").is_err(),
            "a traversal id must not survive deserialization"
        );
    }

    #[test]
    fn attachment_content_type_and_url_are_derived_from_the_id() {
        let id = AttachmentId::parse("0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.webp").unwrap();
        let card = CardId::from("card-9");
        let attachment = PlannerAttachment::new(&card, id, 7);
        assert_eq!(
            attachment.content_type, "image/webp",
            "content type must come from the id, never from a caller header"
        );
        assert_eq!(
            attachment.url,
            "/api/cards/card-9/planner/attachments/0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.webp",
            "the url must come from the same builder the upload response uses"
        );
    }

    /// The host path is the one thing about an attachment that must never
    /// reach a browser, and this type is the shape that reaches one.
    #[test]
    fn the_wire_shape_has_exactly_four_keys_and_no_path() {
        let id = AttachmentId::parse("0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png").unwrap();
        let json =
            serde_json::to_value(PlannerAttachment::new(&CardId::from("card-9"), id, 7)).unwrap();
        let mut keys = json
            .as_object()
            .expect("an attachment is an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, ["contentType", "id", "size", "url"]);
    }
}
