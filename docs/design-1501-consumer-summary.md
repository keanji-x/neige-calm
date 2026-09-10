# Optional consumer completion summary

A consumer may opt into presentation through the existing arbitrary completion
`result`, only when its task/user result contract permits this root shape:

```json
{
  "$neige_result_presentation": "worker-summary-v1",
  "summary": "Key results, source used, actual test outcomes.",
  "details": {"direct_call_command": "full verification script"}
}
```

Recognition requires exactly these three keys, this discriminator, a nonblank
summary of at most 2048 UTF-8 bytes, and present details of any JSON type.
The safely quoted summary replaces the raw preview within the existing budget.
It is untrusted worker data, not kernel verification or Planner acceptance.
Full details remain in the original completion event. Reference qualification
continues comparing the original entire result, execution identity and event ID.
Existing kernel source binding/check facts remain separate evidence.

Malformed, oversized, unknown-version and ordinary legacy results retain their
existing accepted storage and bounded raw preview, including its truncation flag.
No heuristic field selection, nested/string decoding, new Event/queue/MCP fields,
or database migration is introduced. A historical value with this exact reserved
shape cannot be distinguished from a deliberate opt-in.

Only the CandidateConsumer prompt teaches this optional convention. A mandated
root result shape takes precedence. Strict R1/R2 reviewer reports are unchanged.
The change neither qualifies another source nor accepts a consumer automatically.

Checks cover production rendering and queued transport with long details,
legacy/malformed/UTF-8 boundaries, safe framing, full event retention and exact
reference mismatches. Native validation separately requires actual Planner
judgment and explicit acceptance of the exact consumer and candidate.
