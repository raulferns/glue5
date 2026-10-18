#!/usr/bin/env bash
# The game's code reaches the Build workflow through a draft release of the
# repository, and the apps built from it go back into the same draft: a
# draft is seen only by those who can write to the repository (and by
# workflows with `contents: write`). Published, it would give the game to
# everyone, so a release with it is put back to draft.
#
#   draft.sh guard [RELEASE]     every published release with the game's code (or only RELEASE) back to draft
#   draft.sh fetch DIR           the newest draft's .wasm, checked, as DIR/b/<build>/game.wasm
#   draft.sh get RELEASE NAME    a file of the draft, if it has it, into the current directory
#   draft.sh upload RELEASE FILE...  files into the draft, replacing those of the same name
#
# As in a workflow: GH_TOKEN, GITHUB_REPOSITORY; fetch writes the draft's id
# to GITHUB_OUTPUT (release=).
set -euo pipefail

repo=${GITHUB_REPOSITORY:?}
root=$(cd "$(dirname "$0")/../.." && pwd)

die() {
    echo "::error::$*" >&2
    exit 1
}

# The files that must stay private: the game's code and what is built from it.
private='.name | endswith(".wasm") or startswith("Glue5-")'

releases() {
    gh api --paginate --slurp "repos/$repo/releases" | jq '[.[][]]'
}

sha256() { { sha256sum "$1" 2>/dev/null || shasum -a 256 "$1"; } | cut -d' ' -f1; }

guard() {
    local ids id release name failed=0
    if [ -n "${1:-}" ]; then
        ids=$1
    else
        ids=$(releases | jq -r ".[] | select(.draft | not) | select(any(.assets[]; $private)) | .id")
    fi
    for id in $ids; do
        # Its state now, not the event's.
        release=$(gh api "repos/$repo/releases/$id")
        [ "$(jq -r .draft <<<"$release")" = false ] || continue
        jq -e "any(.assets[]; $private)" <<<"$release" >/dev/null || continue
        name=$(jq -r '.name // .tag_name' <<<"$release")
        failed=1
        if gh api -X PATCH "repos/$repo/releases/$id" -F draft=true >/dev/null; then
            echo "::error::Release \"$name\" was published with the game's code or an app built from it: it is a draft again. Keep it a draft: published, anyone can download its files."
            continue
        fi
        # An immutable release cannot go back to draft: its files go, or the
        # release itself.
        echo "::error::Release \"$name\" could not go back to draft: deleting its private files"
        jq -r ".assets[] | select($private) | .id" <<<"$release" | while read -r asset; do
            gh api -X DELETE "repos/$repo/releases/assets/$asset" ||
                { gh api -X DELETE "repos/$repo/releases/$id" && break; }
        done
    done
    return $failed
}

fetch() {
    local dir=$1 release id wasm asset sum build
    guard || die "a published release had the game's code; see above"
    release=$(releases | jq "[.[] | select(.draft) | select(any(.assets[]; .name | endswith(\".wasm\")))]
        | sort_by(.created_at) | last // empty")
    [ -n "$release" ] || die "No draft release with game.wasm. Releases > Draft a new release ($GITHUB_SERVER_URL/$repo/releases/new): attach game.wasm and press \"Save draft\" (not \"Publish release\")."
    id=$(jq -r .id <<<"$release")
    wasm=$(jq -r '[.assets[] | select(.name | endswith(".wasm"))] | if length == 1 then .[0].id else empty end' <<<"$release")
    [ -n "$wasm" ] || die "The draft \"$(jq -r .name <<<"$release")\" has several .wasm files: keep one."

    mkdir -p "$dir"
    curl -fsSL -H "Authorization: Bearer $GH_TOKEN" -H "Accept: application/octet-stream" \
        -o "$dir/game.wasm" "$GITHUB_API_URL/repos/$repo/releases/assets/$wasm"
    sum=$(sha256 "$dir/game.wasm")
    # The supported builds, as the game's build script lists them.
    build=$(grep -o '("[0-9a-z]*", "[0-9a-f]\{64\}")' "$root/crates/glue5-game/build/main.rs" |
        tr -d '(",)' | awk -v sum="$sum" '$2 == sum { print $1 }')
    [ -n "$build" ] || die "game.wasm (sha256 $sum) is not a supported build of the game."
    mkdir -p "$dir/b/$build"
    mv "$dir/game.wasm" "$dir/b/$build/game.wasm"
    echo "game.wasm: build $build, from the draft \"$(jq -r .name <<<"$release")\""
    echo "release=$id" >>"${GITHUB_OUTPUT:-/dev/null}"
}

# The draft RELEASE, or fail if it is not one any more.
draft() {
    local release
    release=$(gh api "repos/$repo/releases/$1")
    if [ "$(jq -r .draft <<<"$release")" != true ]; then
        guard "$1" || true
        die "The release is not a draft any more: nothing goes into it."
    fi
    printf '%s' "$release"
}

get() {
    local asset
    asset=$(draft "$1" | jq -r --arg name "$2" '.assets[] | select(.name == $name) | .id')
    [ -n "$asset" ] || return 0
    curl -fsSL -H "Authorization: Bearer $GH_TOKEN" -H "Accept: application/octet-stream" \
        -o "$2" "$GITHUB_API_URL/repos/$repo/releases/assets/$asset"
}

upload() {
    local id=$1 release url file name old
    shift
    for file; do
        release=$(draft "$id")
        name=${file##*/}
        old=$(jq -r --arg name "$name" '.assets[] | select(.name == $name) | .id' <<<"$release")
        [ -z "$old" ] || gh api -X DELETE "repos/$repo/releases/assets/$old"
        url=$(jq -r '.upload_url | sub("\\{.*"; "")' <<<"$release")
        curl -fsS -X POST -H "Authorization: Bearer $GH_TOKEN" -H "Content-Type: application/octet-stream" \
            --data-binary @"$file" "$url?name=$name&label=$name%20(${commit:0:7})" >/dev/null
        echo "uploaded $name to the draft \"$(jq -r .name <<<"$release")\""
    done
}

: "${GITHUB_API_URL:=https://api.github.com}" "${GITHUB_SERVER_URL:=https://github.com}"
commit=${GITHUB_SHA:-$(git -C "$root" rev-parse HEAD)}
command=${1:-}
shift || true
case $command in
guard | fetch | get | upload) "$command" "$@" ;;
*) die "usage: draft.sh guard [RELEASE] | fetch DIR | get RELEASE NAME | upload RELEASE FILE..." ;;
esac
