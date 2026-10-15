# Shared by the gitforge Actions reads (checks read, its enrichment, CI evidence).
# resolve_repo $1: set host and repo to the trusted gh host and the canonical owner/name of the
# repository selector.
resolve_repo() {
  identity=$(timeout --foreground 3 gh repo view "$1" --json nameWithOwner,url --jq '(.url | split("/")[2]), .nameWithOwner') || return 1
  host=${identity%%
*}
  repo=${identity#*
}
}
# gh api within the caller's budget_end (epoch seconds).
# Status 2: the time budget ran out before or during the call; 1: any other failure.
gh_api() {
  remaining=$((budget_end - $(date +%s)))
  [ "$remaining" -gt 0 ] || return 2
  # Keep the kernel's process group; timeout the direct HTTP client, not a parent shell.
  # Bound the complete selected stream before any caller captures it. The final
  # status marker rejects both a truncated stream and a failed later page.
  response=$({ timeout --foreground "$remaining" gh api "$@"; api_status=$?;
    printf '\ngh-enrich-status:%s\n' "$api_status"; } | head -c 262208) || return 1
  case "${response##*
}" in
    gh-enrich-status:0) ;;
    gh-enrich-status:124) return 2 ;;
    *) return 1 ;;
  esac
  response=${response%
gh-enrich-status:0}
  [ "$(printf '%s' "$response" | wc -c)" -le 262144 ] || return 1
  printf '%s\n' "$response"
}
