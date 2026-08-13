use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

use crate::{
    api::{CommentView, FeedQuery, LemmyApi, MutationResult, Page, PostDetail, PostView},
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
    deleted_posts: Arc<Mutex<HashSet<(crate::ProfileId, crate::PostId)>>>,
    confirmed_posts: Arc<Mutex<HashMap<(crate::ProfileId, crate::PostId), PostView>>>,
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
            deleted_posts: Arc::new(Mutex::new(HashSet::new())),
            confirmed_posts: Arc::new(Mutex::new(HashMap::new())),
            refreshes: Arc::new(Mutex::new(HashMap::new())),
            context_epochs: Arc::new(Mutex::new(HashMap::new())),
            cache_writes: Arc::new(Mutex::new(())),
        }
    }

    fn register_refresh(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
        requested: u64,
    ) -> (u64, u64) {
        let epochs = self
            .context_epochs
            .lock()
            .expect("context epoch state poisoned");
        let epoch = *epochs.get(profile).unwrap_or(&0);
        let mut refreshes = self.refreshes.lock().expect("refresh state poisoned");
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

    fn write_refresh_locked(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
        generation: u64,
        epoch: u64,
        feed: &CachedFeed,
    ) -> Result<bool> {
        let epochs = self
            .context_epochs
            .lock()
            .expect("context epoch state poisoned");
        if epochs.get(profile).copied().unwrap_or_default() != epoch {
            return Ok(false);
        }
        let mut refreshes = self.refreshes.lock().expect("refresh state poisoned");
        let state = refreshes.entry((profile.clone(), key.clone())).or_default();
        if state.latest != generation {
            return Ok(false);
        }
        self.cache.write_feed(profile, key, feed)?;
        state.completed = Some((generation, None));
        Ok(true)
    }

    fn record_refresh_error(
        &self,
        profile: &crate::ProfileId,
        key: &FeedKey,
        generation: u64,
        epoch: u64,
        error: String,
    ) {
        let epochs = self
            .context_epochs
            .lock()
            .expect("context epoch state poisoned");
        if epochs.get(profile).copied().unwrap_or_default() != epoch {
            return;
        }
        let mut refreshes = self.refreshes.lock().expect("refresh state poisoned");
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

    pub fn invalidate_profile_context(&self, profile: &crate::ProfileId) {
        let _cache_write = self.cache_writes.lock().expect("cache write lock poisoned");
        let mut epochs = self
            .context_epochs
            .lock()
            .expect("context epoch state poisoned");
        let epoch = epochs.entry(profile.clone()).or_default();
        *epoch = epoch.saturating_add(1);
        let mut refreshes = self.refreshes.lock().expect("refresh state poisoned");
        refreshes.retain(|(id, _), _| id != profile);
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

    pub async fn feed_with_generation(
        &self,
        context: &ProfileContext,
        query: FeedQuery,
        requested_generation: u64,
    ) -> Result<CachedRead<Page<PostView>>> {
        let key = FeedKey::new(feed_key(&query));
        let (generation, epoch) =
            self.register_refresh(&context.profile.id, &key, requested_generation);
        let Some(mut cached) = self.cache.read_feed(&context.profile.id, &key)? else {
            let mut page = self.api.feed(context, query).await?;
            let _cache_write = self.cache_writes.lock().expect("cache write lock poisoned");
            self.reconcile_page(&context.profile.id, &mut page);
            let _ = self.write_refresh_locked(
                &context.profile.id,
                &key,
                generation,
                epoch,
                &CachedFeed::new(page_to_value(&page), unix_now(), false),
            )?;
            return Ok(CachedRead {
                value: page,
                stale: false,
                refresh_error: None,
            });
        };
        let mut page = page_from_value(&cached.entity)?;
        let _cache_write = self.cache_writes.lock().expect("cache write lock poisoned");
        self.reconcile_page(&context.profile.id, &mut page);
        cached.entity = page_to_value(&page);
        cached.stale = true;
        let _ = self.write_refresh_locked(&context.profile.id, &key, generation, epoch, &cached)?;
        drop(_cache_write);
        let api = self.api.clone();
        let repository = self.clone();
        let context = context.clone();
        tokio::spawn(async move {
            match api.feed(&context, query).await {
                Ok(mut page) => {
                    let write_result = {
                        let _cache_write = repository
                            .cache_writes
                            .lock()
                            .expect("cache write lock poisoned");
                        repository.reconcile_page(&context.profile.id, &mut page);
                        repository.write_refresh_locked(
                            &context.profile.id,
                            &key,
                            generation,
                            epoch,
                            &CachedFeed::new(page_to_value(&page), unix_now(), false),
                        )
                    };
                    if let Err(error) = write_result {
                        repository.record_refresh_error(
                            &context.profile.id,
                            &key,
                            generation,
                            epoch,
                            error.to_string(),
                        );
                    }
                }
                Err(error) => repository.record_refresh_error(
                    &context.profile.id,
                    &key,
                    generation,
                    epoch,
                    error.to_string(),
                ),
            }
        });
        Ok(CachedRead {
            value: page,
            stale: true,
            refresh_error: None,
        })
    }

    pub fn cached_feed(
        &self,
        context: &ProfileContext,
        query: &FeedQuery,
    ) -> Result<Option<CachedRead<Page<PostView>>>> {
        let key = FeedKey::new(feed_key(query));
        let _cache_write = self.cache_writes.lock().expect("cache write lock poisoned");
        let Some(mut cached) = self.cache.read_feed(&context.profile.id, &key)? else {
            return Ok(None);
        };
        let mut page = page_from_value(&cached.entity)?;
        let before = page_to_value(&page);
        self.reconcile_page(&context.profile.id, &mut page);
        if page_to_value(&page) != before {
            cached.entity = page_to_value(&page);
            self.cache.write_feed(&context.profile.id, &key, &cached)?;
        }
        Ok(Some(CachedRead {
            value: page,
            stale: cached.stale,
            refresh_error: None,
        }))
    }

    pub fn take_completed_feed(
        &self,
        context: &ProfileContext,
        query: &FeedQuery,
    ) -> CompletedFeed {
        let key = FeedKey::new(feed_key(query));
        let completion = {
            let mut refreshes = self.refreshes.lock().expect("refresh state poisoned");
            refreshes
                .get_mut(&(context.profile.id.clone(), key))
                .and_then(|state| state.completed.take())
        };
        let Some((generation, error)) = completion else {
            return Ok(None);
        };
        if let Some(error) = error {
            return Ok(Some((
                generation,
                Err(crate::error::AppError::Network(error)),
            )));
        }
        let Some(read) = self.cached_feed(context, query)? else {
            return Ok(None);
        };
        if read.stale {
            return Ok(None);
        }
        Ok(Some((generation, Ok(read))))
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

    pub async fn mutate(
        &self,
        context: &ProfileContext,
        mutation: Mutation,
    ) -> Result<MutationResult> {
        let deleted_post = match &mutation {
            Mutation::DeletePost(id) => Some(*id),
            _ => None,
        };
        let result = self.api.mutate(context, mutation).await?;
        if !result.success {
            return Ok(result);
        }
        let _cache_write = self.cache_writes.lock().expect("cache write lock poisoned");
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
        let key = FeedKey::new("home");
        if let Ok(Some(mut cached)) = self.cache.read_feed(&context.profile.id, &key)
            && let Ok(mut page) = page_from_value(&cached.entity)
        {
            self.reconcile_page(&context.profile.id, &mut page);
            cached.entity = page_to_value(&page);
            cached.stale = false;
            self.cache.write_feed(&context.profile.id, &key, &cached)?;
        }
        Ok(result)
    }

    pub async fn session(&self, profile: &crate::ProfileId) -> Result<Option<crate::Session>> {
        self.credentials.get_session(profile).await
    }
}

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

#[cfg(test)]
mod tests {
    use super::feed_key;
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
}
