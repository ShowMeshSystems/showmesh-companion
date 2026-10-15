# Releasing ShowMesh Companion

## Versioning

The companion follows the versioning scheme in ShowMesh Core's
[`docs/RELEASING.md`](https://github.com/ShowMeshSystems/showmesh/blob/main/docs/RELEASING.md):
`MAJOR.MINOR.PATCH`, MAJOR names the show season (`0.x` is 2026, `1.x` is
2027), the `-alpha.N`, `-beta.N` and `-rc.N` suffixes, the January start of
the next season's line, and the compatibility rule that starts at `1.0.0`.
Those rules live in that one file; this one covers only what differs here.

- The companion's MINOR and PATCH move independently of Core's. A companion
  and a coordinator from the same season share a major number.
- The version changes only in the pull request that cuts a release.
- Each release's notes name the oldest ShowMesh Core version it needs, when
  it needs one.

## Cutting a release

1. Decide the version number per the scheme above.
2. Set it in all five places: `package.json`, `package-lock.json` (the two
   top-level `version` fields), `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`
   (the `showmesh-companion` entry), and `src-tauri/tauri.conf.json`. Edit
   `package-lock.json` by hand; `npm install` rewrites more than the version.
3. Write `docs/release-notes/v<VERSION>.md`. It becomes the GitHub release
   body.
4. Get the change merged to `main` through a pull request, with the macOS CI
   job passing.
5. Tag the merge commit on `main` as `v<VERSION>` and push the tag.

The tag push runs `.github/workflows/release.yml`, which checks that the tag
matches `tauri.conf.json`, runs the tests, builds a universal macOS DMG, and
publishes it as an unsigned GitHub prerelease. A mismatched tag fails the run
before anything is published.
