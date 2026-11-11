//! SSH/SFTP access to a checkout that stays on another machine.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ssh2::{CheckResult, HashType, KnownHostFileKind, OpenFlags, OpenType, Session as RawSession};

/// An ordinary `Host` entry in the user's SSH configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
}

/// Connection details. The password is intentionally not represented here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub host: String,
    pub user: String,
    pub port: u16,
}

/// The non-secret part of a remote checkout.  Passwords and private-key
/// passphrases deliberately have no place in this type or in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDescriptor {
    pub login: Login,
    pub path: String,
}

impl RemoteDescriptor {
    pub fn new(login: Login, path: String) -> Result<Self> {
        anyhow::ensure!(
            path.starts_with('/'),
            "the repository path must be absolute"
        );
        Ok(Self { login, path })
    }
}

impl Login {
    pub fn new(host: String, user: String, port: u16) -> Result<Self> {
        anyhow::ensure!(!host.trim().is_empty(), "the SSH host is missing");
        anyhow::ensure!(!user.trim().is_empty(), "the SSH user is missing");
        Ok(Self { host, user, port })
    }
}

/// An open, authenticated connection. It holds its TCP socket until dropped.
pub struct Session {
    login: Login,
    raw: RawSession,
}

impl Session {
    /// Connect using the supplied password, or the SSH agent when it is blank.
    /// The caller owns the password and can clear it as soon as this returns.
    pub fn connect(login: Login, password: Option<&str>, trust_new_host: bool) -> Result<Self> {
        Self::connect_inner(login, password, None, trust_new_host, None)
    }

    /// Connect with an explicit private key and known-hosts file. This is
    /// useful for isolated transport tests without borrowing the user's SSH
    /// agent or their `known_hosts` file.
    pub fn connect_with_key(login: Login, private_key: &Path, known_hosts: &Path, trust_new_host: bool) -> Result<Self> {
        Self::connect_inner(login, None, Some(private_key), trust_new_host, Some(known_hosts))
    }

    fn connect_inner(login: Login, password: Option<&str>, private_key: Option<&Path>, trust_new_host: bool, known_hosts: Option<&Path>) -> Result<Self> {
        let tcp = TcpStream::connect((login.host.as_str(), login.port))
            .with_context(|| format!("could not reach {}:{}", login.host, login.port))?;
        tcp.set_read_timeout(Some(Duration::from_secs(15)))
            .context("could not set the SSH read timeout")?;
        tcp.set_write_timeout(Some(Duration::from_secs(15)))
            .context("could not set the SSH write timeout")?;
        let mut raw = RawSession::new().context("could not prepare SSH")?;
        raw.set_tcp_stream(tcp);
        raw.handshake().context("the SSH handshake failed")?;
        verify_host(&raw, &login, trust_new_host, known_hosts)?;

        match (password.filter(|password| !password.is_empty()), private_key) {
            (Some(password), _) => raw
                .userauth_password(&login.user, password)
                .context("the SSH password was refused")?,
            (None, Some(private_key)) => raw
                .userauth_pubkey_file(&login.user, None, private_key, None)
                .context("the SSH key was refused")?,
            (None, None) => raw
                .userauth_agent(&login.user)
                .context("could not authenticate with the SSH agent")?,
        }
        anyhow::ensure!(
            raw.authenticated(),
            "the SSH server did not authenticate you"
        );
        Ok(Self { login, raw })
    }

    /// Lists immediate folders over SFTP. Returned paths are absolute server
    /// paths, ready to be passed back into this method to descend.
    pub fn folders(&self, path: &str) -> Result<Vec<String>> {
        anyhow::ensure!(path.starts_with('/'), "remote folders must be absolute");
        let sftp = self.raw.sftp().context("could not start SFTP")?;
        let mut folders: Vec<String> = sftp
            .readdir(Path::new(path))
            .with_context(|| format!("could not read {path}"))?
            .into_iter()
            .filter(|(_, stat)| stat.is_dir())
            .map(|(path, _)| path.display().to_string())
            .collect();
        folders.sort();
        Ok(folders)
    }

    /// Checks that a selected folder is a non-bare Git checkout. It does not
    /// read or copy any file from that checkout.
    pub fn verify_checkout(&self, path: &str) -> Result<()> {
        anyhow::ensure!(
            path.starts_with('/'),
            "the repository path must be absolute"
        );
        let output = self.command(&["git", "-C", path, "rev-parse", "--is-inside-work-tree"])?;
        anyhow::ensure!(output.trim() == "true", "{path} is not a Git working tree");
        Ok(())
    }

    pub fn login(&self) -> &Login {
        &self.login
    }

    /// Reads a working-tree file over SFTP. The caller decides whether its
    /// bytes are text; SSH never creates a local copy.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let sftp = self.raw.sftp().context("could not start SFTP")?;
        let mut file = sftp.open(Path::new(path)).with_context(|| format!("could not read {path}"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).with_context(|| format!("could not read {path}"))?;
        Ok(bytes)
    }

    /// Replaces a working-tree file over SFTP. Git index changes remain Git
    /// commands and are deliberately not hidden in this transport method.
    pub fn write_file(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let sftp = self.raw.sftp().context("could not start SFTP")?;
        let mut file = sftp.open_mode(Path::new(path), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE, 0o644, OpenType::File)
            .with_context(|| format!("could not write {path}"))?;
        file.write_all(bytes).with_context(|| format!("could not write {path}"))?;
        Ok(())
    }

    pub fn exists(&self, path: &str) -> bool {
        self.raw.sftp().and_then(|sftp| sftp.stat(Path::new(path))).is_ok()
    }

    pub(crate) fn command(&self, words: &[&str]) -> Result<String> {
        let command = words
            .iter()
            .map(|word| quote(word))
            .collect::<Vec<_>>()
            .join(" ");
        let mut channel = self
            .raw
            .channel_session()
            .context("could not open an SSH channel")?;
        channel
            .exec(&command)
            .context("could not run the command on the SSH server")?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        channel.read_to_string(&mut stdout)?;
        channel.stderr().read_to_string(&mut stderr)?;
        channel.wait_close()?;
        if channel.exit_status()? != 0 {
            bail!(
                "{}",
                if stderr.trim().is_empty() {
                    "the remote command failed"
                } else {
                    stderr.trim()
                }
            );
        }
        Ok(stdout)
    }
}

/// Refuse unknown or changed servers. The UI will later offer an explicit
/// trust decision; silently adding a key here would make a first connection
/// vulnerable to a machine-in-the-middle attack.
fn verify_host(raw: &RawSession, login: &Login, trust_new_host: bool, known_hosts: Option<&Path>) -> Result<()> {
    let file = match known_hosts {
        Some(file) => file.to_path_buf(),
        None => {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
            let Some(home) = home else {
                bail!("could not find known_hosts to verify {}", login.host)
            };
            PathBuf::from(home).join(".ssh").join("known_hosts")
        }
    };
    let mut known = raw.known_hosts().context("could not read known_hosts")?;
    // An absent file is the normal first-connection case. `check` below will
    // report it as an unknown server and only the explicit UI choice may then
    // create the file.
    if file.exists() {
        known
            .read_file(&file, KnownHostFileKind::OpenSSH)
            .context("could not read known_hosts")?;
    }
    let (key, kind) = raw
        .host_key()
        .context("the SSH server did not provide a host key")?;
    let name = if login.port == 22 {
        login.host.clone()
    } else {
        format!("[{}]:{}", login.host, login.port)
    };
    match known.check(&name, key) {
        CheckResult::Match => Ok(()),
        CheckResult::Mismatch => bail!("the host key for {} has changed", name),
        CheckResult::NotFound if trust_new_host => {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)
                    .context("could not create the SSH settings folder")?;
            }
            known
                .add(&name, key, &name, kind.into())
                .context("could not remember the new host key")?;
            known
                .write_file(&file, KnownHostFileKind::OpenSSH)
                .context("could not save known_hosts")?;
            Ok(())
        }
        CheckResult::NotFound => bail!(
            "{} is not in known_hosts (SHA-256 fingerprint: {})",
            name,
            fingerprint(raw)
        ),
        CheckResult::Failure => bail!("could not verify the host key for {}", name),
    }
}

fn fingerprint(raw: &RawSession) -> String {
    raw.host_key_hash(HashType::Sha256)
        .map(|hash| hash.iter().map(|byte| format!("{byte:02x}")).collect())
        .unwrap_or_else(|| "unavailable".to_string())
}

/// Reads aliases from `~/.ssh/config`, or `%USERPROFILE%\\.ssh\\config` when
/// HOME is unavailable (as is common on native Windows).
pub fn configured_hosts() -> Result<Vec<Host>> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("could not find your home folder")?;
    let path = PathBuf::from(home).join(".ssh").join("config");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse_hosts(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", path.display())),
    }
}

fn parse_hosts(text: &str) -> Vec<Host> {
    let mut hosts = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else { continue };
        let value = words.collect::<Vec<_>>().join(" ");
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                hosts.append(&mut current);
                current = value
                    .split_whitespace()
                    .filter(|alias| !alias.contains(['*', '?', '!']))
                    .map(|alias| Host {
                        alias: alias.to_string(),
                        hostname: None,
                        user: None,
                        port: None,
                    })
                    .collect();
            }
            "hostname" => current
                .iter_mut()
                .for_each(|host| host.hostname = Some(value.clone())),
            "user" => current
                .iter_mut()
                .for_each(|host| host.user = Some(value.clone())),
            "port" => {
                if let Ok(port) = value.parse() {
                    current.iter_mut().for_each(|host| host.port = Some(port))
                }
            }
            _ => {}
        }
    }
    hosts.append(&mut current);
    hosts
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_keep_their_connection_details() {
        let hosts = parse_hosts(
            "Host forge staging *.ignored\n HostName git.example\n User jane\n Port 2222\n",
        );
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].hostname.as_deref(), Some("git.example"));
        assert_eq!(hosts[1].user.as_deref(), Some("jane"));
        assert_eq!(hosts[0].port, Some(2222));
    }

    #[test]
    fn login_needs_a_host_and_user() {
        assert!(Login::new("".into(), "jane".into(), 22).is_err());
        assert!(Login::new("host".into(), "".into(), 22).is_err());
    }

    #[test]
    fn command_words_are_kept_separate() {
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\"'\"'s'");
    }
}
