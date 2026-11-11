//! End-to-end coverage of the SSH transport.  It is opt-in because it starts
//! an OpenSSH daemon, but it has no dependency on a developer's SSH agent,
//! account keys, or known_hosts file.

use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::Duration,
};

use gitrust_core::{
    ssh::{Login, Session},
    Half, PullMode, Reader, RemoteRepo,
};

fn command(program: &str, arguments: &[&str]) {
    let output = Command::new(program).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(path: &Path, arguments: &[&str]) {
    let path = path.to_str().unwrap();
    let mut all = vec!["-C", path];
    all.extend_from_slice(arguments);
    command("git", &all);
}

struct IsolatedSshd {
    root: PathBuf,
    child: Child,
    port: u16,
    key: PathBuf,
    known_hosts: PathBuf,
}

impl IsolatedSshd {
    fn start() -> Self {
        let root = std::env::temp_dir().join(format!("gitrust-sshd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let key = root.join("client-key");
        let host_key = root.join("host-key");
        command(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-f", key.to_str().unwrap()],
        );
        command(
            "ssh-keygen",
            &[
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                host_key.to_str().unwrap(),
            ],
        );
        fs::copy(key.with_extension("pub"), root.join("authorized_keys")).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPidFile {}\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\nStrictModes no\nPermitRootLogin no\nSubsystem sftp internal-sftp\nLogLevel QUIET\n",
            host_key.display(), root.join("authorized_keys").display(), root.join("sshd.pid").display(),
        );
        fs::write(root.join("sshd_config"), config).unwrap();
        // macOS refuses to start sshd unless argv[0] is an absolute path;
        // `/usr/sbin/sshd` is also the standard location on our Linux CI.
        let child = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f", root.join("sshd_config").to_str().unwrap()])
            .spawn()
            .unwrap();
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Self {
                    known_hosts: root.join("known_hosts"),
                    root,
                    child,
                    port,
                    key,
                };
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("isolated sshd did not start for {user}");
    }

    fn connect(&self) -> Session {
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string();
        Session::connect_with_key(
            Login::new("127.0.0.1".to_string(), user, self.port).unwrap(),
            &self.key,
            &self.known_hosts,
            true,
        )
        .unwrap()
    }
}

impl Drop for IsolatedSshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "starts an isolated sshd; run with GITRUST_SSHD_INTEGRATION=1"]
fn resolves_continues_and_aborts_a_remote_merge() {
    if std::env::var_os("GITRUST_SSHD_INTEGRATION").is_none() {
        return;
    }
    let sshd = IsolatedSshd::start();
    let checkout = sshd.root.join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-b", "main"]);
    git(&checkout, &["config", "user.name", "gitrust ssh test"]);
    git(&checkout, &["config", "user.email", "ssh@example.invalid"]);
    git(&checkout, &["config", "core.editor", "true"]);
    fs::write(checkout.join("note.txt"), "base\n").unwrap();
    git(&checkout, &["add", "note.txt"]);
    git(&checkout, &["commit", "-m", "base"]);
    git(&checkout, &["switch", "-c", "topic"]);
    fs::write(checkout.join("note.txt"), "topic\n").unwrap();
    git(&checkout, &["commit", "-am", "topic"]);
    git(&checkout, &["switch", "main"]);
    fs::write(checkout.join("note.txt"), "main\n").unwrap();
    git(&checkout, &["commit", "-am", "main"]);

    let merge = Command::new("git")
        .args(["-C", checkout.to_str().unwrap(), "merge", "topic"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the fixture must clash");
    let remote = RemoteRepo::new(sshd.connect(), checkout.display().to_string()).unwrap();
    assert_eq!(remote.half_done(), Half::Merge);
    let clashes = remote.clashes().unwrap();
    assert_eq!(clashes.len(), 1);
    assert_eq!(clashes[0].path, "note.txt");
    remote.take_conflict_side("note.txt", true).unwrap();
    assert!(remote.clashes().unwrap().is_empty());
    remote.continue_operation().unwrap();
    assert_eq!(remote.half_done(), Half::None);

    // Branch, upstream, tag, push/fetch and stash all execute from the
    // checkout at the other end of SSH.  The upstream is a local bare
    // repository only to keep this isolated test off the network.
    let upstream = sshd.root.join("upstream.git");
    command("git", &["init", "--bare", upstream.to_str().unwrap()]);
    git(&checkout, &["remote", "add", "origin", upstream.to_str().unwrap()]);
    remote.branch_here("review").unwrap();
    remote.push_branch("review").unwrap();
    remote.delete_remote_branch("origin/review").unwrap();
    remote.push_branch("review").unwrap();
    remote.rename_branch("review", "review-renamed").unwrap();
    remote.push_branch("review-renamed").unwrap();
    remote.set_upstream("review-renamed", Some("origin/review-renamed")).unwrap();
    assert_eq!(remote.tracking().unwrap().remote, "origin/review-renamed");
    remote.fetch_all().unwrap();
    remote.pull(PullMode::Merge).unwrap();
    remote.push_force().unwrap();
    let target = remote.commits(1).unwrap()[0].id;
    remote.tag("review-tag", target, "remote tag").unwrap();
    remote.push_tags().unwrap();
    remote.delete_tag("review-tag").unwrap();
    fs::write(checkout.join("note.txt"), "stash me\n").unwrap();
    remote.stash_save("remote stash").unwrap();
    assert_eq!(remote.stashes().unwrap().len(), 1);
    remote.stash_apply(0).unwrap();
    remote.stash_drop(0).unwrap();
    git(&checkout, &["restore", "note.txt"]);
    remote.checkout_branch("main").unwrap();
    remote.delete_branch("review-renamed").unwrap();

    let merge = Command::new("git")
        .args([
            "-C",
            checkout.to_str().unwrap(),
            "reset",
            "--hard",
            "HEAD~1",
        ])
        .output()
        .unwrap();
    assert!(merge.status.success());
    let merge = Command::new("git")
        .args(["-C", checkout.to_str().unwrap(), "merge", "topic"])
        .output()
        .unwrap();
    assert!(!merge.status.success());
    assert_eq!(remote.half_done(), Half::Merge);
    // Reconnecting does not lose the server-side operation state: a new
    // SSH session discovers precisely the same pending merge and paths.
    drop(remote);
    let remote = RemoteRepo::new(sshd.connect(), checkout.display().to_string()).unwrap();
    assert_eq!(remote.half_done(), Half::Merge);
    assert_eq!(remote.clashes().unwrap().len(), 1);
    remote.abort_operation().unwrap();
    assert_eq!(remote.half_done(), Half::None);

    let pick = Command::new("git")
        .args(["-C", checkout.to_str().unwrap(), "cherry-pick", "topic"])
        .output()
        .unwrap();
    assert!(!pick.status.success(), "the cherry-pick fixture must clash");
    assert_eq!(remote.half_done(), Half::CherryPick);
    assert_eq!(remote.clashes().unwrap().len(), 1);
    remote.abort_operation().unwrap();
    assert_eq!(remote.half_done(), Half::None);

    git(&checkout, &["switch", "topic"]);
    let rebase = Command::new("git")
        .args(["-C", checkout.to_str().unwrap(), "rebase", "main"])
        .output()
        .unwrap();
    assert!(!rebase.status.success(), "the rebase fixture must clash");
    assert_eq!(remote.half_done(), Half::Rebase);
    assert_eq!(remote.clashes().unwrap().len(), 1);
    remote.abort_operation().unwrap();
    assert_eq!(remote.half_done(), Half::None);
}
