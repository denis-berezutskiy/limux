//! SSH host book for the connection manager.
//!
//! Two sources feed the "SSH HOSTS" sidebar section:
//!   * user-managed hosts saved to `~/.config/limux/ssh_hosts.json`
//!   * hosts discovered by parsing `~/.ssh/config`
//!
//! An [`SshHost`] is the address-book entry the UI edits; [`SshHost::to_connection`]
//! turns it into the [`SshConnection`] a terminal tab persists and reconnects with.
//!
//! Pure logic lives here (parser, (de)serialization, mapping) so it can be unit
//! tested without any GTK wiring — matching the project's "pure logic separate
//! from GTK" convention.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout_state::SshConnection;

const SSH_HOSTS_FILE_NAME: &str = "ssh_hosts.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SshHost {
    /// Display name; also the default remote tmux session name.
    pub alias: String,
    /// The real hostname or IP to connect to.
    pub host_name: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub proxy_jump: Option<String>,
    #[serde(default = "default_true")]
    pub persist_tmux: bool,
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Opt-in: on connect, upload the local `limux` CLI to the remote and run
    /// `limux hooks setup` there so a remote coding agent's notifications reach
    /// this machine automatically. Off by default (does extra work on connect).
    #[serde(default)]
    pub provision_notifications: bool,
    /// True when discovered from `~/.ssh/config` rather than user-saved. These
    /// are shown in the sidebar but not written back to `ssh_hosts.json`.
    #[serde(default, skip_serializing)]
    pub from_ssh_config: bool,
}

fn default_true() -> bool {
    true
}

impl SshHost {
    pub fn new(alias: impl Into<String>, host_name: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            host_name: host_name.into(),
            user: None,
            port: None,
            identity_file: None,
            proxy_jump: None,
            persist_tmux: true,
            auto_reconnect: true,
            provision_notifications: false,
            from_ssh_config: false,
        }
    }

    /// Map an address-book entry to the connection a tab persists/reconnects with.
    ///
    /// For a host discovered from `~/.ssh/config`, connect through the **alias
    /// itself** (`ssh <alias>`) rather than the parsed fields, so OpenSSH applies
    /// the entire matching `Host` block — `HostName`, `User`, `Port`,
    /// `IdentityFile`, `ProxyJump`, plus `Include` / `Match` / tokens / arbitrary
    /// options that our lightweight sidebar parser deliberately doesn't model. The
    /// parsed fields still populate the sidebar row for display; they just don't
    /// drive the connection, so a config host always resolves exactly as the
    /// user's own `ssh <alias>` would. User-saved hosts keep their explicit fields.
    pub fn to_connection(&self) -> SshConnection {
        if self.from_ssh_config {
            return SshConnection {
                host: self.alias.clone(),
                user: None,
                port: None,
                identity_file: None,
                proxy_jump: None,
                remote_session_name: normalize(Some(self.alias.as_str())),
                persist_tmux: self.persist_tmux,
                auto_reconnect: self.auto_reconnect,
            };
        }
        SshConnection {
            host: self.host_name.clone(),
            user: normalize(self.user.as_deref()),
            port: self.port,
            identity_file: normalize(self.identity_file.as_deref()),
            proxy_jump: normalize(self.proxy_jump.as_deref()),
            remote_session_name: normalize(Some(self.alias.as_str())),
            persist_tmux: self.persist_tmux,
            auto_reconnect: self.auto_reconnect,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct SshHostBook {
    #[serde(default)]
    hosts: Vec<SshHost>,
}

fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("limux").join(SSH_HOSTS_FILE_NAME))
}

/// User-saved hosts (from `ssh_hosts.json`). Missing/corrupt file → empty.
pub fn load_saved_hosts() -> Vec<SshHost> {
    let Some(path) = config_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<SshHostBook>(&text)
        .map(|book| book.hosts)
        .unwrap_or_default()
}

/// Persist user hosts atomically (temp file + rename), mirroring the session store.
pub fn save_hosts(hosts: &[SshHost]) -> io::Result<()> {
    let path = config_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let book = SshHostBook {
        hosts: hosts
            .iter()
            .filter(|h| !h.from_ssh_config)
            .cloned()
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&book).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Hosts discovered from `~/.ssh/config` (never fails; missing file → empty).
pub fn load_ssh_config_hosts() -> Vec<SshHost> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(home.join(".ssh").join("config")) {
        Ok(text) => parse_ssh_config(&text),
        Err(_) => Vec::new(),
    }
}

/// Merged view for the sidebar: saved hosts first, then `~/.ssh/config` hosts
/// whose alias isn't already user-saved.
pub fn all_hosts() -> Vec<SshHost> {
    let mut hosts = load_saved_hosts();
    let seen: HashSet<String> = hosts.iter().map(|h| h.alias.clone()).collect();
    for host in load_ssh_config_hosts() {
        if !seen.contains(&host.alias) {
            hosts.push(host);
        }
    }
    hosts
}

/// Minimal `~/.ssh/config` parser: collects `Host` blocks and the fields we use.
/// Wildcard-only `Host` patterns (e.g. `Host *`) are skipped. Handles both
/// `Key Value` and `Key=Value`, is case-insensitive on keys, and expands a
/// leading `~/` in IdentityFile.
pub fn parse_ssh_config(text: &str) -> Vec<SshHost> {
    let mut hosts = Vec::new();
    let mut current: Option<SshHost> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = split_key_value(line);
        if value.is_empty() {
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                if let Some(host) = current.take() {
                    hosts.push(host);
                }
                // A Host line may list several patterns; use the first concrete
                // (non-wildcard) one as the alias/hostname default.
                let alias = value
                    .split_whitespace()
                    .find(|pat| !pat.contains('*') && !pat.contains('?'));
                current = alias.map(|alias| {
                    let mut host = SshHost::new(alias, alias);
                    host.from_ssh_config = true;
                    host
                });
            }
            "hostname" => {
                if let Some(host) = current.as_mut() {
                    host.host_name = value.to_string();
                }
            }
            "user" => {
                if let Some(host) = current.as_mut() {
                    host.user = Some(value.to_string());
                }
            }
            "port" => {
                if let Some(host) = current.as_mut() {
                    host.port = value.parse().ok();
                }
            }
            "identityfile" => {
                if let Some(host) = current.as_mut() {
                    host.identity_file = Some(expand_tilde(value));
                }
            }
            "proxyjump" => {
                if let Some(host) = current.as_mut() {
                    host.proxy_jump = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(host) = current.take() {
        hosts.push(host);
    }
    // Drop any host whose alias/hostname/user/proxy begins with `-`. Connecting
    // reaches these values into ssh/scp/tmux argument vectors, where a `-`-leading
    // token is parsed as an option (e.g. `-oProxyCommand=…` → local command
    // execution). A hostile or malformed `~/.ssh/config` must not be able to plant
    // one just by sitting on disk.
    hosts.retain(|host| {
        !dash_leading(Some(&host.alias))
            && !dash_leading(Some(&host.host_name))
            && !dash_leading(host.user.as_deref())
            && !dash_leading(host.proxy_jump.as_deref())
    });
    hosts
}

/// True when a value begins with `-` (and would therefore be misparsed as an
/// ssh/scp/tmux option). Empty/absent values are safe.
fn dash_leading(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.starts_with('-'))
}

fn split_key_value(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let key = &line[..end];
    let value = line[end..]
        .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
        .trim();
    (key, value)
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_and_fields() {
        let cfg = "\
# a comment
Host dev
    HostName 10.0.0.5
    User alice
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host prod
    HostName prod.example.com
    ProxyJump bastion
";
        let hosts = parse_ssh_config(cfg);
        assert_eq!(hosts.len(), 2);

        let dev = &hosts[0];
        assert_eq!(dev.alias, "dev");
        assert_eq!(dev.host_name, "10.0.0.5");
        assert_eq!(dev.user.as_deref(), Some("alice"));
        assert_eq!(dev.port, Some(2222));
        assert!(dev
            .identity_file
            .as_deref()
            .unwrap()
            .ends_with(".ssh/id_ed25519"));
        assert!(!dev.identity_file.as_deref().unwrap().starts_with('~'));
        assert!(dev.from_ssh_config);

        let prod = &hosts[1];
        assert_eq!(prod.host_name, "prod.example.com");
        assert_eq!(prod.proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn skips_wildcard_only_blocks() {
        let cfg = "Host *\n    ForwardAgent yes\nHost real\n    HostName r.example.com\n";
        let hosts = parse_ssh_config(cfg);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "real");
    }

    #[test]
    fn accepts_equals_syntax() {
        let hosts = parse_ssh_config("Host=box\nHostName=192.168.1.9\n");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host_name, "192.168.1.9");
    }

    #[test]
    fn maps_to_connection_and_builds_tmux_command() {
        let mut host = SshHost::new("dev", "10.0.0.5");
        host.user = Some("alice".to_string());
        host.port = Some(2222);
        let conn = host.to_connection();
        assert_eq!(conn.host, "10.0.0.5");
        assert_eq!(conn.user.as_deref(), Some("alice"));
        assert_eq!(conn.remote_session_name.as_deref(), Some("dev"));

        let cmd = conn.reconnect_command();
        assert!(cmd.contains("ssh"));
        assert!(cmd.contains("'alice@10.0.0.5'"));
        assert!(cmd.contains("-p") && cmd.contains("2222"));
        assert!(cmd.contains("tmux new-session -A -s dev"));
        // `-gq` (not `-g`): the quiet flag keeps tmux < 3.3, which lacks the
        // allow-passthrough option, from erroring when the session is created.
        assert!(cmd.contains("set -gq allow-passthrough on"));
        assert!(cmd.contains("command -v tmux"));
        assert!(cmd.contains("TERM=xterm-256color"));
        assert!(cmd.contains("ServerAliveInterval"));
    }

    #[test]
    fn config_host_connects_via_alias_not_parsed_fields() {
        // A ~/.ssh/config-discovered host connects through the alias so OpenSSH
        // applies the whole Host block; the parsed fields are display-only.
        let mut host = SshHost::new("myvm", "10.0.0.9");
        host.user = Some("bob".to_string());
        host.port = Some(2200);
        host.from_ssh_config = true;

        let conn = host.to_connection();
        assert_eq!(conn.host, "myvm");
        assert_eq!(conn.user, None);
        assert_eq!(conn.port, None);
        assert_eq!(conn.identity_file, None);
        assert_eq!(conn.remote_session_name.as_deref(), Some("myvm"));

        let cmd = conn.reconnect_command();
        assert!(cmd.contains("'myvm'"), "cmd={cmd}");
        // The parsed hostname/user/port must NOT drive the connection.
        assert!(!cmd.contains("10.0.0.9"), "cmd={cmd}");
        assert!(!cmd.contains("bob@"), "cmd={cmd}");

        // A user-saved host (not from config) still uses its explicit fields.
        let mut saved = SshHost::new("myvm", "10.0.0.9");
        saved.user = Some("bob".to_string());
        let saved = saved.to_connection();
        assert_eq!(saved.host, "10.0.0.9");
        assert_eq!(saved.user.as_deref(), Some("bob"));
    }

    #[test]
    fn parse_ssh_config_drops_dash_leading_injection_hosts() {
        // A hostile config whose Host pattern or HostName begins with `-` would be
        // misparsed as an ssh option when we connect — drop such entries entirely.
        let cfg = "\
Host good
    HostName 10.0.0.1
Host -oProxyCommand=touch\x20pwned
    HostName x
Host evil
    HostName -oProxyCommand=touch\x20pwned
";
        let hosts = parse_ssh_config(cfg);
        let aliases: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["good"]);
        assert!(!hosts.iter().any(|h| h.alias.starts_with('-')));
        assert!(!hosts.iter().any(|h| h.host_name.starts_with('-')));
    }

    #[test]
    fn provision_notifications_defaults_off_and_survives_absent_json() {
        // Opt-in: a freshly-built host and an older ssh_hosts.json that predates
        // the field must both read as false, never surprising a user with uploads.
        assert!(!SshHost::new("dev", "h").provision_notifications);
        let host: SshHost =
            serde_json::from_str(r#"{"alias":"dev","host_name":"h"}"#).expect("decode");
        assert!(!host.provision_notifications);

        let mut opted_in = SshHost::new("dev", "h");
        opted_in.provision_notifications = true;
        let json = serde_json::to_string(&opted_in).expect("encode");
        let round: SshHost = serde_json::from_str(&json).expect("decode round-trip");
        assert!(round.provision_notifications);
    }

    #[test]
    fn tmux_session_name_is_sanitized() {
        let mut host = SshHost::new("my.box:1", "h");
        host.persist_tmux = true;
        let conn = host.to_connection();
        // '.' and ':' are not allowed by tmux and must be replaced.
        assert_eq!(conn.tmux_session_name(), "my_box_1");
    }
}
