use super::ServerState;
use super::http::header_to_string;
use super::logging::log;
use crate::DockerHost;
use reqwest::blocking::Response;
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

pub struct UpstreamResponse {
    pub status: StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Deserialize)]
struct TokenResponse {
    token: Option<String>,
    access_token: Option<String>,
}

pub fn fetch_and_maybe_store(
    host: &DockerHost,
    path: &str,
    method: &str,
    cache_path: &Path,
    state: &ServerState,
    should_cache_miss: bool,
) -> Result<UpstreamResponse, Box<dyn Error>> {
    let fetch_method = if should_cache_miss {
        Method::GET
    } else {
        Method::from_bytes(method.as_bytes())?
    };
    let upstream = fetch_upstream(host, path, fetch_method, state)?;
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let body = upstream.bytes()?.to_vec();

    if status.is_success() && should_cache_miss {
        let content_type = header_to_string(&headers, CONTENT_TYPE.as_str())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let digest = header_to_string(&headers, "docker-content-digest");
        write_cached_response(cache_path, &content_type, digest.as_deref(), &body)?;
        log(
            state.log_level,
            1,
            "cache",
            &format!("stored {} bytes at {}", body.len(), cache_path.display()),
        );
    }

    Ok(UpstreamResponse {
        status,
        headers,
        body,
    })
}

fn fetch_upstream(
    host: &DockerHost,
    path: &str,
    method: Method,
    state: &ServerState,
) -> Result<Response, Box<dyn Error>> {
    let url = format!("{}{}", host.url.trim_end_matches('/'), path);
    let response = upstream_request(host, &url, method.clone(), state, None)?.send()?;

    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(response);
    }

    let Some(auth_header) = response
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(response);
    };

    let Some(token) = fetch_bearer_token(host, auth_header, state)? else {
        return Ok(response);
    };

    upstream_request(host, &url, method, state, Some(&token))?
        .send()
        .map_err(|error| error.into())
}

fn upstream_request(
    host: &DockerHost,
    url: &str,
    method: Method,
    state: &ServerState,
    bearer_token: Option<&str>,
) -> Result<reqwest::blocking::RequestBuilder, Box<dyn Error>> {
    log(
        state.log_level,
        2,
        "upstream",
        &format!("{} {url}", method.as_str()),
    );

    let mut request = state
        .client
        .request(method, url)
        .header(
            ACCEPT,
            "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json, application/octet-stream",
        );

    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    } else if let (Some(username), Some(token)) = (&host.username, &host.token) {
        request = request.basic_auth(username, Some(token));
    }

    Ok(request)
}

fn fetch_bearer_token(
    host: &DockerHost,
    auth_header: &str,
    state: &ServerState,
) -> Result<Option<String>, Box<dyn Error>> {
    let Some(params) = parse_bearer_challenge(auth_header) else {
        return Ok(None);
    };
    let Some(realm) = params
        .iter()
        .find_map(|(key, value)| (*key == "realm").then_some(*value))
    else {
        return Ok(None);
    };

    let mut token_request = state.client.get(realm);
    for (key, value) in params {
        if key != "realm" {
            token_request = token_request.query(&[(key, value)]);
        }
    }

    if let (Some(username), Some(token)) = (&host.username, &host.token) {
        log(
            state.log_level,
            1,
            "upstream",
            &format!("using configured credentials for {}", host.name),
        );
        token_request = token_request.basic_auth(username, Some(token));
    }

    let token_response: TokenResponse = token_request.send()?.error_for_status()?.json()?;
    Ok(token_response.token.or(token_response.access_token))
}

fn parse_bearer_challenge(header: &str) -> Option<Vec<(&str, &str)>> {
    let challenge = header.strip_prefix("Bearer ")?;
    let mut params = Vec::new();

    for part in challenge.split(',') {
        let (key, value) = part.trim().split_once('=')?;
        params.push((key.trim(), value.trim().trim_matches('"')));
    }

    Some(params)
}

fn write_cached_response(
    path: &Path,
    content_type: &str,
    digest: Option<&str>,
    body: &[u8],
) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::write(path.join("body"), body)?;
    fs::write(path.join("content-type"), content_type)?;

    if let Some(digest) = digest {
        fs::write(path.join("digest"), digest)?;
    }

    Ok(())
}
