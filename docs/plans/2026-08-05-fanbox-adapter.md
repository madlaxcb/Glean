# Fanbox Adapter Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a minimal Tier 2 Fanbox creator-content adapter that only collects content visible to the user's authenticated session.

**Architecture:** Add a self-contained `manifest.toml` and `adapter.rhai` under `plugins/fanbox/`, following the existing Pixiv/Bilibili plugin contract. Keep Rust runtime changes out of scope; validate script compilation, URL routing, credential injection, pagination, field mapping, error handling, and permission boundaries with fixture-backed tests plus one ignored live-network test.

**Tech Stack:** Rust workspace tests, `PluginManager`, Rhai Tier 2 runtime, TOML manifests, JSON response fixtures, reqwest-compatible host HTTP functions.

---

### Task 1: Define the Fanbox plugin contract

**Files:**
- Create: `plugins/fanbox/manifest.toml`
- Reference: `plugins/pixiv/manifest.toml`
- Reference: `crates/glean-core/src/plugin/manifest.rs`

**Step 1: Write the manifest**

Declare plugin id `fanbox`, a creator-content name, version `0.1.0`, Tier 2, minimum Glean version matching the existing official plugins, and URL patterns for `fanbox.cc/@<creator>` and `fanbox.cc/<creator>` creator pages.

Declare only the Fanbox API host required by the adapter and one credential slot named `fanbox_session`. Set `uses_user_session = true`. Do not declare unrelated external-call or content-transform capabilities.

**Step 2: Validate the manifest shape**

Run:

```bash
docker run --rm -v /home/Glean:/workspace -w /workspace rust:1.85 bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo test -p glean-core plugin::manifest::tests -- --nocapture'
```

Expected: existing manifest tests pass.

**Step 3: Commit the contract**

```bash
git add plugins/fanbox/manifest.toml
git commit -m "feat: define Fanbox adapter manifest"
```

### Task 2: Implement safe response and date helpers

**Files:**
- Create: `plugins/fanbox/adapter.rhai`
- Reference: `plugins/pixiv/adapter.rhai:15-112`

**Step 1: Add helper functions**

Implement only the helpers needed by the fixture schema:

- safe string and integer conversion;
- bounded API error extraction without logging credentials or response secrets;
- ISO 8601 timestamp parsing into Unix seconds, reusing the proven Pixiv conversion pattern;
- stable creator and post URL construction with validated IDs.

**Step 2: Add credential-safe request setup**

Use the Host-injected `{{fanbox_session}}` placeholder only in the request header/body mechanism already supported by the runtime. The script must never read or log the credential value. If the credential is missing, fail with a clear message before making content requests.

**Step 3: Compile the script through the existing runtime**

Add or extend a test that loads the manifest and adapter using `include_str!`, then runs the Rhai compilation/evaluation path without network access.

Expected: the script compiles and missing credentials produce a controlled error.

### Task 3: Implement the minimal creator feed flow

**Files:**
- Modify: `plugins/fanbox/adapter.rhai`
- Reference: `plugins/pixiv/adapter.rhai:114-357`
- Reference: `crates/glean-core/src/plugin/runtime.rs:100-150`

**Step 1: Parse the source URL**

Accept one creator identifier from the matched source URL. Reject unsupported paths rather than guessing a creator.

**Step 2: Fetch the authenticated creator content list**

Call the documented Fanbox endpoint represented by the fixture contract, include the injected session credential, and validate HTTP status and response shape. Do not attempt paywall bypass or access escalation.

**Step 3: Implement bounded sequential pagination**

Follow the returned cursor or next-page token in order, stop at the configured first-release page cap, and stop early when the page is exhausted. Use `EXISTING_GUIDS` only for safe early termination after at least one page has been validated.

**Step 4: Map visible posts**

For each visible post, emit:

- stable GUID from the platform post id;
- title;
- creator author;
- post URL;
- published timestamp;
- sanitized content HTML or bounded summary;
- cover/thumbnail image URL when present.

Skip malformed individual posts with a bounded warning and continue; fail the feed when the top-level response is invalid, unauthorized, rate-limited after retries, or structurally incompatible.

**Step 5: Compile and exercise with a fixture**

Run the focused Fanbox test and expect a non-empty parsed feed, stable GUIDs, populated URLs, and timestamps.

### Task 4: Add fixture-backed regression tests

**Files:**
- Modify: `crates/glean-core/src/plugin/manager.rs`
- Create: `crates/glean-core/src/plugin/fixtures/fanbox_posts_page_1.json`
- Create: `crates/glean-core/src/plugin/fixtures/fanbox_posts_page_2.json`
- Reference: existing ignored `bilibili_end_to_end` test in `manager.rs`

**Step 1: Add manifest and script loading coverage**

Load `plugins/fanbox/manifest.toml` and `plugins/fanbox/adapter.rhai` from repository fixtures and assert that the manager routes a Fanbox creator URL to the plugin.

**Step 2: Add field mapping coverage**

Use deterministic fixture responses or the existing test HTTP seam to verify pagination, title, GUID, URL, author, timestamp, and content/image mapping.

**Step 3: Add failure coverage**

Cover:

- missing `fanbox_session`;
- unauthorized response;
- rate-limit response;
- malformed top-level JSON;
- malformed individual post;
- no permission to view a post.

Assert errors are explicit and no credential value appears in the error string or debug log.

**Step 4: Run focused tests**

```bash
docker run --rm -v /home/Glean:/workspace -w /workspace rust:1.85 bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo test -p glean-core plugin::manager::tests -- --nocapture'
```

Expected: all manager tests pass.

### Task 5: Add ignored live-network verification

**Files:**
- Modify: `crates/glean-core/src/plugin/manager.rs`
- Modify: `docs/1.0.0-发布检查清单.md`

**Step 1: Add the opt-in test**

Add an ignored test requiring an explicitly provided creator URL and session credential through environment variables. Never place the credential in source, fixtures, command output, or assertions.

**Step 2: Validate the live result**

Run only when the user has configured a valid session locally:

```bash
GLEAN_FANBOX_URL='https://fanbox.cc/@creator' GLEAN_FANBOX_SESSION='configured-locally' cargo test -p glean-core -- --ignored fanbox_end_to_end
```

Expected: the test confirms a visible post is returned, stable GUID and URL are present, and no unauthorized content is collected.

**Step 3: Record only non-secret results**

Update the release checklist with the test date, environment, and pass/fail result, excluding the creator session and any private content.

### Task 6: Verify, review, commit, and push

**Files:**
- Verify: all changed files

**Step 1: Run the full Docker verification**

```bash
docker run --rm -v /home/Glean:/workspace -w /workspace rust:1.85 bash -lc 'export PATH=/usr/local/cargo/bin:$PATH; cargo fmt --all -- --check && cargo check -p glean-app && cargo test -p glean-core'
```

Expected: formatting, application check, and all core tests pass.

**Step 2: Check the diff for secrets and unrelated changes**

```bash
git diff --check
git status --short
git diff -- plugins/fanbox crates/glean-core/src/plugin docs/1.0.0-发布检查清单.md
```

Confirm no session value, Cookie, API key, or private response body is committed.

**Step 3: Commit and push**

```bash
git add plugins/fanbox crates/glean-core/src/plugin docs/1.0.0-发布检查清单.md
git commit -m "feat: add Fanbox Tier 2 adapter"
git push origin main
```

**Step 4: Confirm GitHub Actions**

```bash
gh run list --repo madlaxcb/Glean --limit 3
```

Expected: the Windows workflow is triggered for the pushed commit.
