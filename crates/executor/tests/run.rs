// SPDX-FileCopyrightText: 2026 Slavi Pantaleev
//
// SPDX-License-Identifier: Apache-2.0

//! Deploy runs against throwaway repositories, as the current user (no namespaces involved).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use kukaroll_config::model::{self, ExecutorProject};
use kukaroll_executor::healthchecks::Healthchecks;
use kukaroll_executor::output::Output;
use kukaroll_executor::run::{Identity, Outcome, Run, STATE_REF};
use rustix::fs::{FlockOperation, flock};

struct Fixture {
    dir: tempfile::TempDir,
    config: ExecutorProject,
    fetch_retry_delays: Vec<Duration>,
    fetch_timeout: Duration,
}

impl Fixture {
    /// A bare `origin`, a `work` clone to push from and the deployed `checkout`.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("home")).unwrap();
        let fixture = Self {
            config: ExecutorProject {
                checkout: dir.path().join("checkout"),
                remote: "origin".into(),
                branch: "main".into(),
                git_user: "unused".into(),
                hooks_user: "unused".into(),
                lock_file: dir.path().join("deploy.lock"),
                pre_flight: None,
                deploy: None,
                healthchecks: None,
            },
            dir,
            fetch_retry_delays: Vec::new(),
            fetch_timeout: Duration::from_secs(60),
        };
        fixture.git(&[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            "origin",
        ]);
        fixture.git(&["clone", "--quiet", "origin", "work"]);
        fixture.commit("1");
        fixture.git(&["clone", "--quiet", "origin", "checkout"]);
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.path("home").display().to_string()),
            ("PATH".into(), std::env::var("PATH").unwrap()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ]
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(self.dir.path())
            .env_clear()
            .envs(self.environment())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// Commits `version` as the file `version` and pushes it; returns the commit.
    fn commit(&self, version: &str) -> String {
        std::fs::write(self.path("work/version"), version).unwrap();
        self.git(&["-C", "work", "add", "version"]);
        self.git(&["-C", "work", "commit", "--quiet", "-m", version]);
        self.git(&["-C", "work", "push", "--quiet", "origin", "main"]);
        self.git(&["-C", "work", "rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        self.git(&["-C", "checkout", "rev-parse", "HEAD"])
    }

    fn deployed(&self) -> Option<String> {
        let commit = self.git(&[
            "-C",
            "checkout",
            "for-each-ref",
            "--format=%(objectname)",
            STATE_REF,
        ]);
        (!commit.is_empty()).then_some(commit)
    }

    fn identity(&self) -> Identity {
        Identity {
            prefix: Vec::new(),
            environment: self.environment(),
        }
    }

    /// A hook appending `label:previous:commit:<version file>` to `hooks.log`, then exiting.
    fn hook(&self, label: &str, exit: u8) -> Option<model::Command> {
        let log = self.path("hooks.log");
        let script = format!(
            "echo {label}:$KUKAROLL_PREVIOUS_COMMIT:$KUKAROLL_COMMIT:$(cat version) >> {log}; echo {label} says hi; exit {exit}",
            log = log.display()
        );
        Some(model::Command(vec!["sh".into(), "-c".into(), script]))
    }

    fn hook_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("hooks.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn run(&self, healthchecks: &Healthchecks) -> (anyhow::Result<Outcome>, String) {
        self.run_as(self.identity(), self.identity(), healthchecks)
    }

    fn run_as(
        &self,
        git: Identity,
        hooks: Identity,
        healthchecks: &Healthchecks,
    ) -> (anyhow::Result<Outcome>, String) {
        let mut output = Output::new();
        let result = Run {
            project: "app",
            config: &self.config,
            git,
            hooks,
            fetch_retry_delays: &self.fetch_retry_delays,
            fetch_timeout: self.fetch_timeout,
        }
        .execute(&mut output, healthchecks);
        (result, output.tail())
    }
}

/// Records the path and body of every request; answers 200.
fn fake_healthchecks() -> (String, mpsc::Receiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ping/check", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let path = request_line.split_whitespace().nth(1).unwrap().to_owned();
            sender
                .send((path, String::from_utf8(body).unwrap()))
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nOK")
                .unwrap();
        }
    });
    (url, receiver)
}

fn pings(receiver: &mpsc::Receiver<(String, String)>) -> Vec<(String, String)> {
    receiver.try_iter().collect()
}

#[test]
fn deploys_new_tips_once_with_hooks_around_the_checkout() {
    let mut fixture = Fixture::new();
    fixture.config.pre_flight = fixture.hook("pre", 0);
    fixture.config.deploy = fixture.hook("deploy", 0);
    let first = fixture.git(&["-C", "work", "rev-parse", "HEAD"]);
    let healthchecks = Healthchecks::new(None);

    let (result, _) = fixture.run(&healthchecks);
    assert_eq!(result.unwrap(), Outcome::Deployed(first.clone()));
    assert_eq!(fixture.deployed().as_deref(), Some(first.as_str()));

    let (result, _) = fixture.run(&healthchecks);
    assert_eq!(result.unwrap(), Outcome::AlreadyDeployed(first.clone()));

    // Pushes coalesce: only the tip is deployed.
    // Pre-flight sees the old tree, deploy the new one.
    fixture.commit("2");
    let third = fixture.commit("3");
    let (result, tail) = fixture.run(&healthchecks);
    assert_eq!(result.unwrap(), Outcome::Deployed(third.clone()));
    assert_eq!(fixture.head(), third);
    assert_eq!(
        fixture.hook_log(),
        [
            format!("pre::{first}:1"),
            format!("deploy::{first}:1"),
            format!("pre:{first}:{third}:1"),
            format!("deploy:{first}:{third}:3"),
        ]
    );
    assert!(tail.contains("deploy says hi\n"), "{tail}");
}

#[test]
fn a_failing_pre_flight_leaves_the_checkout_alone() {
    let mut fixture = Fixture::new();
    let (url, received) = fake_healthchecks();
    let healthchecks = Healthchecks::new(Some(url));
    let first = fixture.git(&["-C", "work", "rev-parse", "HEAD"]);
    fixture.run(&healthchecks).0.unwrap();

    let second = fixture.commit("2");
    fixture.config.pre_flight = fixture.hook("pre", 1);
    fixture.config.deploy = fixture.hook("deploy", 0);
    let (result, _) = fixture.run(&healthchecks);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("pre-flight failed"), "{error}");
    assert_eq!(fixture.head(), first);
    assert_eq!(fixture.deployed().as_deref(), Some(first.as_str()));
    assert_eq!(fixture.hook_log(), [format!("pre:{first}:{second}:1")]);

    let pings = pings(&received);
    let paths: Vec<&str> = pings.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/ping/check/start",
            "/ping/check",
            "/ping/check/start",
            "/ping/check/fail"
        ]
    );
    assert!(pings[1].1.contains("deployed"), "{}", pings[1].1);
    assert!(pings[3].1.contains("pre says hi\n"), "{}", pings[3].1);
    assert!(pings[3].1.contains("pre-flight failed"), "{}", pings[3].1);

    // Once pre-flight passes, the pending commit goes out.
    fixture.config.pre_flight = fixture.hook("pre", 0);
    assert_eq!(
        fixture.run(&healthchecks).0.unwrap(),
        Outcome::Deployed(second)
    );
}

#[test]
fn a_failing_deploy_is_retried_on_the_next_run() {
    let mut fixture = Fixture::new();
    let healthchecks = Healthchecks::new(None);
    let first = fixture.git(&["-C", "work", "rev-parse", "HEAD"]);
    fixture.config.deploy = fixture.hook("deploy", 1);
    assert!(fixture.run(&healthchecks).0.is_err());
    assert_eq!(fixture.head(), first);
    assert_eq!(fixture.deployed(), None);

    fixture.config.deploy = fixture.hook("deploy", 0);
    assert_eq!(
        fixture.run(&healthchecks).0.unwrap(),
        Outcome::Deployed(first.clone())
    );
    assert_eq!(
        fixture.hook_log(),
        [format!("deploy::{first}:1"), format!("deploy::{first}:1")]
    );
}

#[test]
fn checkout_discards_tracked_changes_but_keeps_untracked_files() {
    let fixture = Fixture::new();
    std::fs::write(fixture.path("checkout/version"), "local edit").unwrap();
    std::fs::write(fixture.path("checkout/.env"), "SECRET=1").unwrap();
    let second = fixture.commit("2");
    assert_eq!(
        fixture.run(&Healthchecks::new(None)).0.unwrap(),
        Outcome::Deployed(second)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.path("checkout/version")).unwrap(),
        "2"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.path("checkout/.env")).unwrap(),
        "SECRET=1"
    );
}

#[test]
fn fetch_failures_are_reported() {
    let fixture = Fixture::new();
    let (url, received) = fake_healthchecks();
    fixture.git(&[
        "-C",
        "checkout",
        "remote",
        "set-url",
        "origin",
        "/nonexistent",
    ]);
    let (result, _) = fixture.run(&Healthchecks::new(Some(url)));
    assert!(format!("{:#}", result.unwrap_err()).contains("git fetch failed"));
    let paths: Vec<String> = pings(&received).into_iter().map(|(path, _)| path).collect();
    assert_eq!(paths, ["/ping/check/start", "/ping/check/fail"]);
}

#[test]
fn fetch_failures_are_retried_before_the_run_fails() {
    let mut fixture = Fixture::new();
    fixture.fetch_retry_delays = vec![Duration::from_millis(10), Duration::from_millis(20)];
    let (url, received) = fake_healthchecks();
    fixture.git(&[
        "-C",
        "checkout",
        "remote",
        "set-url",
        "origin",
        "/nonexistent",
    ]);
    let (result, tail) = fixture.run(&Healthchecks::new(Some(url)));
    assert!(format!("{:#}", result.unwrap_err()).contains("git fetch failed"));
    assert!(tail.contains("retrying in 0.01s"), "{tail}");
    assert!(tail.contains("retrying in 0.02s"), "{tail}");
    assert_eq!(tail.matches("retrying").count(), 2, "{tail}");
    let paths: Vec<String> = pings(&received).into_iter().map(|(path, _)| path).collect();
    assert_eq!(paths, ["/ping/check/start", "/ping/check/fail"]);
}

#[test]
fn a_forge_that_comes_back_while_retrying_fails_nothing() {
    let mut fixture = Fixture::new();
    fixture.fetch_retry_delays = vec![Duration::from_millis(500)];
    let (url, received) = fake_healthchecks();
    let origin = fixture.path("origin");
    let away = fixture.path("origin-away");
    std::fs::rename(&origin, &away).unwrap();
    let restorer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        std::fs::rename(&away, &origin).unwrap();
    });
    let (result, tail) = fixture.run(&Healthchecks::new(Some(url)));
    restorer.join().unwrap();
    assert!(matches!(result.unwrap(), Outcome::Deployed(_)));
    assert_eq!(tail.matches("retrying").count(), 1, "{tail}");
    let paths: Vec<String> = pings(&received).into_iter().map(|(path, _)| path).collect();
    assert_eq!(paths, ["/ping/check/start", "/ping/check"]);
}

#[test]
fn a_stalled_fetch_is_killed_and_retried() {
    let mut fixture = Fixture::new();
    fixture.fetch_retry_delays = vec![Duration::from_millis(10)];
    fixture.fetch_timeout = Duration::from_millis(500);
    let marker = fixture.path("stalled.pid");
    fixture.git(&[
        "-C",
        "checkout",
        "remote",
        "set-url",
        "origin",
        "ssh://forge.invalid/app.git",
    ]);
    // A "forge" that accepts the connection and never answers; it records its pid to check it was killed.
    let mut identity = fixture.identity();
    identity.environment.push((
        "GIT_SSH_COMMAND".into(),
        format!("echo $$ >> {}; exec sleep 30; :", marker.display()),
    ));
    let started = std::time::Instant::now();
    let (result, tail) = fixture.run_as(identity.clone(), identity, &Healthchecks::new(None));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains("git fetch failed") && error.contains("timed out"),
        "{error}"
    );
    assert_eq!(tail.matches("retrying").count(), 1, "{tail}");
    let pids = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(pids.lines().count(), 2, "{pids}");
    for pid in pids.lines() {
        // Once killed, it may linger as a zombie until whoever inherited it reaps it.
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.chars().next()))
            .flatten();
        assert!(
            matches!(state, None | Some('Z')),
            "the stalled ssh ({pid}) is still running"
        );
    }
}

#[test]
fn lock_files_are_not_opened_through_symlinks() {
    let mut fixture = Fixture::new();
    let target = fixture.path("elsewhere");
    let link = fixture.path("link.lock");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    fixture.config.lock_file = link;
    fixture.config.deploy = fixture.hook("deploy", 0);
    let (result, _) = fixture.run(&Healthchecks::new(None));
    assert!(
        format!("{:#}", result.unwrap_err()).contains("cannot open the lock file"),
        "a symlinked lock file must fail the run"
    );
    assert!(
        !target.exists(),
        "nothing may be created through the symlink"
    );
    assert!(fixture.hook_log().is_empty());
}

#[test]
fn waits_for_the_lock() {
    let fixture = Fixture::new();
    let lock = std::fs::File::create(&fixture.config.lock_file).unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        drop(lock);
    });
    let (result, tail) = fixture.run(&Healthchecks::new(None));
    releaser.join().unwrap();
    assert!(matches!(result.unwrap(), Outcome::Deployed(_)));
    assert!(tail.contains("waiting for the lock"), "{tail}");
}

#[test]
fn a_missing_checkout_fails_cleanly() {
    let mut fixture = Fixture::new();
    fixture.config.checkout = Path::new("/nonexistent/checkout").to_path_buf();
    assert!(fixture.run(&Healthchecks::new(None)).0.is_err());
}

#[test]
fn hooks_may_use_git_in_a_checkout_they_do_not_own() {
    let mut fixture = Fixture::new();
    let log = fixture.path("safe.log");
    fixture.config.deploy = Some(model::Command(vec![
        "sh".into(),
        "-c".into(),
        format!(
            "echo \"$GIT_CONFIG_COUNT:$GIT_CONFIG_KEY_0:$GIT_CONFIG_VALUE_0\" >> {}",
            log.display()
        ),
    ]));
    let (result, _) = fixture.run(&Healthchecks::new(None));
    assert!(matches!(result.unwrap(), Outcome::Deployed(_)));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        format!("1:safe.directory:{}\n", fixture.config.checkout.display())
    );
}

#[test]
fn git_and_hooks_run_as_their_own_identities() {
    let mut fixture = Fixture::new();
    let log = fixture.path("identities.log");
    fixture.config.pre_flight = Some(model::Command(vec![
        "sh".into(),
        "-c".into(),
        format!("echo \"$WHO:$1:$HOME\" >> {}", log.display()),
        "sh".into(),
        "pre".into(),
    ]));
    // Each identity's prefix and environment apply to its commands only.
    let git = Identity {
        prefix: vec![
            "env".into(),
            format!("GIT_TRACE={}", fixture.path("git.trace").display()),
        ],
        ..fixture.identity()
    };
    let mut hooks = Identity {
        prefix: vec!["env".into(), "WHO=hooks".into()],
        ..fixture.identity()
    };
    hooks.environment.retain(|(name, _)| name != "HOME");
    hooks
        .environment
        .push(("HOME".into(), "/hooks-home".into()));

    let (result, _) = fixture.run_as(git, hooks, &Healthchecks::new(None));
    assert!(matches!(result.unwrap(), Outcome::Deployed(_)));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "hooks:pre:/hooks-home\n"
    );
    let trace = std::fs::read_to_string(fixture.path("git.trace")).unwrap();
    assert!(
        trace.contains("git fetch") && trace.contains("git update-ref"),
        "{trace}"
    );
}
