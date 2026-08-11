# Medium and GitLab Feed Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add local URL normalization for Medium RSS and GitLab project Releases Atom feeds without introducing credentials or third-party services.

**Architecture:** Extend the existing Tier 0 normalizer in `feed/tier0.rs`. Medium profile, publication, and topic URLs become their documented `/feed/...` counterparts; GitLab project roots become `/-/releases.atom`, while already-normalized feeds and deeper paths remain unchanged. Keep all behavior local and deterministic so the existing synchronous and background subscription flows share it automatically.

**Tech Stack:** Rust 2021, `url` crate, existing Tier 0 unit-test structure, Docker Rust toolchain.

---

### Task 1: Add Medium URL normalization

**Files:**
- Modify: `crates/glean-core/src/feed/tier0.rs`

**Step 1: Write the failing tests**

Add tests for:

- `https://medium.com/@user` → `https://medium.com/feed/@user`
- `https://medium.com/publication` → `https://medium.com/feed/publication`
- `https://medium.com/tag/rust` → `https://medium.com/feed/tag/rust`
- an already normalized `/feed/...` URL remaining unchanged
- deeper story URLs remaining unchanged

**Step 2: Run the tests**

Run:

```bash
docker run --rm -v /home/Glean:/workspace -w /workspace rust:1.85 bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo test -p glean-core --lib feed::tier0::tests::medium'
```

Expected: the new tests fail because Medium is not yet handled.

**Step 3: Implement the minimal rule**

Add a `medium.com` route in `normalize`, then normalize only supported profile, publication, and topic paths. Do not rewrite article paths or already-prefixed `/feed` paths. Preserve HTTP/HTTPS and the original `www` form consistently with existing rules.

**Step 4: Run the focused tests**

Run the same Docker test command and expect all Medium tests to pass.

### Task 2: Add GitLab Releases normalization

**Files:**
- Modify: `crates/glean-core/src/feed/tier0.rs`

**Step 1: Write the failing tests**

Add tests for:

- `https://gitlab.com/group/project` → `https://gitlab.com/group/project/-/releases.atom`
- nested namespaces such as `https://gitlab.com/group/subgroup/project`
- a self-managed GitLab host using the same project-root rule
- an existing `/-/releases.atom` URL remaining unchanged
- issue, repository, and other deeper paths remaining unchanged

**Step 2: Run the tests**

Run:

```bash
docker run --rm -v /home/Glean:/workspace -w /workspace rust:1.85 bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo test -p glean-core --lib feed::tier0::tests::gitlab'
```

Expected: the new tests fail because GitLab is not yet handled.

**Step 3: Implement the minimal rule**

Match GitLab hosts by path shape rather than hard-coding only `gitlab.com`, because self-managed instances use arbitrary hosts. Append `/-/releases.atom` only when the path does not already contain a known GitLab action segment and has at least two non-empty path segments.

**Step 4: Run the focused tests**

Run the same Docker test command and expect all GitLab tests to pass.

### Task 3: Full verification

**Files:**
- No additional files.

**Step 1: Format and test**

Run:

```bash
docker run --rm -v /home/Glean:/workspace -w /workspace rust:1.85 bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo fmt --all -- --check && cargo test -p glean-core --lib && cargo check -p glean-app && git diff --check'
```

Expected: formatting, core tests, application compilation, and diff checks pass.

**Step 2: Review the diff**

Confirm only the Tier 0 implementation and its tests changed, with no credentials, network calls, or unrelated refactors.

**Step 3: Commit and push**

```bash
git add crates/glean-core/src/feed/tier0.rs
git commit -m "feat: add Medium and GitLab feed normalization"
git push
```

