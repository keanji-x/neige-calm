# Retire the legacy Web frontend

The repository-root `web/` application is retired at the owner's request. The
maintained `fe/` application, API, database contracts and released migrations
remain. New builds and source releases must contain only the maintained frontend;
`/calm/` must no longer serve an old bundle even when a deployment retains old
static files. The release manifest's Web unit and `web/dist/next` artifact layout
remain stable because the updater uses them independently of the source tree.

Remove legacy source, build/install steps, Docker mounts and CI jobs. Keep API
code generation in `fe/`. Replace the markdown migration comparison against old
implementations with captured regression expectations before deleting the old
source. Preserve pre-existing local changes outside this retirement; a copy of
legacy source including its uncommitted changes is saved outside the repository
at `/tmp/neige-web-retirement/web-before-retirement.tar.gz`.

Acceptance: maintained frontend lint/build/tests, focused package/source and
frontend-routing tests, API regeneration, relevant script gates, and two isolated
reviews. Existing 4040 service removal is separately blocked on administrator
authentication; this source change must not claim to have stopped that service.
