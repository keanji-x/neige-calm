//! Operator configuration and secret snapshot. Errors never include supplied values.
use super::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, fs::OpenOptions, io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub page_messages: usize,
    pub max_response_bytes: usize,
    pub max_result_bytes: usize,
    pub max_parts: usize,
    pub max_part_bytes: usize,
    pub request_timeout_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    sources: Vec<SourceConfig>,
    credentials: BTreeMap<String, Credential>,
    limits: Limits,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceConfig {
    source_id: String,
    base_url: String,
    credential_ref: String,
    directories: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    username: String,
    password_file: String,
}
pub struct Source {
    pub id: String,
    pub base: url::Url,
    pub directories: Vec<String>,
    pub authorization: String,
}
pub struct Snapshot {
    pub sources: BTreeMap<String, Source>,
    pub limits: Limits,
}
pub fn string(s: &str) -> bool {
    !s.trim().is_empty() && !s.chars().any(char::is_control)
}
fn path(s: &str) -> bool {
    string(s) && Path::new(s).is_absolute()
}
fn read(path: &str, cap: usize, secret: bool) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| "config: unreadable file")?;
    let meta = file.metadata().map_err(|_| "config: file metadata")?;
    ensure(meta.is_file(), "config: file type")?;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: getuid has no arguments or preconditions.
        ensure(
            meta.uid() == unsafe { libc::getuid() } && meta.mode() & 0o077 == 0,
            "config: credential permissions",
        )?;
    }
    #[cfg(not(unix))]
    ensure(!secret, "config: unsupported credential permissions")?;
    ensure(meta.len() <= cap as u64, "config: file size")?;
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "config: unreadable file")?;
    ensure(!bytes.is_empty() && bytes.len() <= cap, "config: file size")?;
    Ok(bytes)
}
// Reject duplicate keys recursively before typed decoding (including credential map keys).
pub fn decode(bytes: &[u8]) -> Result<Value> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("unique JSON")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut m = serde_json::Map::new();
                    while let Some((k, Unique(v))) = a.next_entry::<String, Unique>()? {
                        if m.insert(k, v).is_some() {
                            return Err(serde::de::Error::custom("duplicate key"));
                        }
                    }
                    Ok(Unique(Value::Object(m)))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut v = Vec::new();
                    while let Some(Unique(x)) = a.next_element()? {
                        v.push(x);
                    }
                    Ok(Unique(Value::Array(v)))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    v: bool,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Unique(n.into()))
                        .ok_or_else(|| serde::de::Error::custom("nonfinite"))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    serde_json::from_slice::<Unique>(bytes)
        .map(|u| u.0)
        .map_err(|_| "json: invalid or duplicate key".into())
}
// Basic encoding is local to the credential boundary; no environment credentials.
fn basic(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::from("Basic ");
    for c in bytes.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((c.get(1).copied().unwrap_or(0) as u32) << 8)
            | c.get(2).copied().unwrap_or(0) as u32;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
impl Snapshot {
    pub fn load(values: Value) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Values {
            history_config_path: String,
        }
        let v: Values =
            serde_json::from_value(values).map_err(|_| "config: history_config_path")?;
        ensure(path(&v.history_config_path), "config: history_config_path")?;
        let c: Config =
            serde_json::from_value(decode(&read(&v.history_config_path, 65536, false)?)?)
                .map_err(|_| "config: fields or types")?;
        let l = &c.limits;
        for (v, cap) in [
            (l.page_messages, 50),
            (l.max_response_bytes, 2097152),
            (l.max_result_bytes, 131072),
            (l.max_parts, 128),
            (l.max_part_bytes, 16384),
        ] {
            ensure((1..=cap).contains(&v), "config: limits")?;
        }
        ensure(
            (1..=10000).contains(&l.request_timeout_ms),
            "config: limits",
        )?;
        ensure((1..=16).contains(&c.sources.len()), "config: sources")?;
        for (id, cred) in &c.credentials {
            ensure(
                string(id)
                    && string(&cred.username)
                    && !cred.username.contains(':')
                    && path(&cred.password_file),
                "config: credential fields",
            )?;
        }
        let mut sources = BTreeMap::new();
        let mut passwords: BTreeMap<String, String> = BTreeMap::new();
        for s in c.sources {
            ensure(
                string(&s.source_id) && !sources.contains_key(&s.source_id),
                "config: source_id",
            )?;
            ensure(string(&s.credential_ref), "config: credential_ref")?;
            let cred = c
                .credentials
                .get(&s.credential_ref)
                .ok_or("config: credential_ref")?;
            ensure(
                (1..=64).contains(&s.directories.len())
                    && s.directories.iter().all(|d| path(d))
                    && s.directories
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == s.directories.len(),
                "config: directories",
            )?;
            let base = url::Url::parse(&s.base_url).map_err(|_| "config: base_url")?;
            ensure(
                string(&s.base_url)
                    && matches!(base.scheme(), "http" | "https")
                    && base.host_str().is_some()
                    && base.port() != Some(0)
                    && base.username().is_empty()
                    && base.password().is_none()
                    && base.path() == "/"
                    && s.base_url
                        .split_once("://")
                        .is_some_and(|(_, a)| !a.contains('/'))
                    && base.query().is_none()
                    && base.fragment().is_none()
                    && !s.base_url.ends_with('/')
                    && !s.base_url.contains(['?', '#', '\\'])
                    && !s.base_url.chars().any(char::is_whitespace),
                "config: base_url",
            )?;
            let authorization = if let Some(p) = passwords.get(&s.credential_ref) {
                p.clone()
            } else {
                let password = String::from_utf8(read(&cred.password_file, 4096, true)?)
                    .map_err(|_| "config: password encoding")?;
                ensure(!password.contains('\0'), "config: password encoding")?;
                let auth = basic(format!("{}:{password}", cred.username).as_bytes());
                passwords.insert(s.credential_ref.clone(), auth.clone());
                auth
            };
            sources.insert(
                s.source_id.clone(),
                Source {
                    id: s.source_id,
                    base,
                    directories: s.directories,
                    authorization,
                },
            );
        }
        Ok(Self {
            sources,
            limits: c.limits,
        })
    }
}
