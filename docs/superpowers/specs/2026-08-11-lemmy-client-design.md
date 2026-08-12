# Lemmy Command-Line Client

## Product specification

**Status:** Approved
**Date:** 2026-08-11
**Implementation baseline:** Rust with ratatui
**Initial platform:** Linux, with a portable core

## 1. Product intent

Build a command-line Lemmy client whose defining feature is a focused, modal, Vim-like user experience. The client must make ordinary Lemmy reading and personal social interaction efficient without requiring a mouse, while preserving the terminal’s strengths: keyboard control, composability, clear status, and fast navigation.

The client supports multiple independent Lemmy instance/account profiles and lets the user switch between them without restarting. It is online-first, with a local cache for recently viewed data and drafts for resilience. Multimedia can be opened through configured MIME handlers, optionally rendered inline through Kitty graphics, or downloaded to disk and revisited through a current-session download history.

This specification is the contract for implementation. New work must either satisfy a requirement here, refine an explicitly ambiguous requirement, or be separately approved as scope.

## 2. Goals

1. Provide a full-screen terminal client with a coherent Vim-like interaction model.
2. Support anonymous browsing and authenticated personal use across multiple Lemmy instances.
3. Make profile switching a first-class in-application operation.
4. Cover the expected personal Lemmy workflow: reading, searching, voting, saving, subscribing, posting, commenting, editing, and deleting within server permissions.
5. Keep API-version details behind a Lemmy adapter so server evolution does not leak into UI code.
6. Handle media safely and predictably through mailcap by default, optional Kitty rendering, external fallback, and explicit downloads.
7. Preserve drafts and terminal state across recoverable failures.
8. Provide a testable architecture with observable contracts at the input, application, adapter, storage, and media boundaries.

## 3. Non-goals for the personal-client release

The following are not part of the initial personal-client target:

- moderator queues, reports, bans, removals, or community administration;
- site administration and federation administration;
- full Vim compatibility or a complete Ex implementation;
- persistent download history across application launches;
- offline mutation queueing and conflict resolution;
- silent retries of non-idempotent mutations;
- a browser-like embedded HTML engine;
- automatic uploading or local-file publication without an explicit user action.

These may be considered after the personal interaction surface is complete, but implementation must not assume they are required for the first release.

## 4. User and platform assumptions

- Linux terminal behavior, mailcap integration, and Kitty support are first-class.
- The core domain, application, and HTTP layers should remain portable enough for later macOS support.
- The primary user is comfortable with Vim concepts and keyboard-driven terminal applications.
- The application starts as a full-screen TUI. A non-interactive CLI may be added later for diagnostics or scripting but is not required by this specification.
- The client communicates with Lemmy through its HTTP API and must normalize API-specific details into stable internal domain types.

## 5. Interaction model

### 5.1 Modes

The client uses explicit modal input:

- **Normal mode:** navigate lists and timelines, open items, move between panes, invoke actions, switch profiles, manage marks, and quit.
- **Insert mode:** compose posts, comments, edits, searches, commands, and profile values.
- **Visual mode:** select text or list ranges where selection has a meaningful action.
- **Command-line mode:** enter `:` commands for navigation, mutation, refresh, profile management, configuration, help, and quitting.
- **Search mode:** use `/` for forward search and `?` for backward search within the active view.

The input engine owns mode transitions, counts, pending operators, mappings, registers, marks, and search state. It produces semantic application commands and has no Lemmy or widget-specific knowledge.

### 5.2 Vim depth

The client implements a focused modal Vim core rather than claiming full Vim compatibility. It includes:

- normal/insert/visual modes;
- `:` command line and `/`/`?` search;
- common motions and counts for list and text navigation;
- a focused set of repeatable operators where they improve navigation or editing;
- configurable key mappings;
- registers and marks only where they have a clear client-level use;
- contextual help and command discoverability.

The command language must be internally consistent. Exact defaults may evolve during implementation, but every default command and mapping must be documented and configurable.

### 5.3 Content-first layout

The default full-screen layout contains:

- a primary feed, community, search, post, or thread view;
- a detail/thread view when the active object has children;
- a compose/edit buffer when editing text;
- a command/status line;
- unobtrusive indicators for active profile, instance, network state, pending activity, and errors.

Network activity must not redefine Vim state. A failed request returns control to the same mode and view and preserves the selection and draft whenever possible.

### 5.4 Default commands

The command vocabulary must include stable defaults equivalent to:

- `:profile`, `:profile {name}`, `:profile-new`, `:profile-logout`;
- `:login`, `:logout`, `:whoami`;
- `:feed`, `:community`, `:post`, `:search`;
- `:refresh`, `:open`, `:reply`, `:edit`, `:delete`;
- `:subscribe`, `:save`, `:vote`;
- `:media`, `:download-media`, `:downloads`;
- `:help`, `:quit`.

Command names and key mappings are configuration data, not hard-coded behavior. Destructive commands require confirmation.

## 6. Functional requirements

### 6.1 Profiles and instances

A profile represents one instance/account pair. Multiple profiles may point to the same Lemmy host. Anonymous browsing is represented as a profile context without credentials or as an equivalent explicit anonymous mode.

The client must:

- configure, validate, rename, and remove profiles;
- test an instance connection before login;
- support multiple profiles for the same base URL;
- display the active profile and instance at all times;
- switch profiles without restarting;
- isolate profile-scoped cache, drafts, requests, and credentials;
- clear profile-scoped transient selections and buffers on switch;
- prevent requests created under the old profile from using the new profile’s credentials;
- restore the destination profile’s last view and cached content when available.

A profile switch is a hard context transition. In-flight work is cancelled or detached where safe, the active authorization context changes, and no subsequent request may use the old profile.

### 6.2 Authentication

Authentication is profile-scoped and explicit:

- support anonymous requests where the instance permits them;
- log in through the Lemmy HTTP API;
- restore sessions from the OS credential store when possible;
- store passwords, tokens, and session secrets only in the OS credential store;
- keep non-secret profile metadata in an ordinary user configuration file;
- support explicit logout, credential replacement, and profile deletion;
- provide `:login`, `:logout`, and `:whoami` flows;
- distinguish invalid credentials, expired sessions, unavailable instances, authorization failures, and unsupported server behavior;
- support a required 2FA step without echoing one-time codes;
- redact credentials from logs, diagnostics, status messages, and crash reports.

Session data is disposable and revocable. Credential-store unavailability must produce an actionable error rather than silently writing secrets to the configuration file.

### 6.3 Reading and discovery

The personal-client release must support:

- home, local, and subscribed feeds where exposed by the target Lemmy API;
- community browsing and community detail;
- post detail and threaded comments;
- pagination and refresh;
- search across supported Lemmy content;
- opening federated links and identifying their source instance;
- visible scores, author, community, creation/edit state, and interaction state;
- cached recently viewed content with an explicit stale indicator.

The UI must preserve list position and thread context across refreshes when the underlying objects still exist.

### 6.4 Personal interaction

Authenticated users must be able to perform supported operations within server permissions:

- vote on posts and comments;
- save and unsave posts or comments where supported;
- subscribe and unsubscribe to communities;
- create, edit, and delete posts;
- create, edit, and delete comments;
- reply from a thread or selected comment;
- preserve a draft until the server confirms a successful mutation or the user explicitly discards it.

The client must show the target instance and account in mutation context. Mutation results must distinguish confirmed success, explicit server rejection, and uncertain timeout where possible.

### 6.5 Composition and drafts

Compose and edit buffers use Insert mode and provide:

- multiline text editing;
- clear field validation before submission;
- post title/body/link fields appropriate to the selected operation;
- comment and reply composition;
- edit mode that identifies the original object;
- explicit submit, cancel, and discard actions;
- in-session draft preservation across transient network failures and profile navigation where the draft remains associated with its originating profile.

### 6.6 Media viewing

Media handling is asynchronous and follows this policy:

1. Resolve the media URL and MIME type from server metadata, HTTP headers, or filename.
2. Apply configured per-MIME policy.
3. Use mailcap as the default external handler.
4. If Kitty support is enabled and terminal capability detection succeeds, render supported images inline.
5. Otherwise use a configured external handler.
6. For unsupported content, show metadata and a copy/open-URL action instead of failing silently.

The media subsystem must:

- never block the main TUI event loop;
- expose progress, completion, cancellation, and failure state;
- avoid passing credentials to external handlers;
- treat downloaded content as untrusted data;
- clean up temporary files after completion or on a later startup;
- support explicit media opening from a post, comment, or download-history entry.

### 6.7 Media downloads and current-session history

The user can download selected media through `:download-media` or an equivalent default mapping.

Downloads must:

- use a configured default directory or prompt for a destination;
- allow cancellation;
- apply an explicit collision policy: prompt, overwrite, or generate a unique filename;
- retain the final local path, source URL, MIME type, instance/profile, timestamp, and status;
- record failed and cancelled attempts as history entries when enough metadata exists.

`:downloads` opens a searchable current-session download-history buffer. The history supports:

- reopening the local file through the normal media-opening path;
- opening its containing directory;
- copying the local path;
- retrying a failed download;
- deleting a downloaded file with confirmation.

History is cleared when the application exits. Persistent download history is not required for the personal-client release.

### 6.8 Errors and network behavior

Errors are classified as:

- authentication/session;
- authorization;
- validation;
- not found or deleted;
- rate limited;
- transient transport;
- timeout;
- unsupported capability/API behavior;
- malformed server response;
- local cache/storage;
- external media-handler failure.

Each error has a concise user-facing message and, where applicable, a retry or recovery command. Detailed server data is available through a diagnostic view.

Reads may retry bounded transient failures. Non-idempotent mutations must not be blindly retried. A timeout must not be presented as confirmed mutation failure when the server result is unknown. A stale cache may be displayed only with a stale indicator.

## 7. Architecture

The implementation uses a layered adapter architecture:

```text
Terminal events
      ↓
Vim input engine
      ↓
Application commands
      ↓
Application state and navigation
      ↓
Repositories and services
      ↓
Lemmy HTTP adapter, credential store, cache, media handlers
```

### 7.1 Terminal shell

Owns ratatui rendering, crossterm input, terminal initialization/restoration, resize, interrupt, suspend/resume, and terminal capability detection. Terminal cleanup must run on normal exit and recoverable application errors.

### 7.2 Input engine

Translates key sequences into semantic commands and manages modes, mappings, counts, pending operators, search, registers, and marks. It must be unit-testable without a terminal or network.

### 7.3 Application layer

Executes semantic commands such as `OpenPost`, `Reply`, `SwitchProfile`, `RefreshFeed`, `DownloadMedia`, and `ShowDownloads`. It owns navigation history, active buffers, selection, drafts, notifications, pending activity, and user-facing result events.

### 7.4 Domain model

Defines stable internal representations for profiles, instances, users, communities, posts, comments, votes, subscriptions, notifications, media references, downloads, and pagination. Domain types must not expose raw API-version-specific request/response structures.

### 7.5 Lemmy adapter

Encapsulates HTTP transport, authentication, pagination, retries, rate-limit handling, API version/capability detection, response normalization, and request serialization. The application layer depends on adapter interfaces and normalized domain results, not endpoint details.

The adapter must use deterministic fixtures for supported API behavior and must make unsupported operations explicit rather than silently dropping them.

### 7.6 Profile and credential services

Profile metadata is stored in a user configuration file. Secrets are stored through the platform credential store. Profile/account identity and instance base URL are explicit in every repository request. Services expose login, logout, session restoration, profile validation, and deletion without leaking secret values to callers unnecessarily.

### 7.7 Cache and draft store

The cache stores recently viewed entities, pagination metadata, and synchronization timestamps. It is scoped by profile and instance. Drafts are separate from synchronized server data and are associated with their originating profile and operation.

The cache is disposable. Malformed or incompatible cache entries must be ignored or rebuilt without preventing a fresh connection. Cache errors must not overwrite drafts.

### 7.8 Media service

The media service resolves metadata, downloads content, selects handlers, launches external processes, detects Kitty support, tracks cancellation/progress, and records current-session download history. It communicates with the application through events and never blocks input or rendering.

## 8. Configuration

Configuration uses a human-editable TOML file. It contains non-secret settings only:

- global UI and cache settings;
- profile names, instance URLs, and non-secret account labels;
- key mappings and command mappings;
- media policy and Kitty opt-in;
- download directory and collision policy;
- mailcap/external-handler overrides;
- diagnostic/logging policy.

Passwords, tokens, and session secrets must never be stored in TOML. Configuration parsing must reject unknown security-sensitive fields that resemble credentials rather than silently accepting them.

Configuration changes made through the application must be validated before replacement and written atomically.

## 9. Privacy and safety

- Logs are opt-in and redact authorization headers, tokens, passwords, private content, and sensitive profile values.
- Debug logging is disabled by default.
- Profile-scoped cache and configuration files use restrictive permissions where supported.
- External media handlers receive only the intended local path or URL and no authentication material.
- The client never silently uploads local files or sends mutations.
- Destructive actions require confirmation.
- Status and confirmation views identify the active instance and account.
- Downloaded media and temporary files are treated as untrusted content.

## 10. Delivery roadmap

### Phase 1: Foundation

- Rust project and CLI entry point;
- safe terminal lifecycle;
- ratatui shell and event loop;
- Vim normal, insert, command, and search modes;
- application state and navigation primitives;
- configuration parsing and validation;
- profile model and active-profile switching;
- structured status and error reporting.

### Phase 2: Anonymous browsing

- instance connection testing and capability detection;
- home, local, subscribed, and community feeds where supported;
- pagination and refresh;
- post detail and threaded comments;
- search;
- cached reads and stale indicators;
- help and command discovery.

### Phase 3: Authentication and personal interaction

- login, logout, session restoration, and `whoami`;
- OS credential-store integration;
- voting, saving, subscribing, and supported follows;
- create, edit, and delete posts;
- create, edit, and delete comments;
- in-session draft preservation;
- profile/account switching without restart.

### Phase 4: Media and composition

- MIME detection and mailcap integration;
- optional Kitty graphics support;
- external-handler fallback;
- asynchronous downloads and cancellation;
- configurable download directory and collision policy;
- current-session download history;
- rich compose/edit buffers with validation and mutation status.

### Phase 5: Hardening and extensibility

- API compatibility fixtures for supported Lemmy versions;
- rate-limit and transient-error behavior;
- cache invalidation and recovery;
- keymap and command customization;
- Linux packaging and standalone binaries;
- screen-reader and non-color-only accessibility review;
- configuration, profile, keymap, media, and troubleshooting documentation.

## 11. Testing strategy

Tests defend observable contracts rather than implementation details.

### Input engine

Test modes, mappings, counts, motions, pending operators, command parsing, search direction, cancellation, and invalid sequences.

### Application layer

Test profile switching, request context isolation, state transitions, draft preservation, mutation confirmation, download state, and error classification.

### Lemmy adapter

Use deterministic HTTP fixtures to test request serialization, authentication, pagination, response normalization, capability detection, server validation errors, rate limits, and safe mutation retry behavior.

### Profile and credential services

Test profile isolation, secret redaction, login/logout lifecycle, credential-store failures, atomic config replacement, and corrupted-config recovery.

### Cache and drafts

Test profile scoping, stale reads, invalidation, malformed-entry recovery, draft survival, and cache failure isolation.

### Media subsystem

Test MIME resolution, mailcap precedence, Kitty opt-in and capability fallback, external-handler failure, download cancellation, collision policy, temporary-file cleanup, and current-session history operations.

### Smoke scenarios

The application must be exercised end to end with fixture-backed services to verify:

1. launch and terminal restoration;
2. Vim navigation through a feed;
3. opening a post and threaded discussion;
4. composing and preserving a draft;
5. switching profiles;
6. authenticating and performing a supported mutation;
7. opening media through the configured policy;
8. downloading media and inspecting `:downloads`;
9. recovering from a transient network error;
10. exiting without leaving terminal state corrupted.

## 12. Release acceptance criteria

The personal-client release is acceptable when a user can:

1. Configure multiple instance/account profiles.
2. Launch anonymously or restore a saved session securely.
3. Switch profiles inside the TUI.
4. Browse and search Lemmy content with Vim-like navigation.
5. Read threaded discussions.
6. Vote, save, subscribe, post, comment, edit, and delete within server permissions.
7. Preserve drafts across transient failures.
8. Open media through mailcap, optionally render supported images through Kitty, or use an external handler.
9. Download media to disk and inspect current-session download history.
10. Recover from authentication, network, cache, and media-handler failures without losing terminal state or drafts.

A feature is not complete if it works only for one instance, one profile, one terminal size, or one happy-path response. Every release must preserve profile isolation, terminal restoration, secret redaction, and draft safety.
