# X RSSHub Integration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Convert public X user profile URLs into RSSHub user-timeline URLs without requiring an X Developer API key.

**Architecture:** Add a Tier 0 URL normalization rule for `x.com` and `twitter.com`. The existing RSS/Atom fetcher and parser will perform the network request and retain post text, media HTML, publication time, and original post links. RSSHub remains the default public endpoint; a later configuration option can replace its base URL.

**Tech Stack:** Rust, `url::Url`, existing blocking RSS/Atom fetcher, feed-rs parser, Docker-based cargo tests.

---

### Task 1: Add failing URL normalization tests

**Files:**
- Modify: `crates/glean-core/src/feed/tier0.rs`

**Steps:**
1. Add tests for `https://x.com/madlaxcb` and `https://twitter.com/madlaxcb`.
2. Assert both normalize to `https://rsshub.app/twitter/user/madlaxcb`.
3. Add tests for trailing slash and unsupported deeper paths remaining unchanged.
4. Run `docker run --rm -v /home/Glean:/work -w /work rust:1.85 cargo test -p glean-core --lib feed::tier0` and confirm the new tests fail before implementation.

### Task 2: Implement X URL normalization

**Files:**
- Modify: `crates/glean-core/src/feed/tier0.rs`

**Steps:**
1. Match hosts `x.com`, `www.x.com`, `twitter.com`, and `www.twitter.com`.
2. Accept exactly one non-empty path segment as the username.
3. Reject login, intent, status, search, hashtag, and multi-segment URLs.
4. Build `https://rsshub.app/twitter/user/{username}` using URL path segment encoding.
5. Run the Tier 0 tests and confirm they pass.

### Task 3: Verify RSS parsing behavior

**Files:**
- Modify: `crates/glean-core/src/feed/tier0.rs` only if edge cases require it.

**Steps:**
1. Test a representative RSSHub fixture containing text, an image, a video link, `pubDate`, and an original X link.
2. Verify the existing parser preserves publication time, HTML media, and original links.
3. Run the full `glean-core` test suite and `cargo fmt --all -- --check`.

### Task 4: Document operational limitations

**Files:**
- Modify: `docs/2026-08-09-x-rsshub.md`

**Steps:**
1. Document the default RSSHub URL transformation.
2. Document that the public instance may rate-limit or disable X routes.
3. Document the future custom-instance configuration without implementing it now.

### Task 5: Commit and push

**Steps:**
1. Run `git diff --check`.
2. Commit the Tier 0 rule and documentation.
3. Push `main` and verify the GitHub Actions run starts.
