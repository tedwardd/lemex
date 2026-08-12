use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// One parsed mailcap entry: a MIME type and its command template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailcapEntry {
    pub mime_type: String,
    pub command: String,
}

/// Parse a mailcap document without evaluating any shell logic.
///
/// Backslash line continuations are joined, comment lines are skipped, and
/// fields after the command (`test=...`, `needsterminal`, `description=...`)
/// are ignored — a `test=` predicate is never run through a shell.
pub fn parse_mailcap(source: &str) -> Vec<MailcapEntry> {
    let mut entries = Vec::new();
    let mut current = String::new();
    for raw_line in source.lines() {
        let line = raw_line.trim_end();
        if line.trim_start().starts_with('#') && current.is_empty() {
            continue;
        }
        if line.ends_with('\\') {
            current.push_str(&line[..line.len() - 1]);
            continue;
        }
        current.push_str(line);
        if let Some(entry) = parse_mailcap_line(&current) {
            entries.push(entry);
        }
        current.clear();
    }
    if !current.trim().is_empty() {
        if let Some(entry) = parse_mailcap_line(&current) {
            entries.push(entry);
        }
    }
    entries
}

fn parse_mailcap_line(line: &str) -> Option<MailcapEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let fields = split_fields(line);
    let mime_type = fields.first()?.trim();
    let command = fields.get(1)?.trim();
    if mime_type.is_empty() || command.is_empty() || !mime_type.contains('/') {
        return None;
    }
    Some(MailcapEntry {
        mime_type: mime_type.to_owned(),
        command: command.to_owned(),
    })
}

/// Split a mailcap line on top-level `;` separators, respecting quotes.
fn split_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in line.chars() {
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                }
                current.push(character);
            }
            None => match character {
                '"' | '\'' => {
                    quote = Some(character);
                    current.push(character);
                }
                ';' => {
                    fields.push(std::mem::take(&mut current));
                }
                _ => current.push(character),
            },
        }
    }
    fields.push(current);
    fields
}

/// Exact MIME match first, then the `type/*` wildcard entry.
pub fn find_entry<'a>(entries: &'a [MailcapEntry], mime: &str) -> Option<&'a MailcapEntry> {
    if let Some(entry) = entries.iter().find(|entry| entry.mime_type == mime) {
        return Some(entry);
    }
    let mtype = mime.split_once('/').map(|(mtype, _)| mtype)?;
    entries.iter().find(|entry| entry.mime_type == format!("{mtype}/*"))
}

/// Load mailcap entries from `$MAILCAPS` or the standard `~/.mailcap` and
/// `/etc/mailcap` locations. Missing files are not an error.
pub fn load_entries() -> Vec<MailcapEntry> {
    let mut paths = Vec::new();
    if let Ok(configured) = std::env::var("MAILCAPS") {
        for candidate in configured.split(':').filter(|candidate| !candidate.is_empty()) {
            paths.push(PathBuf::from(candidate));
        }
    } else {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".mailcap"));
        }
        paths.push(PathBuf::from("/etc/mailcap"));
    }
    let mut entries = Vec::new();
    for path in paths {
        if let Ok(source) = fs::read_to_string(&path) {
            entries.extend(parse_mailcap(&source));
        }
    }
    entries
}

/// Build a safe argv from a mailcap command template.
///
/// No shell is ever involved: the template is tokenized with shell-like quote
/// handling, `%s`/`%t` placeholders are substituted with the file and MIME
/// values, and `%%` becomes a literal `%`. Unknown `%x` sequences remain
/// literal. If the template has no `%s`, the file is appended as the final
/// argument, matching mailcap convention.
pub fn build_argv(template: &str, file: impl AsRef<std::ffi::OsStr>, mime: &str) -> Vec<OsString> {
    let file = file.as_ref();
    let tokens = tokenize(template);
    let mut argv = Vec::with_capacity(tokens.len() + 1);
    let mut has_file = false;
    for token in tokens {
        if token == "%s" {
            argv.push(file.to_os_string());
            has_file = true;
        } else if token == "%t" {
            argv.push(OsString::from(mime));
        } else if token.contains("%s") {
            argv.push(OsString::from(token.replace("%s", &file.to_string_lossy())));
            has_file = true;
        } else if token.contains("%t") {
            argv.push(OsString::from(token.replace("%t", mime)));
        } else {
            argv.push(OsString::from(token.replace("%%", "%")));
        }
    }
    if !has_file {
        argv.push(file.to_os_string());
    }
    argv
}

/// Tokenize a command template into words, honoring single/double quotes and
/// backslash escapes without any shell expansion.
fn tokenize(template: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in template.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                } else if character == '\\' && active == '"' {
                    escaped = true;
                } else {
                    current.push(character);
                }
            }
            None => match character {
                '\\' => escaped = true,
                '"' | '\'' => quote = Some(character),
                whitespace if whitespace.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                other => current.push(other),
            },
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Best-effort removal of a stale temp file; used by cancellation/cleanup.
pub(crate) fn remove_temporary(target: &Path, id: crate::domain::DownloadId) {
    let _ = fs::remove_file(temporary_path(target, id));
}

/// Temp path used while a download streams to disk. Deterministic per
/// (target, download id) so cancellation and cleanup can find it.
pub(crate) fn temporary_path(target: &Path, id: crate::domain::DownloadId) -> PathBuf {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    parent.join(format!(".{name}.part-{}", id.0))
}
