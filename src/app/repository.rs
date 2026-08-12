use std::{collections::HashSet, sync::{Arc, Mutex}};

use serde_json::{json, Value};

use crate::{
    api::{FeedQuery, LemmyApi, MutationResult, Page, PostDetail, PostView},
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

#[derive(Clone)]
pub struct Repository {
    pub api: Arc<dyn LemmyApi>,
    pub cache: Arc<dyn CacheStore>,
    pub credentials: Arc<dyn CredentialStore>,
    deleted_posts: Arc<Mutex<HashSet<(crate::ProfileId, crate::PostId)>>>,
}

impl Repository {
    pub fn new(api: Arc<dyn LemmyApi>, cache: Arc<dyn CacheStore>, credentials: Arc<dyn CredentialStore>) -> Self {
        Self { api, cache, credentials, deleted_posts: Arc::new(Mutex::new(HashSet::new())) }
    }

    fn filter_deleted(&self, profile: &crate::ProfileId, page: &mut Page<PostView>) {
        let deleted = self.deleted_posts.lock().expect("deleted post set poisoned");
        page.items.retain(|post| !deleted.contains(&(profile.clone(), post.id)));
    }

    pub async fn feed(&self, context: &ProfileContext, query: FeedQuery) -> Result<CachedRead<Page<PostView>>> {
        let key = FeedKey::new(feed_key(&query));
        let Some(mut cached) = self.cache.read_feed(&context.profile.id, &key)? else {
            let mut page = self.api.feed(context, query).await?;
            self.filter_deleted(&context.profile.id, &mut page);
            self.cache.write_feed(&context.profile.id, &key, &CachedFeed::new(page_to_value(&page), unix_now(), false))?;
            return Ok(CachedRead { value: page, stale: false, refresh_error: None });
        };
        let mut page = page_from_value(&cached.entity)?;
        self.filter_deleted(&context.profile.id, &mut page);
        cached.entity = page_to_value(&page);
        cached.stale = true;
        self.cache.write_feed(&context.profile.id, &key, &cached)?;
        let api = self.api.clone();
        let cache = self.cache.clone();
        let deleted_posts = self.deleted_posts.clone();
        let context = context.clone();
        tokio::spawn(async move {
            if let Ok(mut page) = api.feed(&context, query).await {
                let deleted = deleted_posts.lock().expect("deleted post set poisoned");
                page.items.retain(|post| !deleted.contains(&(context.profile.id.clone(), post.id)));
                let _ = cache.write_feed(&context.profile.id, &key, &CachedFeed::new(page_to_value(&page), unix_now(), false));
            }
        });
        Ok(CachedRead { value: page, stale: true, refresh_error: None })
    }


    pub fn cached_feed(&self, context: &ProfileContext, query: &FeedQuery) -> Result<Option<CachedRead<Page<PostView>>>> {
        let key = FeedKey::new(feed_key(query));
        let Some(cached) = self.cache.read_feed(&context.profile.id, &key)? else { return Ok(None); };
        let page = page_from_value(&cached.entity)?;
        Ok(Some(CachedRead { value: page, stale: cached.stale, refresh_error: None }))
    }

    pub async fn post(&self, context: &ProfileContext, id: crate::PostId) -> Result<PostDetail> {
        self.api.post(context, id).await
    }

    pub async fn mutate(&self, context: &ProfileContext, mutation: Mutation) -> Result<MutationResult> {
        let deleted_post = match &mutation { Mutation::DeletePost(id) => Some(*id), _ => None };
        let result = self.api.mutate(context, mutation).await?;
        if !result.success { return Ok(result); }
        if let Some(id) = deleted_post {
            self.deleted_posts.lock().expect("deleted post set poisoned").insert((context.profile.id.clone(), id));
        }
        let key = FeedKey::new("home");
        if let Ok(Some(mut cached)) = self.cache.read_feed(&context.profile.id, &key) {
            if let Ok(mut page) = page_from_value(&cached.entity) {
                if let Some(id) = deleted_post {
                    page.items.retain(|candidate| candidate.id != id);
                } else if let Some(post) = &result.post {
                    if let Some(found) = page.items.iter_mut().find(|candidate| candidate.id == post.id) { *found = post.clone(); } else { page.items.push(post.clone()); }
                }
                cached.entity = page_to_value(&page);
                cached.stale = false;
                self.cache.write_feed(&context.profile.id, &key, &cached)?;
            }
        }
        Ok(result)
    }

    pub async fn session(&self, profile: &crate::ProfileId) -> Result<Option<crate::Session>> {
        self.credentials.get_session(profile).await
    }
}

fn feed_key(query: &FeedQuery) -> String {
    if query == &FeedQuery::home() { return "home".into(); }
    serde_json::to_string(&(query.sort.as_str(), query.page, query.limit, query.community.map(|id| id.0), query.search.as_deref())).unwrap_or_else(|_| "home".into())
}

fn unix_now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or_default() }

fn page_to_value(page: &Page<PostView>) -> Value {
    json!({ "items": page.items.iter().map(post_to_value).collect::<Vec<_>>(), "next_page": page.next_page })
}

fn page_from_value(value: &Value) -> Result<Page<PostView>> {
    let items = value.get("items").and_then(Value::as_array).ok_or_else(|| AppError::Storage("cached feed missing items".into()))?.iter().map(post_from_value).collect::<Result<Vec<_>>>()?;
    let next_page = value.get("next_page").and_then(Value::as_u64).map(|page| page as u32);
    Ok(Page { items, next_page })
}

fn post_to_value(post: &PostView) -> Value {
    json!({ "id": post.id.0, "title": post.title, "body": post.body, "url": post.url.as_ref().map(|url| url.as_str()), "community_id": post.community_id.0, "creator_id": post.creator_id.0, "score": post.score, "comments": post.comments, "published": post.published })
}

fn post_from_value(value: &Value) -> Result<PostView> {
    let id = value.get("id").and_then(Value::as_i64).ok_or_else(|| AppError::Storage("cached post missing id".into()))?;
    let title = value.get("title").and_then(Value::as_str).ok_or_else(|| AppError::Storage("cached post missing title".into()))?;
    let url = value.get("url").and_then(Value::as_str).map(url::Url::parse).transpose().map_err(|error| AppError::Storage(format!("cached post url: {error}")))?;
    Ok(PostView { id: crate::PostId(id), title: title.into(), body: value.get("body").and_then(Value::as_str).map(str::to_owned), url, community_id: crate::CommunityId(value.get("community_id").and_then(Value::as_i64).unwrap_or_default()), creator_id: crate::UserId(value.get("creator_id").and_then(Value::as_i64).unwrap_or_default()), score: value.get("score").and_then(Value::as_i64).unwrap_or_default(), comments: value.get("comments").and_then(Value::as_i64).unwrap_or_default(), published: value.get("published").and_then(Value::as_str).map(str::to_owned) })
}
