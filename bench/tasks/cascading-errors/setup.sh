#!/bin/sh
set -eu
work="$1"
mkdir -p "$work"

# One wrong field type, and every use of it downstream complains. The point is
# the ratio: 150-odd diagnostics, all of them equally error-shaped, and one
# edit that removes every one.
{
  cat <<'HEAD'
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

HEAD

  # Every tenant is a call site that fails for the same reason. A filter that
  # keeps "the errors" keeps 150 copies of the same one.
  i=0
  while [ $i -lt 150 ]; do
    if [ $(( i % 7 )) -eq 0 ]; then
      printf 'fn tenant_%s() -> Session {\n    Session { id: "t%s".to_string(), expires_at: None }\n}\n\n' "$i" "$i"
    else
      printf 'fn tenant_%s() -> Session {\n    Session { id: "t%s".to_string(), expires_at: Some(%s) }\n}\n\n' "$i" "$i" "$(( i * 10 + 100 ))"
    fi
    i=$((i + 1))
  done

  # Referenced, so the fixed program has no dead code and its confirming build
  # is quiet. A hundred and fifty warnings on success would be a second haystack.
  printf 'fn audit(now: u64) -> usize {\n    let tenants: [fn() -> Session; 150] = [\n'
  i=0
  while [ $i -lt 150 ]; do
    printf '        tenant_%s,\n' "$i"
    i=$((i + 1))
  done
  printf '    ];\n    tenants.iter().filter(|make| make().expired(now)).count()\n}\n\n'

  cat <<'TAIL'
fn main() {
    let a = Session::new("a");
    let b = Session::with_expiry("b", 100);
    println!("{} {} {}", a.id, a.expired(50), a.remaining(50));
    println!("{} {} {}", b.id, b.expired(50), b.remaining(50));
    println!("expired {}", audit(50));
}
TAIL
} > "$work/session.rs"
