#!/bin/sh
set -eu
state=$(dirname "$0")/state
get_arg() {
  key=$1; shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "$key" ]; then printf '%s' "$2"; return; fi
    shift
  done
  return 1
}
block() {
  name=$1
  [ -f "$state/block_$name" ] || return 0
  touch "$state/${name}_started"
  while [ ! -f "$state/release_$name" ]; do sleep 0.02; done
}
if [ "$1" = api ]; then
  method=$(get_arg --method "$@")
  if [ "$method" = POST ]; then
    block issue_create_before
    input=$(cat)
    count=$(cat "$state/issue_create_count" 2>/dev/null || printf 0)
    count=$((count + 1))
    printf '%s' "$input" | jq --argjson number 731 --arg url 'https://github.com/owner/repo/issues/731' '{number:$number,html_url:$url,state:"open",title:.title,body:.body,labels:[]}' > "$state/created.json"
    printf '%s' "$count" > "$state/issue_create_count"
    block issue_create
    cat "$state/created.json"
  else
    page=1
    for arg in "$@"; do case "$arg" in page=*) page=${arg#page=} ;; esac; done
    count=$(cat "$state/probe_count" 2>/dev/null || printf 0)
    count=$((count + 1))
    printf '%s' "$count" > "$state/probe_count"
    [ ! -f "$state/outage" ] || exit 1
    [ ! -f "$state/page${page}.fail" ] || exit 1
    if [ -f "$state/documents" ]; then printf '[]\n[]'; exit 0; fi
    if [ -f "$state/output.fail" ] && [ "$count" -gt 2 ]; then exit 1; fi
    if [ -f "$state/invalid" ]; then printf broken; exit 0; fi
    if [ -f "$state/page${page}.json" ]; then cat "$state/page${page}.json"; else printf '[]'; fi
  fi
elif [ "$1:$2" = issue:list ]; then
  [ "$(get_arg --json "$@")" = number,url,state,title,body,labels ] || exit 2
  query=$(get_arg --search "$@")
  wanted=$(get_arg --state "$@")
  limit=$(get_arg --limit "$@")
  cat "$state/search.json" | jq --arg query "$query" --arg state "$wanted" --argjson limit "$limit" '[.[] | select((.title | contains($query)) or (.body | contains($query))) | select($state == "all" or (.state | ascii_downcase) == $state)][: $limit]'
elif [ "$1:$2" = issue:view ]; then
  case "$(get_arg --json "$@")" in
    number,url,state,title,body,labels) cat "$state/view.json" ;;
    body) jq -r .body "$state/view.json" ;;
    *) exit 2 ;;
  esac
else
  printf 'unsupported external API: %s\n' "$*" >&2
  exit 2
fi
