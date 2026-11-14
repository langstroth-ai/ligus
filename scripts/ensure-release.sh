#!/usr/bin/env bash
# Print the id of the GitHub release for a tag, creating it if there is none.
#
#   scripts/ensure-release.sh <owner/repo> <tag>
#
# Used by release.yml and release-cli.yml so both attach to the same release
# and neither release action ever creates, renames or rewrites it. An existing
# release (published or draft) is reused untouched: its title, notes and
# prerelease/latest state belong to whoever published it. A new release is
# created as a published prerelease with a placeholder title and body, so it
# is not "Latest" (the updater and install.sh keep pointing at the previous
# release) until the owner writes the notes and promotes it:
#
#   gh release edit <tag> --title "Ligus <tag>" --notes-file notes.md \
#     --prerelease=false --latest
#
# Needs GH_TOKEN with contents: write.
set -euo pipefail

repo="${1:?usage: ensure-release.sh <owner/repo> <tag>}"
tag="${2:?usage: ensure-release.sh <owner/repo> <tag>}"
delay="${ENSURE_RELEASE_RETRY_DELAY:-5}"

# The tag reaches gh's jq filter and URLs below; accept only release tags.
if ! [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "ensure-release: refusing malformed tag '$tag'" >&2
  exit 2
fi
if ! [[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "ensure-release: refusing malformed repo '$repo'" >&2
  exit 2
fi

lookup() {
  local id
  # Published releases (incl. prereleases) are found by tag...
  if id=$(gh api "repos/$repo/releases/tags/$tag" --jq .id 2>/dev/null) && [ -n "$id" ]; then
    echo "$id"
    return 0
  fi
  # ...drafts are not, so scan the list for one.
  id=$(gh api --paginate "repos/$repo/releases?per_page=100" \
    --jq ".[] | select(.draft and .tag_name == \"$tag\") | .id" | head -n1) || id=""
  if [ -n "$id" ]; then
    echo "$id"
    return 0
  fi
  return 1
}

for attempt in 1 2 3 4 5; do
  if lookup; then
    exit 0
  fi
  # --verify-tag: never let gh create the tag; the release must sit on the
  # pushed tag. A concurrent creator (the other release workflow) makes this
  # fail with already_exists; the next lookup then finds its release.
  if gh release create "$tag" --repo "$repo" --verify-tag --prerelease \
      --title "Ligus $tag" \
      --notes "Release notes to follow. Installers are attached below." >/dev/null; then
    echo "ensure-release: created prerelease $tag" >&2
  else
    echo "ensure-release: create failed (attempt $attempt), looking again" >&2
  fi
  sleep "$delay"
done

echo "ensure-release: no release for $tag after 5 attempts" >&2
exit 1
