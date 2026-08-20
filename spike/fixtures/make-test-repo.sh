#!/usr/bin/env bash
# Recreates the throwaway repository the spike runs against.
# Task 019 should replace this with a checked-in fixture repo.
set -euo pipefail
R="${1:-/tmp/rimaia-spike/testrepo}"
rm -rf "$R" && mkdir -p "$R/src" && cd "$R"
cat > Cargo.toml <<'TOML'
[package]
name = "spike-testrepo"
version = "0.1.0"
edition = "2021"
TOML
cat > src/lib.rs <<'RS'
pub fn slugify(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_replaces_spaces() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }
}
RS
printf 'target\n' > .gitignore
git init -q -b main && git add -A
git -c user.email=spike@local -c user.name=Spike commit -qm "initial: slugify helper"
echo "test repo ready at $R"
