use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    api::{
        CommentView, CommunityQuery, CommunityView, FeedListing, FeedQuery, LemmyApi,
        MutationResult, Page, PostDetail, PostView,
    },
    cache::{CacheStore, CachedFeed, FeedKey},
    domain::{Mutation, ProfileContext},
    error::{AppError, Result},
    profiles::CredentialStore,
};

#[derive(Debug)]
pub struct CachedRead<T> {
    pub value: T,
    pub stale: bool,
    pub refresh_error: Option<AppError>,
}

/// Result of a completed background feed refresh: the generation that
/// finished and its read result, or `None` when nothing has completed yet.
type CompletedFeed = Result<Option<(u64, Result<CachedRead<Page<PostView>>>)>>;

#[derive(Default)]
struct RefreshState {
    latest: u64,
    completed: Option<(u64, Option<String>)>,
}

#[derive(Clone)]
pub struct Repository {
    pub api: Arc<dyn LemmyApi>,
    pub cache: Arc<dyn CacheStore>,
    pub credentials: Arc<dyn CredentialStore>,
    deleted_posts: Arc<std::sync::Mutex<HashSet<(crate::ProfileId, crate::PostId)>>>,
    confirmed_posts: Arc<std::sync::Mutex<HashMap<(crate::ProfileId, crate::PostId), PostView>>>,
    refreshes: Arc<Mutex<HashMap<(crate::ProfileId, FeedKey), RefreshState>>>,
    context_epochs: Arc<Mutex<HashMap<crate::ProfileId, u64>>>,
    cache_writes: Arc<Mutex<()>>,
}

impl Repository {
    pub fn new(
        api: Arc<dyn LemmyApi>,
        cache: Arc<dyn CacheStore>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            api,
            cache,
            credentials,
            deleted_posts: Arc::new(std::sync::Mutex::new(HashSet::new())),
            confirmed_posts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            refreshes: Arc::new(Mutex::new(HashMap::new())),
            context_epochs: Arc::new(Mutex::new(HashMap::new())),
            cache_writes: Arc::new(Mutex::new(())),
        }
    }

    async fn register_refresh(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
        requested: u64,
    ) -> (u64, u64) {
        let epochs = self.context_epochs.lock().await;
        let epoch = *epochs.get(profile).unwrap_or(&0);
        let mut refreshes = self.refreshes.lock().await;
        let state = refreshes.entry((profile.clone(), key.clone())).or_default();
        let generation = if requested == 0 {
            state.latest.saturating_add(1)
        } else {
            requested
        };
        if generation >= state.latest {
            state.latest = generation;
            state.completed = None;
        }
        (generation, epoch)
    }

    /// Record a completed fresh write for `generation`. `epoch` mismatch or
    /// a superseded generation refuses the write, so a stale background
    /// refresh can never clobber a newer one (or a context switch).
    async fn write_refresh_locked(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
        generation: u64,
        epoch: u64,
        feed: &CachedFeed,
    ) -> Result<bool> {
        let epochs = self.context_epochs.lock().await;
        if epochs.get(profile).copied().unwrap_or_default() != epoch {
            return Ok(false);
        }
        // The refreshes guard is held across the awaited cache write so the
        // supersession check and the completed marker stay atomic with the
        // payload write (the cache store's own connection lock serializes
        // the SQLite statement itself).
        let mut refreshes = self.refreshes.lock().await;
        let state = refreshes.entry((profile.clone(), key.clone())).or_default();
        if state.latest != generation {
            return Ok(false);
        }
        crate::cache::ops::write_feed(&self.cache, profile, key, feed).await?;
        state.completed = Some((generation, None));
        Ok(true)
    }

    async fn record_refresh_error(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
        generation: u64,
        epoch: u64,
        error: String,
    ) {
        let epochs = self.context_epochs.lock().await;
        if epochs.get(profile).copied().unwrap_or_default() != epoch {
            return;
        }
        let mut refreshes = self.refreshes.lock().await;
        let state = refreshes.entry((profile.clone(), key.clone())).or_default();
        if state.latest == generation {
            state.completed = Some((generation, Some(error)));
        }
    }

    fn reconcile_page(&self, profile: &crate::ProfileId, page: &mut Page<PostView>) {
        let deleted = self
            .deleted_posts
            .lock()
            .expect("deleted post set poisoned");
        page.items
            .retain(|post| !deleted.contains(&(profile.clone(), post.id)));
        let confirmed = self
            .confirmed_posts
            .lock()
            .expect("confirmed post state poisoned");
        for post in &mut page.items {
            if let Some(updated) = confirmed.get(&(profile.clone(), post.id)) {
                *post = updated.clone();
            }
        }
        let present = page
            .items
            .iter()
            .map(|post| post.id)
            .collect::<HashSet<_>>();
        for ((mutation_profile, id), post) in confirmed.iter() {
            if mutation_profile == profile
                && !deleted.contains(&(profile.clone(), *id))
                && !present.contains(id)
            {
                page.items.push(post.clone());
            }
        }
    }

    pub async fn invalidate_profile_context(&self, profile: &crate::ProfileId) {
        let _cache_write = self.cache_writes.lock().await;
        let mut epochs = self.context_epochs.lock().await;
        let epoch = epochs.entry(profile.clone()).or_default();
        *epoch = epoch.saturating_add(1);
        let mut refreshes = self.refreshes.lock().await;
        refreshes.retain(|(id, _), _| id != profile);
        drop(refreshes);
        drop(epochs);
        self.deleted_posts
            .lock()
            .expect("deleted post set poisoned")
            .retain(|(id, _)| id != profile);
        self.confirmed_posts
            .lock()
            .expect("confirmed post state poisoned")
            .retain(|(id, _), _| id != profile);
    }

    pub async fn feed(
        &self,
        context: &ProfileContext,
        query: FeedQuery,
    ) -> Result<CachedRead<Page<PostView>>> {
        self.feed_with_generation(context, query, 0).await
    }

    /// Start a feed refresh, returning directly in both cases:
    /// - cache hit: the stale page immediately, with a background refresh
    ///   spawned (completion lands via `take_completed_feed`);
    /// - cache miss: NO page yet. The caller must not await the network part
    ///   on a miss; use `spawn_feed_miss` for a fully detached miss and apply
    ///   the result when `take_completed_feed` yields it.
    ///
    /// The inline miss arm is kept for repository-level callers (tests) that
    /// accept the blocking fetch.
    pub async fn feed_with_generation(
        &self,
        context: &ProfileContext,
        query: FeedQuery,
        requested_generation: u64,
    ) -> Result<CachedRead<Page<PostView>>> {
        let key = FeedKey::new(feed_key(&query));
        let (generation, epoch) = self
            .register_refresh(&context.profile.id, &key, requested_generation)
            .await;
        let Some(mut cached) =
            crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await?
        else {
            let mut page = self.api.feed(context, query).await?;
            let _cache_write = self.cache_writes.lock().await;
            self.reconcile_page(&context.profile.id, &mut page);
            let _ = self
                .write_refresh_locked(
                    &context.profile.id,
                    &key,
                    generation,
                    epoch,
                    &CachedFeed::new(page_to_value(&page), unix_now(), false),
                )
                .await?;
            return Ok(CachedRead {
                value: page,
                stale: false,
                refresh_error: None,
            });
        };
        let mut page = page_from_value(&cached.entity)?;
        let _cache_write = self.cache_writes.lock().await;
        self.reconcile_page(&context.profile.id, &mut page);
        cached.entity = page_to_value(&page);
        cached.stale = true;
        let _ = self
            .write_refresh_locked(&context.profile.id, &key, generation, epoch, &cached)
            .await?;
        drop(_cache_write);
        self.refresh_in_background(
            &context.profile.id,
            key.clone(),
            generation,
            epoch,
            context,
            query,
        );
        Ok(CachedRead {
            value: page,
            stale: true,
            refresh_error: None,
        })
    }

    /// Spawn a fully detached feed fetch for a cache miss: registers the
    /// generation (so a later `take_completed_feed` can pick the result up
    /// and supersession stays generation-guarded) and returns immediately.
    pub async fn spawn_feed_miss(
        &self,
        context: &ProfileContext,
        query: FeedQuery,
        requested_generation: u64,
    ) -> Result<()> {
        let key = FeedKey::new(feed_key(&query));
        let (generation, epoch) = self
            .register_refresh(&context.profile.id, &key, requested_generation)
            .await;
        self.refresh_in_background(&context.profile.id, key, generation, epoch, context, query);
        Ok(())
    }

    /// The background half of a feed refresh: fetch, reconcile with local
    /// mutations, write through to the cache (generation-guarded), and leave
    /// a completion for `take_completed_feed` to drain. Fire-and-forget.
    fn refresh_in_background(
        &self,
        profile: &crate::ProfileId,
        key: FeedKey,
        generation: u64,
        epoch: u64,
        context: &ProfileContext,
        query: FeedQuery,
    ) {
        let api = self.api.clone();
        let repository = self.clone();
        let context = context.clone();
        let profile = profile.clone();
        tokio::spawn(async move {
            match api.feed(&context, query).await {
                Ok(mut page) => {
                    let write_result = {
                        let _cache_write = repository.cache_writes.lock().await;
                        repository.reconcile_page(&profile, &mut page);
                        repository
                            .write_refresh_locked(
                                &profile,
                                &key,
                                generation,
                                epoch,
                                &CachedFeed::new(page_to_value(&page), unix_now(), false),
                            )
                            .await
                    };
                    if let Err(error) = write_result {
                        repository
                            .record_refresh_error(
                                &profile,
                                &key,
                                generation,
                                epoch,
                                error.to_string(),
                            )
                            .await;
                    }
                }
                Err(error) => {
                    repository
                        .record_refresh_error(&profile, &key, generation, epoch, error.to_string())
                        .await;
                }
            }
        });
    }

    pub async fn cached_feed(
        &self,
        context: &ProfileContext,
        query: &FeedQuery,
    ) -> Result<Option<CachedRead<Page<PostView>>>> {
        let key = FeedKey::new(feed_key(query));
        let _cache_write = self.cache_writes.lock().await;
        let Some(mut cached) =
            crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await?
        else {
            return Ok(None);
        };
        let mut page = page_from_value(&cached.entity)?;
        let before = page_to_value(&page);
        self.reconcile_page(&context.profile.id, &mut page);
        if page_to_value(&page) != before {
            cached.entity = page_to_value(&page);
            crate::cache::ops::write_feed(&self.cache, &context.profile.id, &key, &cached).await?;
        }
        Ok(Some(CachedRead {
            value: page,
            stale: cached.stale,
            refresh_error: None,
        }))
    }

    /// Drain a completed background feed refresh for `query`; `None` until
    /// the refresh (if any) lands. A row still marked stale yields nothing
    /// (the refresh is still in flight or failed without a completion).
    pub async fn take_completed_feed(
        &self,
        context: &ProfileContext,
        query: &FeedQuery,
    ) -> CompletedFeed {
        let key = FeedKey::new(feed_key(query));
        let Some((generation, error)) = self.take_completion(&context.profile.id, &key).await?
        else {
            return Ok(None);
        };
        if let Some(error) = error {
            return Ok(Some((
                generation,
                Err(crate::error::AppError::Network(error)),
            )));
        }
        let Some(read) = self.cached_feed(context, query).await? else {
            return Ok(None);
        };
        if read.stale {
            return Ok(None);
        }
        Ok(Some((generation, Ok(read))))
    }

    /// Take the completed marker (generation + optional error) for `key`,
    /// if the background refresh that owns it has finished.
    async fn take_completion(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
    ) -> Result<Option<(u64, Option<String>)>> {
        let mut refreshes = self.refreshes.lock().await;
        Ok(refreshes
            .get_mut(&(profile.clone(), key.clone()))
            .and_then(|state| state.completed.take()))
    }

    /// Record a freshly fetched feed (e.g. a paginated page the app loaded
    /// itself) into the cache, reconciled with local mutations. Used by the
    /// app's detached read flow so revisits paint instantly from cache.
    pub async fn record_fresh_feed_page(
        &self,
        context: &ProfileContext,
        query: FeedQuery,
        page: &Page<PostView>,
    ) -> Result<()> {
        let key = FeedKey::new(feed_key(&query));
        let _cache_write = self.cache_writes.lock().await;
        let mut page = page.clone();
        self.reconcile_page(&context.profile.id, &mut page);
        crate::cache::ops::write_feed(
            &self.cache,
            &context.profile.id,
            &key,
            &CachedFeed::new(page_to_value(&page), unix_now(), false),
        )
        .await
    }

    pub async fn post(&self, context: &ProfileContext, id: crate::PostId) -> Result<PostDetail> {
        self.api.post(context, id).await
    }

    pub async fn comments(
        &self,
        context: &ProfileContext,
        id: crate::PostId,
    ) -> Result<Vec<CommentView>> {
        self.api.comments(context, id).await
    }

    /// Cached post detail, reconciled with local mutations. A deleted post
    /// never surfaces from cache.
    pub async fn cached_post(
        &self,
        context: &ProfileContext,
        id: crate::PostId,
    ) -> Result<Option<CachedRead<PostDetail>>> {
        let key = post_detail_key(id);
        let _cache_write = self.cache_writes.lock().await;
        let Some(cached) =
            crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await?
        else {
            return Ok(None);
        };
        if self
            .deleted_posts
            .lock()
            .expect("deleted post set poisoned")
            .contains(&(context.profile.id.clone(), id))
        {
            return Ok(None);
        }
        let mut detail = post_detail_from_value(&cached.entity)?;
        if let Some(updated) = self
            .confirmed_posts
            .lock()
            .expect("confirmed post state poisoned")
            .get(&(context.profile.id.clone(), id))
        {
            detail.post = updated.clone();
        }
        Ok(Some(CachedRead {
            value: detail,
            stale: cached.stale,
            refresh_error: None,
        }))
    }

    /// Write a freshly fetched post detail through to the cache, reconciled
    /// with the latest confirmed mutation for that post (a mutation that
    /// landed after the fetch must not be overwritten by the fetch).
    pub async fn record_fresh_post(
        &self,
        context: &ProfileContext,
        id: crate::PostId,
        detail: &PostDetail,
    ) -> Result<()> {
        if self
            .deleted_posts
            .lock()
            .expect("deleted post set poisoned")
            .contains(&(context.profile.id.clone(), id))
        {
            return Ok(());
        }
        let key = post_detail_key(id);
        let _cache_write = self.cache_writes.lock().await;
        let mut detail = detail.clone();
        if let Some(updated) = self
            .confirmed_posts
            .lock()
            .expect("confirmed post state poisoned")
            .get(&(context.profile.id.clone(), id))
        {
            detail.post = updated.clone();
        }
        crate::cache::ops::write_feed(
            &self.cache,
            &context.profile.id,
            &key,
            &CachedFeed::new(post_detail_to_value(&detail), unix_now(), false),
        )
        .await
    }

    /// Cached comment thread for a post.
    pub async fn cached_comments(
        &self,
        context: &ProfileContext,
        id: crate::PostId,
    ) -> Result<Option<CachedRead<Vec<CommentView>>>> {
        let key = comments_key(id);
        let _cache_write = self.cache_writes.lock().await;
        let Some(cached) =
            crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await?
        else {
            return Ok(None);
        };
        let comments = comments_from_value(&cached.entity)?;
        Ok(Some(CachedRead {
            value: comments,
            stale: cached.stale,
            refresh_error: None,
        }))
    }

    /// Write a freshly fetched comment thread through to the cache.
    pub async fn record_fresh_comments(
        &self,
        context: &ProfileContext,
        id: crate::PostId,
        comments: &[CommentView],
    ) -> Result<()> {
        let key = comments_key(id);
        let _cache_write = self.cache_writes.lock().await;
        crate::cache::ops::write_feed(
            &self.cache,
            &context.profile.id,
            &key,
            &CachedFeed::new(
                json!({ "items": comments.iter().map(comment_to_value).take(MAX_CACHED_COMMENTS).collect::<Vec<_>>() }),
                unix_now(),
                false,
            ),
        )
        .await
    }

    /// Cached community list for a listing (`All`/`Local`/`Subscribed`).
    pub async fn cached_communities(
        &self,
        context: &ProfileContext,
        query: &CommunityQuery,
    ) -> Result<Option<CachedRead<Page<CommunityView>>>> {
        let key = community_feed_key(query.listing);
        let _cache_write = self.cache_writes.lock().await;
        let Some(cached) =
            crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await?
        else {
            return Ok(None);
        };
        let page = communities_page_from_value(&cached.entity)?;
        Ok(Some(CachedRead {
            value: page,
            stale: cached.stale,
            refresh_error: None,
        }))
    }

    /// Write a freshly fetched community list through to the cache.
    pub async fn record_fresh_communities(
        &self,
        context: &ProfileContext,
        query: &CommunityQuery,
        page: &Page<CommunityView>,
    ) -> Result<()> {
        let key = community_feed_key(query.listing);
        let _cache_write = self.cache_writes.lock().await;
        crate::cache::ops::write_feed(
            &self.cache,
            &context.profile.id,
            &key,
            &CachedFeed::new(communities_page_to_value(page), unix_now(), false),
        )
        .await
    }

    pub async fn mutate(
        &self,
        context: &ProfileContext,
        mutation: Mutation,
    ) -> Result<MutationResult> {
        let deleted_post = match &mutation {
            Mutation::DeletePost(id) => Some(*id),
            _ => None,
        };
        let result = self.api.mutate(context, mutation.clone()).await?;
        if !result.success {
            return Ok(result);
        }
        let _cache_write = self.cache_writes.lock().await;
        if let Some(id) = deleted_post {
            self.deleted_posts
                .lock()
                .expect("deleted post set poisoned")
                .insert((context.profile.id.clone(), id));
            self.confirmed_posts
                .lock()
                .expect("confirmed post state poisoned")
                .remove(&(context.profile.id.clone(), id));
        } else if let Some(post) = &result.post {
            self.confirmed_posts
                .lock()
                .expect("confirmed post state poisoned")
                .insert((context.profile.id.clone(), post.id), post.clone());
        }
        // The home feed always reconciles so the very first screen reflects
        // the change immediately even before any refresh lands.
        let key = FeedKey::new("home");
        if let Ok(Some(mut cached)) =
            crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await
            && let Ok(mut page) = page_from_value(&cached.entity)
        {
            self.reconcile_page(&context.profile.id, &mut page);
            cached.entity = page_to_value(&page);
            cached.stale = false;
            crate::cache::ops::write_feed(&self.cache, &context.profile.id, &key, &cached).await?;
        }
        self.reconcile_entity_rows(context, &mutation, &result)
            .await;
        Ok(result)
    }

    /// Keep the per-entity cache rows (`post:<id>`, `comments:<id>`,
    /// `communities:*`) consistent with a confirmed mutation: posts and
    /// comments get the freshly returned objects, deleted posts wipe their
    /// thread row, and a subscription marks every community row stale.
    async fn reconcile_entity_rows(
        &self,
        context: &ProfileContext,
        mutation: &Mutation,
        result: &MutationResult,
    ) {
        match (mutation, result.post.as_ref(), result.comment.as_ref()) {
            (Mutation::DeletePost(id), _, _) => {
                let key = comments_key(*id);
                let _ = crate::cache::ops::write_feed(
                    &self.cache,
                    &context.profile.id,
                    &key,
                    &CachedFeed::new(json!({ "items": [] }), unix_now(), false),
                )
                .await;
            }
            (
                Mutation::VotePost { .. } | Mutation::SavePost { .. } | Mutation::EditPost(_),
                Some(post),
                _,
            ) => {
                let key = post_detail_key(post.id);
                if let Ok(Some(mut cached)) =
                    crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await
                {
                    cached.entity["post"] = post_to_value(post);
                    cached.stale = false;
                    let _ = crate::cache::ops::write_feed(
                        &self.cache,
                        &context.profile.id,
                        &key,
                        &cached,
                    )
                    .await;
                }
            }
            (
                Mutation::CreateComment(_)
                | Mutation::VoteComment { .. }
                | Mutation::EditComment(_)
                | Mutation::DeleteComment(_),
                _,
                Some(comment),
            ) => {
                let key = comments_key(comment.post_id);
                if let Ok(Some(mut cached)) =
                    crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await
                {
                    let mut items = cached
                        .entity
                        .get("items")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    items.retain(|item| {
                        item.get("id").and_then(Value::as_i64) != Some(comment.id.0)
                    });
                    if !matches!(mutation, Mutation::DeleteComment(_)) {
                        items.push(comment_to_value(comment));
                        items.truncate(MAX_CACHED_COMMENTS);
                    }
                    cached.entity = json!({ "items": items });
                    cached.stale = false;
                    let _ = crate::cache::ops::write_feed(
                        &self.cache,
                        &context.profile.id,
                        &key,
                        &cached,
                    )
                    .await;
                }
            }
            (Mutation::Subscribe { .. }, _, _) => {
                for listing in [
                    FeedListing::All,
                    FeedListing::Local,
                    FeedListing::Subscribed,
                ] {
                    let key = community_feed_key(listing);
                    if let Ok(Some(mut cached)) =
                        crate::cache::ops::read_feed(&self.cache, &context.profile.id, &key).await
                    {
                        cached.stale = true;
                        let _ = crate::cache::ops::write_feed(
                            &self.cache,
                            &context.profile.id,
                            &key,
                            &cached,
                        )
                        .await;
                    }
                }
            }
            _ => {}
        }
    }

    pub async fn session(&self, profile: &crate::ProfileId) -> Result<Option<crate::Session>> {
        self.credentials.get_session(profile).await
    }
}

/// Cap on cached comment rows: the API bounds fetches to
/// [`crate::api::http::MAX_ARRAY_ITEMS`]; caches mirror that bound.
const MAX_CACHED_COMMENTS: usize = 1024;

fn feed_key(query: &FeedQuery) -> String {
    if query == &FeedQuery::home() {
        return "home".into();
    }
    // The pagination cursor is opaque server data: a hostile instance could
    // mint unboundedly long or unboundedly many distinct cursors. Truncate
    // it so a single key can never exceed ~256 bytes; real Lemmy cursors are
    // short base64-ish tokens, so legit pages still get distinct keys.
    let bounded_cursor = query
        .page
        .as_deref()
        .map(|cursor| cursor.chars().take(256).collect::<String>());
    serde_json::to_string(&(
        query.sort.as_str(),
        bounded_cursor.as_deref(),
        query.limit,
        query.community.map(|id| id.0),
        query.search.as_deref(),
        query.listing,
    ))
    .unwrap_or_else(|_| "home".into())
}

/// Cache key for a community list: one row per listing. Feed keys are the
/// literal `home` or a JSON array starting with `[`, so they can never
/// collide with the prefixed entity keys.
fn community_feed_key(listing: FeedListing) -> FeedKey {
    FeedKey::new(format!("communities:{listing:?}"))
}

fn post_detail_key(id: crate::PostId) -> FeedKey {
    FeedKey::new(format!("post:{}", id.0))
}

fn comments_key(id: crate::PostId) -> FeedKey {
    FeedKey::new(format!("comments:{}", id.0))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn page_to_value(page: &Page<PostView>) -> Value {
    json!({ "items": page.items.iter().map(post_to_value).collect::<Vec<_>>(), "next_page": page.next_page })
}

fn page_from_value(value: &Value) -> Result<Page<PostView>> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Storage("cached feed missing items".into()))?
        .iter()
        .map(post_from_value)
        .collect::<Result<Vec<_>>>()?;
    let next_page = value
        .get("next_page")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(Page { items, next_page })
}

fn post_to_value(post: &PostView) -> Value {
    json!({ "id": post.id.0, "title": post.title, "body": post.body, "url": post.url.as_ref().map(|url| url.as_str()), "community_id": post.community_id.0, "creator_id": post.creator_id.0, "score": post.score, "comments": post.comments, "published": post.published })
}

fn post_from_value(value: &Value) -> Result<PostView> {
    let id = value
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Storage("cached post missing id".into()))?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Storage("cached post missing title".into()))?;
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .map(url::Url::parse)
        .transpose()
        .map_err(|error| AppError::Storage(format!("cached post url: {error}")))?;
    Ok(PostView {
        id: crate::PostId(id),
        title: title.into(),
        body: value.get("body").and_then(Value::as_str).map(str::to_owned),
        url,
        community_id: crate::CommunityId(
            value
                .get("community_id")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        ),
        creator_id: crate::UserId(
            value
                .get("creator_id")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        ),
        score: value
            .get("score")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        comments: value
            .get("comments")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        published: value
            .get("published")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn post_detail_to_value(detail: &PostDetail) -> Value {
    json!({
        "post": post_to_value(&detail.post),
        "comments": detail.comments.iter().map(comment_to_value).collect::<Vec<_>>()
    })
}

fn post_detail_from_value(value: &Value) -> Result<PostDetail> {
    let post = post_from_value(
        value
            .get("post")
            .ok_or_else(|| AppError::Storage("cached post detail missing post".into()))?,
    )?;
    let comments = value
        .get("comments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| comment_from_value(item).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(PostDetail { post, comments })
}

fn comment_to_value(comment: &CommentView) -> Value {
    json!({ "id": comment.id.0, "post_id": comment.post_id.0, "content": comment.content, "creator_id": comment.creator_id.0, "creator_name": comment.creator_name, "score": comment.score })
}

/// Decode a cached comment row (`{"items": [...]}`), tolerating entries that
/// no longer decode (forward-compatible projection).
fn comments_from_value(value: &Value) -> Result<Vec<CommentView>> {
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| comment_from_value(item).ok())
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| AppError::Storage("cached comments missing items".into()))
}

fn comment_from_value(value: &Value) -> Result<CommentView> {
    let id = value
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Storage("cached comment missing id".into()))?;
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Storage("cached comment missing content".into()))?;
    Ok(CommentView {
        id: crate::CommentId(id),
        post_id: crate::PostId(
            value
                .get("post_id")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        ),
        content: content.into(),
        creator_id: crate::UserId(
            value
                .get("creator_id")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        ),
        creator_name: value
            .get("creator_name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        score: value
            .get("score")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
    })
}

fn communities_page_to_value(page: &Page<CommunityView>) -> Value {
    json!({ "items": page.items.iter().map(community_to_value).collect::<Vec<_>>(), "next_page": page.next_page })
}

fn communities_page_from_value(value: &Value) -> Result<Page<CommunityView>> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Storage("cached communities missing items".into()))?
        .iter()
        .filter_map(|item| community_from_value(item).ok())
        .collect::<Vec<_>>();
    let next_page = value
        .get("next_page")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(Page { items, next_page })
}

fn community_to_value(community: &CommunityView) -> Value {
    json!({ "id": community.id.0, "name": community.name, "title": community.title, "subscribers": community.subscribers, "subscribed": community.subscribed })
}

fn community_from_value(value: &Value) -> Result<CommunityView> {
    let id = value
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Storage("cached community missing id".into()))?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Storage("cached community missing name".into()))?;
    Ok(CommunityView {
        id: crate::CommunityId(id),
        name: name.into(),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        subscribers: value
            .get("subscribers")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        subscribed: value
            .get("subscribed")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::FeedQuery;

    #[test]
    fn home_feed_uses_the_short_home_key() {
        assert_eq!(feed_key(&FeedQuery::home()), "home");
    }

    #[test]
    fn subscribed_feed_gets_a_distinct_cache_key() {
        // The subscribed listing and the home feed are different content and
        // must never share a cache row.
        let subscribed = FeedQuery::subscribed();
        assert_ne!(
            feed_key(&subscribed),
            feed_key(&FeedQuery::home()),
            "subscribed and home feeds must not share a cache key"
        );
        assert_ne!(feed_key(&subscribed), "home");
    }

    #[test]
    fn listing_is_part_of_the_key_across_queries() {
        let mut subscribed = FeedQuery::subscribed();
        subscribed.community = Some(crate::CommunityId(3));
        let mut home_in_community = FeedQuery::home();
        home_in_community.community = Some(crate::CommunityId(3));
        assert_ne!(
            feed_key(&subscribed),
            feed_key(&home_in_community),
            "same community, different listing: keys must differ"
        );
    }

    #[test]
    fn sort_is_part_of_the_key() {
        // Sorting is a server-side contract: a re-sorted feed is different
        // content and must get its own cache row.
        let active = FeedQuery::home();
        let mut newest = FeedQuery::home();
        newest.sort = "New".into();
        assert_ne!(
            feed_key(&active),
            feed_key(&newest),
            "different sorts must not share a cache key"
        );
    }

    #[test]
    fn entity_keys_never_collide_with_feed_keys() {
        // A hostile server could mint a cursor literally equal to one of the
        // prefixed entity keys; the feed-key tuple serialization keeps the
        // namespaces structurally disjoint.
        let home = feed_key(&FeedQuery::home());
        let mut cursed = FeedQuery::home();
        cursed.page = Some("post:1".into());
        let page = feed_key(&cursed);
        assert_ne!(home.as_str(), "post:1");
        assert!(!page.starts_with("post:") && !page.starts_with("comments:"));
        assert!(!page.starts_with("communities:"));
        assert_ne!(page, community_feed_key(FeedListing::All).as_str());
    }

    #[test]
    fn entity_key_shapes() {
        assert_eq!(
            community_feed_key(FeedListing::All).as_str(),
            "communities:All"
        );
        assert_eq!(
            community_feed_key(FeedListing::Local).as_str(),
            "communities:Local"
        );
        assert_eq!(
            community_feed_key(FeedListing::Subscribed).as_str(),
            "communities:Subscribed"
        );
        assert_eq!(post_detail_key(crate::PostId(7)).as_str(), "post:7");
        assert_eq!(comments_key(crate::PostId(7)).as_str(), "comments:7");
    }

    #[test]
    fn post_detail_round_trips_through_projection() {
        let detail = PostDetail {
            post: PostView {
                id: crate::PostId(1),
                title: "hello".into(),
                body: Some("body".into()),
                url: Some("https://example.com/a".parse().unwrap()),
                community_id: crate::CommunityId(2),
                creator_id: crate::UserId(3),
                score: 4,
                comments: 5,
                published: Some("2026-01-01T00:00:00Z".into()),
            },
            comments: vec![CommentView {
                id: crate::CommentId(9),
                post_id: crate::PostId(1),
                content: "first".into(),
                creator_id: crate::UserId(3),
                creator_name: "alice".into(),
                score: 1,
            }],
        };
        let mut value = post_detail_to_value(&detail);
        // Mutation reconciliation mutates entity rows in place with the same
        // projection; prove the stored shape is stable under it.
        value["post"]["score"] = json!(99);
        let round = post_detail_from_value(&value).expect("decode");
        assert_eq!(round.post.id, detail.post.id);
        assert_eq!(round.post.title, detail.post.title);
        assert_eq!(round.post.score, 99, "decode reflects the stored score");
        assert_eq!(round.comments.len(), 1);
        assert_eq!(round.comments[0].content, "first");
    }
}
