project := "kojutsu"

export RUST_BACKTRACE := "1"
export RUST_LOG := project + "=trace"

default:
    just --list


dev *args:
    cargo run -- {{args}}

release *args:
    cargo run --release -- {{args}}

test *args:
    cargo nextest run -- {{args}}

# Assert our shortest unique ID prefixes match what `jj log` prints. They must:
# prefixes too long are noise, prefixes too short are rejected by `jj` as
# ambiguous when we pass them back as revision arguments.
check-prefixes revset='':
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    if [ -n '{{revset}}' ]; then args=(-r '{{revset}}'); fi
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    cargo run --quiet -- --debug-prefixes "${args[@]}" | sort > "$tmp/kojutsu"
    jj --ignore-working-copy log --no-graph "${args[@]}" \
        -T 'change_id.shortest().prefix() ++ "|" ++ commit_id.shortest().prefix() ++ "\n"' \
        | sort > "$tmp/jj"
    diff -u --label jj "$tmp/jj" --label kojutsu "$tmp/kojutsu"
    echo "prefixes match jj ($(wc -l < "$tmp/jj") commits)"

fix:
    cargo fix -p {{project}} --allow-dirty --allow-staged
    cargo clippy --fix -p {{project}} --allow-dirty --allow-staged


fmt:
    cargo fmt

flamegraph:
    CARGO_PROFILE_RELEASE_DEBUG=true cargo flamegraph

udeps:
    cargo +nightly udeps
