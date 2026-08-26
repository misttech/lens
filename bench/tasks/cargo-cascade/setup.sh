#!/bin/sh
set -eu
work="${1:?usage: setup.sh <dir>}"
mkdir -p "$work/src"

cat > "$work/Cargo.toml" <<'TOML'
[package]
name = "sessions"
version = "0.1.0"
edition = "2021"

[dependencies]
TOML

{
  printf '//! A session store. One field has the wrong type.\n\n'
  printf 'pub struct Session {\n    pub id: String,\n    // Wrong: a session without an expiry never expires, so this is optional.\n    pub expires_at: u64,\n}\n\n'
  printf 'impl Session {\n    pub fn expired(&self, now: u64) -> bool {\n        match self.expires_at {\n            Some(at) => at <= now,\n            None => false,\n        }\n    }\n}\n\n'
  i=0
  while [ $i -lt 150 ]; do
    printf 'pub fn tenant_%s() -> Session {\n    Session { id: "t%s".to_string(), expires_at: Some(%s) }\n}\n\n' "$i" "$i" "$(( i * 10 + 100 ))"
    i=$((i + 1))
  done
  printf '#[cfg(test)]\nmod tests {\n    use super::*;\n\n'
  printf '    #[test]\n    fn a_session_with_no_expiry_never_expires() {\n        let session = Session { id: "a".to_string(), expires_at: None };\n        assert!(!session.expired(9_999));\n    }\n\n'
  printf '    #[test]\n    fn an_expiry_in_the_past_has_expired() {\n        assert!(tenant_0().expired(9_999));\n    }\n}\n'
} > "$work/src/lib.rs"
