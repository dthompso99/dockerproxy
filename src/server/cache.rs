use super::ServerState;
use super::logging::{log, log_always};
use crate::DockerHost;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

pub(super) struct CachedResponse {
    pub(super) content_type: String,
    pub(super) digest: Option<String>,
    pub(super) body: Vec<u8>,
    modified: SystemTime,
}

pub(super) struct CacheEntry {
    pub(super) id: String,
    pub(super) host: String,
    pub(super) kind: String,
    pub(super) repository: String,
    pub(super) reference: String,
    pub(super) size: u64,
    pub(super) age_secs: u64,
    pub(super) ttl_remaining_secs: u64,
    pub(super) expires_at: Option<SystemTime>,
}

pub(super) fn read_cached_response(path: &Path) -> io::Result<Option<CachedResponse>> {
    let body_path = path.join("body");
    if !body_path.exists() {
        return Ok(None);
    }

    let metadata = fs::metadata(&body_path)?;
    let content_type = fs::read_to_string(path.join("content-type"))
        .unwrap_or_else(|_| "application/octet-stream".to_string());
    let digest = fs::read_to_string(path.join("digest")).ok();
    let body = fs::read(body_path)?;

    Ok(Some(CachedResponse {
        content_type: content_type.trim().to_string(),
        digest: digest.map(|value| value.trim().to_string()),
        body,
        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    }))
}

pub(super) fn is_stale(cached: &CachedResponse, ttl: u64) -> bool {
    if ttl == 0 {
        return true;
    }

    cached
        .modified
        .elapsed()
        .map(|age| age > Duration::from_secs(ttl))
        .unwrap_or(true)
}

pub(super) fn prune_expired_cache_entries(
    cache_dir: &Path,
    ttl: u64,
    log_level: u16,
) -> io::Result<()> {
    if !cache_dir.exists() {
        return Ok(());
    }

    let mut removed = 0;
    prune_expired_entries(cache_dir, ttl, log_level, &mut removed)?;
    remove_empty_dirs(cache_dir)?;

    log(
        log_level,
        1,
        "cache",
        &format!("prune removed {removed} expired cache entries"),
    );
    Ok(())
}

pub(super) fn start_cache_pruner(state: Arc<ServerState>) {
    thread::spawn(move || {
        let interval = Duration::from_secs(state.ttl.clamp(60, 3600));

        loop {
            thread::sleep(interval);
            if let Err(error) =
                prune_expired_cache_entries(&state.cache_dir, state.ttl, state.log_level)
            {
                log_always("cache", &format!("prune failed: {error}"));
            }
        }
    });
}

pub(super) fn list_cache_entries(cache_dir: &Path, ttl: u64) -> io::Result<Vec<CacheEntry>> {
    let mut entries = Vec::new();

    if cache_dir.exists() {
        collect_cache_entries(cache_dir, cache_dir, ttl, &mut entries)?;
    }

    entries.sort_by(|left, right| {
        left.host
            .cmp(&right.host)
            .then(left.kind.cmp(&right.kind))
            .then(left.repository.cmp(&right.repository))
            .then(left.reference.cmp(&right.reference))
    });

    Ok(entries)
}

pub(super) fn delete_cache_entry(cache_dir: &Path, id: &str, log_level: u16) -> io::Result<bool> {
    let Some(entry_path) = cache_entry_path_from_id(cache_dir, id) else {
        return Ok(false);
    };

    if entry_path.exists() {
        fs::remove_dir_all(&entry_path)?;
        remove_empty_dirs(cache_dir)?;
        log(
            log_level,
            1,
            "cache",
            &format!("deleted cache entry {}", entry_path.display()),
        );
    }

    Ok(true)
}

pub(super) fn cache_manifest_path(
    cache_dir: &Path,
    host: &DockerHost,
    name: &str,
    reference: &str,
) -> PathBuf {
    cache_dir
        .join(sanitize_path_component(&host.name))
        .join("manifests")
        .join(sanitize_slash_path(name))
        .join(sanitize_path_component(reference))
}

pub(super) fn cache_blob_path(
    cache_dir: &Path,
    host: &DockerHost,
    name: &str,
    digest: &str,
) -> PathBuf {
    cache_dir
        .join(sanitize_path_component(&host.name))
        .join("blobs")
        .join(sanitize_slash_path(name))
        .join(sanitize_path_component(digest))
}

fn prune_expired_entries(
    path: &Path,
    ttl: u64,
    log_level: u16,
    removed: &mut u64,
) -> io::Result<()> {
    if is_cache_entry_dir(path) {
        if is_cache_entry_expired(path, ttl)? {
            log(
                log_level,
                1,
                "cache",
                &format!("pruning expired cache entry {}", path.display()),
            );
            fs::remove_dir_all(path)?;
            *removed += 1;
        }

        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            prune_expired_entries(&entry_path, ttl, log_level, removed)?;
        }
    }

    Ok(())
}

fn is_cache_entry_dir(path: &Path) -> bool {
    path.join("body").is_file()
}

fn is_cache_entry_expired(path: &Path, ttl: u64) -> io::Result<bool> {
    let modified = fs::metadata(path.join("body"))?.modified()?;
    Ok(modified
        .elapsed()
        .map(|age| age > Duration::from_secs(ttl))
        .unwrap_or(true))
}

fn remove_empty_dirs(path: &Path) -> io::Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }

    let mut is_empty = true;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();

        if entry_path.is_dir() && remove_empty_dirs(&entry_path)? {
            fs::remove_dir(entry_path)?;
        } else {
            is_empty = false;
        }
    }

    Ok(is_empty)
}

fn collect_cache_entries(
    cache_dir: &Path,
    path: &Path,
    ttl: u64,
    entries: &mut Vec<CacheEntry>,
) -> io::Result<()> {
    if is_cache_entry_dir(path) {
        if let Some(entry) = cache_entry_from_path(cache_dir, path, ttl)? {
            entries.push(entry);
        }
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_cache_entries(cache_dir, &entry_path, ttl, entries)?;
        }
    }

    Ok(())
}

fn cache_entry_from_path(
    cache_dir: &Path,
    path: &Path,
    ttl: u64,
) -> io::Result<Option<CacheEntry>> {
    let relative = match path.strip_prefix(cache_dir) {
        Ok(relative) => relative,
        Err(_) => return Ok(None),
    };
    let parts: Vec<String> = relative
        .components()
        .filter_map(component_to_string)
        .collect();

    if parts.len() < 4 {
        return Ok(None);
    }

    let host = parts[0].clone();
    let kind = parts[1].clone();
    let reference = parts.last().cloned().unwrap_or_default();
    let repository = parts[2..parts.len() - 1].join("/");
    let body_path = path.join("body");
    let metadata = fs::metadata(&body_path)?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let age_secs = modified.elapsed().unwrap_or_default().as_secs();
    let ttl_remaining_secs = ttl.saturating_sub(age_secs);
    let expires_at = modified.checked_add(Duration::from_secs(ttl));

    Ok(Some(CacheEntry {
        id: relative.to_string_lossy().into_owned(),
        host,
        kind,
        repository,
        reference,
        size: metadata.len(),
        age_secs,
        ttl_remaining_secs,
        expires_at,
    }))
}

fn cache_entry_path_from_id(cache_dir: &Path, id: &str) -> Option<PathBuf> {
    let id_path = Path::new(id);

    if id_path.is_absolute()
        || id_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }

    let path = cache_dir.join(id_path);
    is_cache_entry_dir(&path).then_some(path)
}

fn component_to_string(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
        _ => None,
    }
}

fn sanitize_slash_path(value: &str) -> PathBuf {
    value.split('/').fold(PathBuf::new(), |path, part| {
        path.join(sanitize_path_component(part))
    })
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .collect()
}
