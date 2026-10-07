# Enumerate the complete REST inventory; search indexing is not landing evidence.
page=1
matches='[]'
while :; do
  out=$(gh api --hostname "$1" "$2" --method GET -f state=all -f per_page=100 -f "page=$page") || exit 3
  selected=$(printf '%s' "$out" | jq -sce --arg title "$3" --arg body "$4" '
    if length == 1 then .[0] else error("expected one inventory document") end |
    if type != "array" then error("invalid inventory") else . end |
    if all(.[]; type == "object" and (.title | type == "string")
        and has("body") and ((.body | type == "string") or .body == null)
        and (.number | type == "number") and .number > 0 and (.number | floor) == .number
        and (.html_url | type == "string") and (.html_url | startswith("https://"))
        and (.state == "open" or .state == "closed") and (.labels | type == "array"))
    then [.[] | select(has("pull_request") | not) | select(.title == $title and .body == $body)]
    else error("incomplete inventory") end') || exit 3
  matches=$(printf '%s\n%s' "$matches" "$selected" | jq -cs '.[0] + .[1]') || exit 3
  count=$(printf '%s' "$out" | jq -r length) || exit 3
  [ "$count" -ge 100 ] || break
  page=$((page + 1))
done
count=$(printf '%s' "$matches" | jq -r length) || exit 3
case "$count" in
  0) exit 1 ;;
  1) printf '%s' "$matches" | jq -ce '.[0] | {issue_number:.number,issue_url:.html_url}' || exit 3 ;;
  *) exit 3 ;;
esac
