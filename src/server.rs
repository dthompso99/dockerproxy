use crate::{Config, DockerHost};
use cache::{
    cache_blob_path, cache_manifest_path, is_stale, prune_expired_cache_entries,
    read_cached_response, start_cache_pruner,
};
use http::{
    read_http_headers, write_registry_response, write_text_response, write_upstream_response,
};
use logging::{log, log_always};
use registry::fetch_and_maybe_store;
use reqwest::blocking::Client;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Instant;
use ui::{handle_ui_delete, write_ui_response};

mod cache;
mod http;
mod logging;
mod registry;
mod ui;

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

struct ServerState {
    config: Config,
    cache_dir: PathBuf,
    ttl: u64,
    log_level: u16,
    client: Client,
}

enum RegistryRequest<'a> {
    Ping,
    Manifest { name: &'a str, reference: &'a str },
    Blob { name: &'a str, digest: &'a str },
}

struct RequestTarget {
    path: String,
    namespace: Option<String>,
}

pub fn start_server(
    port: u16,
    config: Config,
    cache_dir: String,
    ttl: u64,
    log_level: u16,
) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let state = Arc::new(ServerState {
        config,
        cache_dir: PathBuf::from(cache_dir),
        ttl,
        log_level,
        client: Client::builder()
            .user_agent("dockerproxy/0.1")
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?,
    });

    log_always(
        "server",
        &format!(
            "Starting registry cache on port {} with cache directory {} and ttl {}",
            port,
            state.cache_dir.display(),
            state.ttl
        ),
    );
    prune_expired_cache_entries(&state.cache_dir, state.ttl, state.log_level)?;
    start_cache_pruner(Arc::clone(&state));
    log(state.log_level, 1, "server", "accept loop started");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
                let peer_addr = stream
                    .peer_addr()
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|_| "unknown peer".to_string());
                log(
                    state.log_level,
                    1,
                    &format!("conn#{connection_id}"),
                    &format!("accepted from {peer_addr}"),
                );

                let state = Arc::clone(&state);
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, connection_id, &state) {
                        log_always(
                            &format!("conn#{connection_id}"),
                            &format!("failed to handle connection: {error}"),
                        );
                    }
                });
            }
            Err(error) => log_always("server", &format!("failed to accept connection: {error}")),
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    connection_id: u64,
    state: &ServerState,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let request = read_http_headers(&mut stream)?;
    let request_line = request.lines().next().unwrap_or_default();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();

    log(
        state.log_level,
        1,
        &format!("conn#{connection_id}"),
        &format!("request {method} {path} {version}"),
    );
    log(
        state.log_level,
        2,
        &format!("conn#{connection_id}"),
        &format!(
            "headers: {}",
            request.replace("\r\n", " | ").replace('\n', " | ")
        ),
    );

    if method == "CONNECT" {
        return tunnel_connect(stream, path, connection_id, state.log_level);
    }

    match (method, path) {
        ("GET", "/health") => write_text_response(&mut stream, "200 OK", "ok\n")?,
        ("GET", "/" | "/ui") => write_ui_response(&mut stream, state)?,
        ("POST", path) if path.starts_with("/ui/delete") => {
            handle_ui_delete(&mut stream, path, state)?
        }
        ("GET" | "HEAD", path) => handle_registry_request(&mut stream, method, path, state)?,
        _ => write_text_response(&mut stream, "404 Not Found", "not found\n")?,
    }

    log(
        state.log_level,
        1,
        &format!("conn#{connection_id}"),
        &format!("responded in {:?}", started.elapsed()),
    );
    Ok(())
}

fn handle_registry_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    state: &ServerState,
) -> Result<(), Box<dyn Error>> {
    let mut target = parse_request_target(path);
    normalize_embedded_namespace(&state.config, &mut target);

    let Some(registry_request) = parse_registry_request(&target.path) else {
        write_text_response(stream, "404 Not Found", "not found\n")?;
        return Ok(());
    };

    let Some(host) = upstream_host(&state.config, target.namespace.as_deref()) else {
        write_text_response(
            stream,
            "502 Bad Gateway",
            "no matching upstream host configured\n",
        )?;
        return Ok(());
    };

    log(
        state.log_level,
        1,
        "upstream",
        &format!(
            "selected host '{}' for namespace '{}'",
            host.name,
            target.namespace.as_deref().unwrap_or("docker.io")
        ),
    );

    match registry_request {
        RegistryRequest::Ping => {
            write_registry_response(
                stream,
                "200 OK",
                "application/json",
                None,
                &[],
                method == "HEAD",
            )?;
        }
        RegistryRequest::Manifest { name, reference } => {
            let cache_path = cache_manifest_path(&state.cache_dir, host, name, reference);
            serve_cached_or_fetch(stream, method, host, &target.path, cache_path, state, true)?;
        }
        RegistryRequest::Blob { name, digest } => {
            let cache_path = cache_blob_path(&state.cache_dir, host, name, digest);
            serve_cached_or_fetch(
                stream,
                method,
                host,
                &target.path,
                cache_path,
                state,
                method == "GET",
            )?;
        }
    }

    Ok(())
}

fn serve_cached_or_fetch(
    stream: &mut TcpStream,
    method: &str,
    host: &DockerHost,
    path: &str,
    cache_path: PathBuf,
    state: &ServerState,
    should_cache_miss: bool,
) -> Result<(), Box<dyn Error>> {
    if let Some(cached) = read_cached_response(&cache_path)? {
        if is_stale(&cached, state.ttl) {
            log(
                state.log_level,
                1,
                "cache",
                &format!("expired {} -> deleting", cache_path.display()),
            );
            fs::remove_dir_all(&cache_path)?;
        } else {
            log(
                state.log_level,
                1,
                "cache",
                &format!("hit {}", cache_path.display()),
            );
            write_registry_response(
                stream,
                "200 OK",
                &cached.content_type,
                cached.digest.as_deref(),
                &cached.body,
                method == "HEAD",
            )?;
            return Ok(());
        }
    }

    log(
        state.log_level,
        1,
        "cache",
        &format!("miss {} -> {}", cache_path.display(), path),
    );

    let upstream_response =
        fetch_and_maybe_store(host, path, method, &cache_path, state, should_cache_miss)?;
    write_upstream_response(
        stream,
        upstream_response.status,
        &upstream_response.headers,
        &upstream_response.body,
        method == "HEAD",
    )?;
    Ok(())
}

fn parse_request_target(path: &str) -> RequestTarget {
    let Some((path, query)) = path.split_once('?') else {
        return RequestTarget {
            path: path.to_string(),
            namespace: None,
        };
    };

    let namespace = query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == "ns").then(|| value.to_string())
    });

    RequestTarget {
        path: path.to_string(),
        namespace,
    }
}

fn normalize_embedded_namespace(config: &Config, target: &mut RequestTarget) {
    if target.namespace.is_some() {
        return;
    }

    for host in config.hosts.iter().filter(|host| host.name != "dockerhub") {
        for namespace in host_namespaces(host) {
            let prefix = format!("/v2/{namespace}/");
            if let Some(rest) = target.path.strip_prefix(&prefix) {
                target.namespace = Some(namespace.to_string());
                target.path = format!("/v2/{rest}");
                return;
            }
        }
    }

    if let Some((namespace, rest)) = embedded_registry_namespace(&target.path) {
        target.namespace = Some(namespace.to_string());
        target.path = format!("/v2/{rest}");
    }
}

fn host_namespaces(host: &DockerHost) -> Vec<&str> {
    let mut namespaces = vec![host.name.as_str()];
    if let Some(hostname) = registry_hostname(&host.url) {
        if hostname != host.name {
            namespaces.push(hostname);
        }
    }
    namespaces
}

fn embedded_registry_namespace(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/v2/")?;
    let (namespace, rest) = rest.split_once('/')?;

    if namespace == "localhost" || namespace.contains('.') || namespace.contains(':') {
        Some((namespace, rest))
    } else {
        None
    }
}

fn parse_registry_request(path: &str) -> Option<RegistryRequest<'_>> {
    let rest = path.strip_prefix("/v2/")?;

    if rest.is_empty() {
        return Some(RegistryRequest::Ping);
    }

    if let Some((name, reference)) = rest.split_once("/manifests/") {
        return Some(RegistryRequest::Manifest { name, reference });
    }

    if let Some((name, digest)) = rest.split_once("/blobs/") {
        return Some(RegistryRequest::Blob { name, digest });
    }

    None
}

fn upstream_host<'a>(config: &'a Config, namespace: Option<&str>) -> Option<&'a DockerHost> {
    let namespace = namespace.unwrap_or("docker.io");

    if namespace == "docker.io" || namespace == "registry-1.docker.io" {
        return dockerhub_host(config);
    }

    config
        .hosts
        .iter()
        .find(|host| host_matches_namespace(host, namespace))
}

fn dockerhub_host(config: &Config) -> Option<&DockerHost> {
    config
        .hosts
        .iter()
        .find(|host| host.name == "dockerhub")
        .or_else(|| {
            config
                .hosts
                .iter()
                .find(|host| host.url.contains("registry-1.docker.io"))
        })
}

fn host_matches_namespace(host: &DockerHost, namespace: &str) -> bool {
    host.name == namespace
        || registry_hostname(&host.url)
            .map(|hostname| hostname == namespace)
            .unwrap_or(false)
}

fn registry_hostname(url: &str) -> Option<&str> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    without_scheme.split('/').next()?.split(':').next()
}

fn tunnel_connect(
    mut client: TcpStream,
    target: &str,
    connection_id: u64,
    log_level: u16,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();

    if !target.contains(':') {
        write_text_response(
            &mut client,
            "400 Bad Request",
            "CONNECT target must include a port\n",
        )?;
        return Ok(());
    }

    log(
        log_level,
        1,
        &format!("conn#{connection_id}"),
        &format!("opening CONNECT tunnel to {target}"),
    );

    let mut upstream = match TcpStream::connect(target) {
        Ok(upstream) => {
            log(
                log_level,
                1,
                &format!("conn#{connection_id}"),
                &format!("connected upstream {target} in {:?}", started.elapsed()),
            );
            upstream
        }
        Err(error) => {
            log_always(
                &format!("conn#{connection_id}"),
                &format!("failed to connect to {target}: {error}"),
            );
            write_text_response(&mut client, "502 Bad Gateway", "upstream connect failed\n")?;
            return Ok(());
        }
    };

    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    log(
        log_level,
        1,
        &format!("conn#{connection_id}"),
        "sent 200 Connection Established",
    );

    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let client_shutdown = client.try_clone()?;
    let upstream_shutdown = upstream.try_clone()?;

    let client_to_upstream = thread::spawn(move || {
        let result = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_shutdown.shutdown(Shutdown::Write);
        result
    });
    let upstream_to_client = io::copy(&mut upstream, &mut client);
    let _ = client_shutdown.shutdown(Shutdown::Write);

    match client_to_upstream.join() {
        Ok(Ok(bytes)) => log(
            log_level,
            2,
            &format!("conn#{connection_id}"),
            &format!("client -> upstream copied {bytes} bytes"),
        ),
        Ok(Err(error)) => log_always(
            &format!("conn#{connection_id}"),
            &format!("client -> upstream tunnel closed with error: {error}"),
        ),
        Err(_) => log_always(
            &format!("conn#{connection_id}"),
            "client -> upstream tunnel worker panicked",
        ),
    }

    match upstream_to_client {
        Ok(bytes) => log(
            log_level,
            2,
            &format!("conn#{connection_id}"),
            &format!("upstream -> client copied {bytes} bytes"),
        ),
        Err(error) => log_always(
            &format!("conn#{connection_id}"),
            &format!("upstream -> client tunnel closed with error: {error}"),
        ),
    }

    log(
        log_level,
        1,
        &format!("conn#{connection_id}"),
        &format!(
            "CONNECT tunnel to {target} closed after {:?}",
            started.elapsed()
        ),
    );

    Ok(())
}
