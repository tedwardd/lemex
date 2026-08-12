use crate::input::Mode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpItem {
    pub key: &'static str,
    pub action: &'static str,
}

/// One searchable help entry in the command index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpEntry {
    pub command: &'static str,
    pub description: &'static str,
    pub group: &'static str,
}

/// Searchable command help. `contains` answers "is this command documented";
/// `search` returns the entries matching a free-text query across command,
/// description, and group.
#[derive(Clone, Copy, Debug)]
pub struct HelpIndex {
    entries: &'static [HelpEntry],
}

impl Default for HelpIndex {
    fn default() -> Self {
        Self { entries: HELP_ENTRIES }
    }
}

impl HelpIndex {
    pub fn contains(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.entries.iter().any(|entry| {
            entry.command.to_lowercase().contains(&needle)
                || entry.group.to_lowercase().contains(&needle)
        })
    }

    pub fn search(&self, query: &str) -> Vec<&'static HelpEntry> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.entries.iter().collect();
        }
        self.entries
            .iter()
            .filter(|entry| {
                entry.command.to_lowercase().contains(&query)
                    || entry.description.to_lowercase().contains(&query)
                    || entry.group.to_lowercase().contains(&query)
            })
            .collect()
    }
}

static HELP_ENTRIES: &[HelpEntry] = &[
    // Profile management
    HelpEntry { command: ":profile", description: "list configured profiles (active marked)", group: "profile" },
    HelpEntry { command: ":profile <name>", description: "switch to a profile; a hard context transition", group: "profile" },
    HelpEntry { command: ":profile-new <id> <instance-url> [label]", description: "create or replace a profile", group: "profile" },
    HelpEntry { command: ":profile-delete <id>", description: "delete a profile and its stored session", group: "profile" },
    HelpEntry { command: ":login <username> <password>", description: "log in; the session is stored in the OS credential store only after success", group: "profile" },
    HelpEntry { command: ":logout", description: "log out; keeps non-secret profile metadata", group: "profile" },
    HelpEntry { command: ":whoami", description: "show the active session user or anonymous", group: "profile" },
    // Navigation
    HelpEntry { command: ":feed", description: "show the home feed", group: "navigation" },
    HelpEntry { command: ":community [<id>]", description: "open a community feed (defaults to the selected post's community)", group: "navigation" },
    HelpEntry { command: ":post", description: "open the selected post", group: "navigation" },
    HelpEntry { command: ":search <query>", description: "search posts (filters download history with the panel open)", group: "navigation" },
    HelpEntry { command: ":open", description: "open the selected post", group: "navigation" },
    HelpEntry { command: ":refresh", description: "refresh the current view", group: "navigation" },
    HelpEntry { command: "j/k", description: "move down/up the selection", group: "navigation" },
    HelpEntry { command: "Esc", description: "back / cancel", group: "navigation" },
    // Media
    HelpEntry { command: ":media", description: "open the selected post's media through the configured handler", group: "media" },
    HelpEntry { command: ":download-media", description: "download the selected post's media", group: "media" },
    // Download history
    HelpEntry { command: ":downloads", description: "open (or close) the current-session download history", group: "downloads" },
    HelpEntry { command: ":downloads search <query>", description: "filter download history", group: "downloads" },
    HelpEntry { command: ":downloads reopen", description: "reopen the selected download", group: "downloads" },
    HelpEntry { command: ":downloads reveal", description: "reveal the download directory", group: "downloads" },
    HelpEntry { command: ":downloads copy", description: "copy the download path", group: "downloads" },
    HelpEntry { command: ":downloads retry", description: "retry a failed download", group: "downloads" },
    HelpEntry { command: ":downloads cancel", description: "cancel an in-flight download", group: "downloads" },
    HelpEntry { command: ":downloads delete", description: "delete a completed download's local file", group: "downloads" },
    HelpEntry { command: ":downloads overwrite|keep", description: "resolve a collision prompt", group: "downloads" },
    // Mutations
    HelpEntry { command: ":reply <text>", description: "reply to the selected post", group: "mutation" },
    HelpEntry { command: ":edit <title>", description: "retitle the selected post", group: "mutation" },
    HelpEntry { command: ":delete", description: "delete the selected post (download with the panel open)", group: "mutation" },
    HelpEntry { command: ":vote <score>", description: "vote on the selected post", group: "mutation" },
    HelpEntry { command: ":save", description: "save the selected post", group: "mutation" },
    HelpEntry { command: ":subscribe", description: "subscribe to the selected post's community", group: "mutation" },
    // Configuration
    HelpEntry { command: ":set keymap <name> <keys>", description: "configure a key mapping (applied on next launch)", group: "config" },
    HelpEntry { command: ":set media kitty on|off", description: "toggle kitty media rendering", group: "config" },
    HelpEntry { command: ":set media mailcap on|off", description: "toggle mailcap handler use", group: "config" },
    HelpEntry { command: ":set download-dir <path>", description: "set the download directory", group: "config" },
    HelpEntry { command: ":set collision-policy <prompt|overwrite|unique-name>", description: "set the download collision policy", group: "config" },
    HelpEntry { command: ":set cache-dir <path>", description: "set the cache directory (next launch)", group: "config" },
    HelpEntry { command: ":set cache-size <bytes>", description: "set the cache size limit (next launch)", group: "config" },
    HelpEntry { command: ":set logging on|off [level]", description: "toggle opt-in logging (redacts secrets)", group: "config" },
    // Session
    HelpEntry { command: ":help [topic]", description: "show searchable help; filter by topic", group: "session" },
    HelpEntry { command: ":quit", description: "quit the client", group: "session" },
];

pub fn contextual_help(mode: Mode) -> &'static [HelpItem] {
    match mode {
        Mode::Normal => &[
            HelpItem { key: "j/k", action: "move" },
            HelpItem { key: "Enter", action: "open" },
            HelpItem { key: "r", action: "refresh" },
            HelpItem { key: "i", action: "compose" },
            HelpItem { key: "q", action: "quit" },
        ],
        Mode::Insert => &[
            HelpItem { key: "text", action: "edit draft" },
            HelpItem { key: "Esc", action: "normal mode" },
        ],
        Mode::Visual => &[
            HelpItem { key: "j/k", action: "extend selection" },
            HelpItem { key: "Esc", action: "normal mode" },
        ],
        Mode::Command => &[
            HelpItem { key: "text", action: "enter command" },
            HelpItem { key: "Enter", action: "run command" },
            HelpItem { key: "Esc", action: "cancel" },
        ],
        Mode::SearchForward => &[
            HelpItem { key: "text", action: "search forward" },
            HelpItem { key: "Enter", action: "run search" },
            HelpItem { key: "Esc", action: "cancel" },
        ],
        Mode::SearchBackward => &[
            HelpItem { key: "text", action: "search backward" },
            HelpItem { key: "Enter", action: "run search" },
            HelpItem { key: "Esc", action: "cancel" },
        ],
    }
}

pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::Visual => "VISUAL",
        Mode::Command => "COMMAND",
        Mode::SearchForward => "SEARCH FORWARD",
        Mode::SearchBackward => "SEARCH BACKWARD",
    }
}
