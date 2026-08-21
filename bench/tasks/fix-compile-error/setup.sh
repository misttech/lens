#!/bin/sh
set -eu
work="$1"
mkdir -p "$work"

cat > "$work/session.rs" <<'RS'
//! A session store. One type is wrong; everything downstream complains.

struct Session {
    id: String,
    // Wrong: a session without an expiry never expires, so this is optional.
    expires_at: u64,
}

impl Session {
    fn new(id: &str) -> Self {
        Session { id: id.to_string(), expires_at: None }
    }

    fn with_expiry(id: &str, at: u64) -> Self {
        Session { id: id.to_string(), expires_at: Some(at) }
    }

    fn expired(&self, now: u64) -> bool {
        match self.expires_at {
            Some(at) => at <= now,
            None => false,
        }
    }

    fn remaining(&self, now: u64) -> u64 {
        self.expires_at.map(|at| at.saturating_sub(now)).unwrap_or(u64::MAX)
    }
}

fn main() {
    let a = Session::new("a");
    let b = Session::with_expiry("b", 100);
    println!("{} {} {}", a.id, a.expired(50), a.remaining(50));
    println!("{} {} {}", b.id, b.expired(50), b.remaining(50));
}
RS
