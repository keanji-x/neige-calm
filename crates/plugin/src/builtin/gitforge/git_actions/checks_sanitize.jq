def clean:
  tostring
  | gsub("\u001b\\][^\u0007\u001b]*(\u0007|\u001b\\\\)"; "")
  | gsub("\u001b\\[[0-?]*[ -/]*[@-~]"; "")
  | gsub("[\u061c\u180e\u200b-\u200f\u202a-\u202e\u2060-\u206f\ufeff]"; "")
  | gsub("[\u0000-\u001f\u007f-\u009f]"; " ")
  | reduce credential_keys[] as $key
      (. ; $ENV[$key] as $secret | if $secret != null and $secret != "" then split($secret) | join("[REDACTED]") else . end)
  | gsub("(gh[pousr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)"; "[REDACTED]")
  | gsub("(?i)(authorization[ :=]+(bearer|basic)[ ]+[^ ,;]+|bearer[ ]+[^ ,;]+|basic[ ]+[A-Za-z0-9+/=]+)"; "[REDACTED]")
  | gsub("(?i)(authorization|token|password|secret|api[_-]?key)[ :=]+[^ ,;]+"; "[REDACTED]")
  | gsub("https?://[^ /]+@"; "https://[REDACTED]@")
  | gsub("https?://[^ ]*[?][^ ]*"; "[REDACTED URL]")
  | .[0:1000];

def clean_check:
  .name |= clean
  | if has("url") then (.url | clean) as $url
      | if $url == .url then . else .id = (._source.id | clean) | del(.url) end
    else .id |= clean end
  | del(._source);
