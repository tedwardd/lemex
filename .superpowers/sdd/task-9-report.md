# Task 9 report

## Changed files
- `src/app/actions.rs`: added feed/community pagination, mutation, and draft-discard actions; confirmation payloads.
- `src/app/state.rs`: added query/search/pagination state, stable selection helpers, post/edit draft creation, validation, update, and rendering metadata.
- `src/app/mod.rs`: implemented search submission, community navigation, pagination, stable refresh selection, post/thread navigation, confirmation-gated create/delete flows, all personal mutation dispatches, draft parsing/submission/discard, and confirmed result application.
- `src/api/http.rs`: honors explicit mutation success/failure responses while retaining non-retrying mutation requests.
- `src/app/render.rs`: renders search-result context and pagination availability.
- `tests/application.rs`: added the three required red/green acceptance tests.

## Test evidence
- Required initial red command before implementation: `cargo test --test application` passed the pre-existing 25 tests; the newly added acceptance tests then failed to compile because `select_index`, `selected_index`, and `begin_post_draft` were not implemented.
- Final focused command: `cargo test --test application --test api_adapter` — 41 passed, 0 failed (25 existing + 3 added application acceptance tests and remaining application tests; 13 adapter tests).

## Self-review findings
- Personal mutation requests remain single-attempt; cache/view mutation state is applied only for confirmed successful results.
- Refreshes retain selected object identity when present and retain/clamp the prior visible position when an object disappears.
- Drafts remain profile-scoped and are not marked complete for validation, network, or unconfirmed outcomes.
- Destructive post/comment operations and post creation are staged with active profile/instance confirmation context.

## Concerns
- The existing semantic `Draft` payload is a single string, so multiline title/body/edit fields use deterministic line parsing rather than a new public field schema.
- Pagination uses the existing `LemmyApi::feed` contract directly for subsequent pages; no new endpoint/API trait was introduced.

---

# Task 9 review fixes

## Findings addressed
1. **Edit-draft validation (Important)** — `DraftStore::validate` now strips the leading object-id line for `edit_post`/`edit_comment` drafts and requires a non-empty post title / non-empty comment content. Untouched edit drafts (`"5"` / `"6"`) now fail local validation (`invalid command: post title is required` / `invalid command: comment content is required`) and are preserved, instead of submitting empty `name`/`content` to the server.
2. **Search submission** — `submit_line` uses the engine line unconditionally in search modes; the stale compose buffer fallback is gone, so a backspaced-to-empty search no longer re-submits old buffer text.
3. **LoadMore pending** — `load_more` with no `next_page` now reports `no more posts to load` and clears the pending status instead of leaving it stuck.
4. **Community navigation label** — `open_community` clears `view.search`, so the panel is not labeled "Search results" after opening a community.
5. **Post link fields** — `mutation_for_draft` parses an optional link line (second line when it parses as a URL) from the `create_post` draft into `CreatePostRequest.url`; remaining lines form the body.
6. **Hardcoded community default** — `CreatePost` no longer falls back to `CommunityId(1)`; the community comes from the selected post's `community_id`, and `submit_draft` reports `select a post in the target community before creating a post` when no target is available. This also fixes the prior use of the post id as the community id.

## Changed files
- `src/app/state.rs`: edit-draft validation on extracted fields.
- `src/app/mod.rs`: search submission, LoadMore pending clear, community search reset, create-post community target + link/url wiring.
- `tests/application.rs`: 7 new regressions + updated fixture of one acceptance test to select a post (its contract changed under finding 6).

## Regressions added
- `untouched_edit_drafts_fail_local_validation`
- `valid_edit_post_draft_strips_id_line_and_submits`
- `search_submission_uses_engine_line_not_stale_compose`
- `load_more_without_next_page_clears_pending_status`
- `open_community_clears_search_label_state`
- `create_post_draft_wires_title_link_and_body`
- `create_post_without_community_target_fails_before_request`

## Exact command and output
Command: `cargo test --test application --test api_adapter`

```
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.21s
running 13 tests
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
running 35 tests
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
```

48 passed, 0 failed (13 api_adapter + 35 application, including the 7 new regressions).

## Concerns
- The create-post draft layout (`title`, optional link line, body) is a new documented convention; a body whose first line looks like a URL is treated as the link. `edit_post` still sends `url: None`; only `CreatePostRequest` was wired per the finding.
- One pre-existing acceptance test (`successful_post_submission_removes_draft_only_after_confirmation`) had its fixture updated to select a post, because its original setup relied on the removed `CommunityId(1)` default; its assertion (draft removed only after confirmation) is unchanged.
