# User values are argv; the input document is encoded by the owning lowerer.
out=$(printf '%s' "$3" | gh api --hostname "$1" "$2" --method POST --input -) || exit 3
printf '%s' "$out" | jq -sce '
  if length == 1 then .[0] else error("expected one create receipt") end |
  if (.number | type == "number") and .number > 0 and (.number | floor) == .number
     and (.html_url | type == "string") and (.html_url | startswith("https://"))
  then {issue_number:.number,issue_url:.html_url}
  else error("invalid create receipt") end' || exit 3
