use super::ServerState;
use super::cache::{delete_cache_entry, list_cache_entries};
use super::http::{
    escape_html, format_bytes, format_duration, query_value, url_decode, url_encode,
    write_html_response, write_redirect_response, write_text_response,
};
use std::io;
use std::net::TcpStream;

pub(super) fn write_ui_response(stream: &mut TcpStream, state: &ServerState) -> io::Result<()> {
    let entries = list_cache_entries(&state.cache_dir, state.ttl)?;
    let total_size: u64 = entries.iter().map(|entry| entry.size).sum();
    let mut html = String::new();

    html.push_str(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>dockerproxy cache</title>",
    );
    html.push_str("<style>body{font-family:system-ui,sans-serif;margin:2rem;}table{border-collapse:collapse;width:100%;}th,td{border-bottom:1px solid #ddd;padding:.5rem;text-align:left;}th{background:#f6f6f6;}code{font-family:ui-monospace,monospace;}button{cursor:pointer;} .muted{color:#666;}</style>");
    html.push_str("</head><body><h1>dockerproxy cache</h1>");
    html.push_str(&format!(
        "<p class=\"muted\">{} entries, {}, ttl {}</p>",
        entries.len(),
        format_bytes(total_size),
        format_duration(state.ttl),
    ));
    html.push_str("<table><thead><tr><th>Host</th><th>Type</th><th>Repository</th><th>Reference</th><th>Size</th><th>Age</th><th>Deletes In</th><th></th></tr></thead><tbody>");

    for entry in entries {
        html.push_str("<tr>");
        html.push_str(&format!("<td>{}</td>", escape_html(&entry.host)));
        html.push_str(&format!("<td>{}</td>", escape_html(&entry.kind)));
        html.push_str(&format!(
            "<td><code>{}</code></td>",
            escape_html(&entry.repository)
        ));
        html.push_str(&format!(
            "<td><code>{}</code></td>",
            escape_html(&entry.reference)
        ));
        html.push_str(&format!("<td>{}</td>", format_bytes(entry.size)));
        html.push_str(&format!("<td>{}</td>", format_duration(entry.age_secs)));
        html.push_str(&format!(
            "<td>{}</td>",
            entry
                .expires_at
                .map(|_| format_duration(entry.ttl_remaining_secs))
                .unwrap_or_else(|| "now".to_string())
        ));
        html.push_str("<td>");
        html.push_str(&format!(
            "<form method=\"post\" action=\"/ui/delete?id={}\"><button type=\"submit\">Delete</button></form>",
            url_encode(&entry.id)
        ));
        html.push_str("</td></tr>");
    }

    html.push_str("</tbody></table></body></html>");
    write_html_response(stream, "200 OK", &html)
}

pub(super) fn handle_ui_delete(
    stream: &mut TcpStream,
    path: &str,
    state: &ServerState,
) -> io::Result<()> {
    let Some(id) = query_value(path, "id").and_then(|value| url_decode(&value)) else {
        write_text_response(stream, "400 Bad Request", "missing id\n")?;
        return Ok(());
    };

    if !delete_cache_entry(&state.cache_dir, &id, state.log_level)? {
        write_text_response(stream, "400 Bad Request", "invalid id\n")?;
        return Ok(());
    }

    write_redirect_response(stream, "/ui")
}
