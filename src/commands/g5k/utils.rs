use std::{env, fs, io};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use anyhow::{anyhow, bail, Context};
use ssh2::{CheckResult, KnownHostFileKind, Session};
use tracing::{debug, info, warn};

const TARGET: &str = "g5k";

pub const G5K_ACCESS_HOST: &str = "access.grid5000.fr";
const G5K_ACCESS_PORT: u16 = 22;
pub const G5K_API_URL: &str = "https://api.grid5000.fr/stable";
const SSH_TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_CHUNK_SIZE: usize = 256 * 1024;

pub fn ensure_identifier(what: &str, value: &str, allow_upper: bool) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.chars().all(|c| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '-' | '_' | '.')
            || (allow_upper && c.is_ascii_uppercase())
    });

    if !valid {
        bail!("Invalid {what} \"{value}\": only letters, digits, '-', '_' and '.' are allowed");
    }

    Ok(())
}


pub fn connect_to_ssh(user: &str) -> anyhow::Result<Session> {
    let address = (G5K_ACCESS_HOST, G5K_ACCESS_PORT)
        .to_socket_addrs()
        .with_context(|| format!("Could not resolve {G5K_ACCESS_HOST}"))?
        .next()
        .ok_or_else(|| anyhow!("Could not resolve {G5K_ACCESS_HOST}"))?;

    let tcp = TcpStream::connect_timeout(&address, SSH_TIMEOUT)
        .with_context(|| format!("Could not connect to {G5K_ACCESS_HOST}"))?;

    let mut session = Session::new()?;
    session.set_tcp_stream(tcp);
    session.set_timeout(SSH_TIMEOUT.as_millis() as u32);
    session.handshake().context("SSH handshake failed")?;

    check_host_key(&session)?;
    authenticate(&session, user)?;

    Ok(session)
}

fn home_dir() -> anyhow::Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("Could not find your home directory"))
}

/// Verifies the server against `~/.ssh/known_hosts`, like OpenSSH does. Unknown hosts are rejected.
fn check_host_key(session: &Session) -> anyhow::Result<()> {
    let known_hosts_path = home_dir()?.join(".ssh").join("known_hosts");

    let mut known_hosts = session.known_hosts()?;

    if known_hosts_path.exists() {
        known_hosts
            .read_file(&known_hosts_path, KnownHostFileKind::OpenSSH)
            .with_context(|| format!("Could not read \"{}\"", known_hosts_path.display()))?;
    }

    let (key, _) = session.host_key().ok_or_else(|| anyhow!("The server did not present a host key"))?;

    match known_hosts.check_port(G5K_ACCESS_HOST, G5K_ACCESS_PORT, key) {
        CheckResult::Match => Ok(()),
        CheckResult::Mismatch => bail!("The host key of {G5K_ACCESS_HOST} does not match \"{}\". Refusing to connect", known_hosts_path.display()),
        CheckResult::NotFound => bail!("{G5K_ACCESS_HOST} is not in \"{}\". Run `ssh <user>@{G5K_ACCESS_HOST}` once to verify and add its host key", known_hosts_path.display()),
        CheckResult::Failure => bail!("Could not verify the host key of {G5K_ACCESS_HOST}"),
    }
}

/// Tries the SSH agent first, then the default private keys
fn authenticate(session: &Session, user: &str) -> anyhow::Result<()> {
    match session.userauth_agent(user) {
        Ok(()) if session.authenticated() => return Ok(()),
        Ok(()) => {},
        Err(e) => debug!(target: TARGET, "SSH agent authentication failed: {e}"),
    }

    let ssh_dir = home_dir()?.join(".ssh");

    for key_name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let key_path = ssh_dir.join(key_name);

        if !key_path.exists() {
            continue;
        }

        match session.userauth_pubkey_file(user, None, &key_path, None) {
            Ok(()) if session.authenticated() => return Ok(()),
            Ok(()) => {},
            Err(e) => debug!(target: TARGET, "Authentication with \"{}\" failed: {e}", key_path.display()),
        }
    }

    bail!("SSH authentication failed for \"{user}\" on {G5K_ACCESS_HOST}. Make sure your key is registered in your Grid'5000 account and loaded in your SSH agent (passphrase-protected keys need the agent)")
}

/// Runs a command and waits for it to complete, so the following steps do not race with it
pub fn ssh_exec(session: &Session, command: &str) -> anyhow::Result<String> {
    let mut channel = session.channel_session()?;
    channel.exec(command)?;

    let mut stdout = String::new();
    channel.read_to_string(&mut stdout)?;

    let mut stderr = String::new();
    channel.stderr().read_to_string(&mut stderr)?;

    channel.wait_close()?;
    let status = channel.exit_status()?;

    if status != 0 {
        bail!("Remote command \"{command}\" failed with status {status}: {}", stderr.trim());
    }

    Ok(stdout)
}

pub fn scp_upload(session: &Session, local_path: &Path, remote_path: &str, permissions: i32, always_overwrite: bool) -> anyhow::Result<()> {
    let local_size = fs::metadata(local_path)
        .with_context(|| format!("Could not read \"{}\"", local_path.display()))?
        .len();

    if !always_overwrite {
        match remote_file_size(session, remote_path)? {
            Some(remote_size) if remote_size == local_size => {
                info!(target: TARGET, "\"~/{remote_path}\" already exists ({} KiB), skipping upload", local_size / 1024);
                return Ok(());
            },
            Some(remote_size) => {
                warn!(target: TARGET, "\"~/{remote_path}\" exists but its size differs (remote {remote_size} B, local {local_size} B), uploading again");
            },
            None => {}
        }
    }

    let mut file = File::open(local_path).with_context(|| format!("Could not open \"{}\"", local_path.display()))?;
    let size = file.metadata()?.len();

    let mut channel = session.scp_send(Path::new(remote_path), permissions, size, None)?;

    let mut buffer = vec![0u8; UPLOAD_CHUNK_SIZE];
    let mut sent: u64 = 0;
    let mut last_logged_percent = 0;

    loop {
        let read = file.read(&mut buffer)?;

        if read == 0 {
            break;
        }

        channel.write_all(&buffer[..read])?;
        sent += read as u64;

        let percent = if size == 0 { 100 } else { sent * 100 / size };

        if percent >= last_logged_percent + 10 {
            last_logged_percent = percent / 10 * 10;
            info!(target: TARGET, "\t{last_logged_percent}% ({} / {} KiB)", sent / 1024, size / 1024);
        }
    }

    // Required for the remote end to acknowledge and finish writing the file
    channel.send_eof()?;
    channel.wait_eof()?;
    channel.close()?;
    channel.wait_close()?;

    Ok(())
}

/// Size in bytes of a remote regular file, or `None` if it does not exist.
/// `remote_path` is relative to the remote home directory.
fn remote_file_size(session: &Session, remote_path: &str) -> anyhow::Result<Option<u64>> {
    let remote_path = remote_path.strip_prefix("~/").unwrap_or(remote_path);

    // Always exits with 0: prints the size when the file exists and nothing otherwise
    let command = format!("f=\"$HOME\"/{}; if [ -f \"$f\" ]; then wc -c < \"$f\"; fi", shell_quote(remote_path));
    let output = ssh_exec(session, &command)?;
    let output = output.trim();

    if output.is_empty() {
        return Ok(None);
    }

    let size = output
        .parse()
        .with_context(|| format!("Unexpected size \"{output}\" for remote file \"{remote_path}\""))?;

    Ok(Some(size))
}

/// SCP can only receive single files, so every regular file of the remote directory is listed
/// and received one by one, keeping the directory structure. Existing local files are overwritten.
/// Returns the number of downloaded files and their total size in bytes.
pub fn download_dir(session: &Session, remote_dir: &str, remote_root: &Path, local_root: &Path) -> anyhow::Result<(usize, u64)> {
    let list_command = format!("find {} -type f -print0", shell_quote(remote_dir));
    let output = ssh_exec(session, &list_command)?;

    let mut files = 0;
    let mut bytes = 0;

    for remote_file in output.split('\0').filter(|path| !path.is_empty()) {
        let relative_path = Path::new(remote_file).strip_prefix(remote_root)?;
        ensure_safe_relative_path(relative_path)?;

        let local_path = local_root.join(relative_path);
        debug!(target: TARGET, "{} -> {}", remote_file, local_path.display());

        bytes += scp_download(session, remote_file, &local_path)?;
        files += 1;
    }

    // `find -type f` does not list empty directories, keep them
    let dirs_command = format!("find {} -type d -empty -print0", shell_quote(remote_dir));
    for remote_empty_dir in ssh_exec(session, &dirs_command)?.split('\0').filter(|path| !path.is_empty()) {
        let relative_path = Path::new(remote_empty_dir).strip_prefix(remote_root)?;
        ensure_safe_relative_path(relative_path)?;
        fs::create_dir_all(local_root.join(relative_path))?;
    }

    Ok((files, bytes))
}

/// Downloads one remote file over SCP. `remote_path` is relative to the remote home directory.
/// The file is written to a temporary name first, so an interrupted download never leaves a truncated result behind.
fn scp_download(session: &Session, remote_path: &str, local_path: &Path) -> anyhow::Result<u64> {
    if let Some(parent) = local_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Could not create \"{}\"", parent.display()))?;
    }

    let (mut channel, stat) = session
        .scp_recv(Path::new(remote_path))
        .with_context(|| format!("Could not receive \"~/{remote_path}\""))?;

    let mut temporary_path: PathBuf = local_path.to_path_buf();
    temporary_path.as_mut_os_string().push(".part");

    let mut file = File::create(&temporary_path).with_context(|| format!("Could not create \"{}\"", temporary_path.display()))?;
    let copied = io::copy(&mut channel, &mut file)?;

    // Required for the remote end to finish the transfer
    channel.send_eof()?;
    channel.wait_eof()?;
    channel.close()?;
    channel.wait_close()?;

    if copied != stat.size() {
        let _ = fs::remove_file(&temporary_path);
        bail!("Incomplete download of \"~/{remote_path}\": got {copied} of {} bytes", stat.size());
    }

    file.sync_all()?;
    drop(file);
    fs::rename(&temporary_path, local_path)?;

    Ok(copied)
}

/// Only plain components (no `..`, no root, no prefix) so a remote path can never escape the local results directory
pub fn ensure_safe_relative_path(path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty() || !path.components().all(|component| matches!(component, Component::Normal(_))) {
        bail!("Refusing unsafe remote path \"{}\"", path.display());
    }

    Ok(())
}


/// Single-quotes a string for a POSIX shell
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
