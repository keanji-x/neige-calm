# CR-1501-FILES composition

The parent-approved design freezes the indexed GET and required JSON DTO in
`docs/design-1501-task-file-access.md`. Core owns the strict response/identity,
base64/byte-count/basename checks and bounded preview text. No generic transport
change is needed.

`AcceptedReport` gains only optional `renderArtifact(index)`. Artifact references
remain text; the feature's action and dialog render props. App supplies the fixed
Track/key/attempt/index operation through the existing transport and Unauthorized
channel, validates/decodes bytes, and owns the octet-stream Blob download.

`useTaskArtifactFiles` belongs to the Track route, so only one file dialog can exist
across its task/history reports. Its action passes through app-owned TaskRecovery
and TaskReport props; no new Context or architecture allowlist is needed. Opening explicitly mounts the read; closing,
changing selection or unmounting aborts it, ignores late completion and revokes
the object URL. Payloads do not enter QueryClient, storage, or persistent task
state. Retry is explicit. Parent owns generated OpenAPI and ownership trailers.
