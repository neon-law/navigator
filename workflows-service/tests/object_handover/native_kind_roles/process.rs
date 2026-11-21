//! Bounded lifetime for only the acceptance command's newly created process group.
use nix::errno::Errno;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) type Health = Arc<AtomicBool>;

pub(super) fn healthy(health: &Health) -> bool {
    health.load(Ordering::SeqCst)
}

fn absent(pid: Pid) -> bool {
    matches!(killpg(pid, None), Err(Errno::ESRCH))
}

struct Group {
    child: Child,
    pid: Pid,
    deadline: Instant,
    health: Health,
    status: Option<ExitStatus>,
}

impl Group {
    fn poll(&mut self) -> bool {
        if self.status.is_none() {
            match self.child.try_wait() {
                Ok(status) => self.status = status,
                Err(_) => return false,
            }
        }
        self.status.is_some() && absent(self.pid)
    }

    fn stop_with(&mut self, signal: impl Fn(Pid, Signal) -> Result<(), Errno>) -> bool {
        if self.poll() {
            return true;
        }
        let _ = signal(self.pid, Signal::SIGTERM);
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let gentle = Instant::now() + (remaining / 2).min(Duration::from_secs(2));
        while Instant::now() < gentle {
            if self.poll() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = signal(self.pid, Signal::SIGKILL);
        while Instant::now() < self.deadline {
            if self.poll() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let drained = self.poll();
        if !drained {
            self.health.store(false, Ordering::SeqCst);
        }
        drained
    }

    fn stop(&mut self) -> bool {
        self.stop_with(killpg)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Long-lived owned port-forward, sharing the same bounded process-group drain.
pub(super) struct Forward {
    group: Group,
    private: tempfile::TempDir,
}

impl Forward {
    pub(super) fn start(health: &Health, command: &mut Command) -> Self {
        assert!(healthy(health));
        let private = tempfile::tempdir().unwrap();
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::fs::File::create(private.path().join("stdout")).unwrap())
            .stderr(std::fs::File::create(private.path().join("stderr")).unwrap())
            .process_group(0);
        let child = command
            .spawn()
            .expect("owned recovery forward could not start");
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
        Self {
            group: Group {
                child,
                pid,
                deadline: Instant::now() + Duration::from_secs(5),
                health: Arc::clone(health),
                status: None,
            },
            private,
        }
    }

    /// No command arguments, raw output, or connection settings leave this owned file.
    pub(super) fn diagnostic(&mut self) -> serde_json::Value {
        use std::io::{Read as _, Seek as _};
        let status = self.group.child.try_wait();
        let mut tail = Vec::new();
        if let Ok(mut file) = std::fs::File::open(self.private.path().join("stderr")) {
            let _ = file.seek(std::io::SeekFrom::End(-8192));
            let _ = file.take(8192).read_to_end(&mut tail);
        }
        let text = String::from_utf8_lossy(&tail).to_ascii_lowercase();
        serde_json::json!({
            "leader_running": matches!(&status, Ok(None)),
            "leader_exit_code": status.ok().flatten().and_then(|value| value.code()),
            "group_absent": absent(self.group.pid),
            "connection_reset": text.contains("connection reset by peer"),
            "lost_pod_connection": text.contains("lost connection to pod"),
            "broken_pipe": text.contains("broken pipe"),
        })
    }

    pub(super) fn close(&mut self) -> bool {
        self.group.deadline = Instant::now() + Duration::from_secs(5);
        self.group.stop()
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.close();
    }
}

pub(super) fn execute(
    health: &Health,
    command: &mut Command,
    input: Option<&[u8]>,
) -> (bool, Vec<u8>, Vec<u8>) {
    execute_with_budget(health, command, input, Duration::from_secs(180))
}

pub(super) fn execute_with_budget(
    health: &Health,
    command: &mut Command,
    input: Option<&[u8]>,
    budget: Duration,
) -> (bool, Vec<u8>, Vec<u8>) {
    assert!(
        healthy(health),
        "an earlier owned process was not proved drained; retain dependent resources"
    );
    let private = tempfile::tempdir().unwrap();
    // A finite owned file avoids an unbounded parent write to a child's stdin pipe.
    let stdin = private.path().join("stdin");
    std::fs::write(&stdin, input.unwrap_or_default()).unwrap();
    command
        .stdin(std::fs::File::open(stdin).unwrap())
        .stdout(std::fs::File::create(private.path().join("stdout")).unwrap())
        .stderr(std::fs::File::create(private.path().join("stderr")).unwrap())
        .process_group(0);
    let deadline = Instant::now() + budget;
    let child = command
        .spawn()
        .expect("owned acceptance command could not start");
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap());
    let mut group = Group {
        child,
        pid,
        deadline,
        health: Arc::clone(health),
        status: None,
    };
    // Reserve cleanup within this budget. Cleanup success is never command success.
    let cleanup_reserve = (budget / 4).min(Duration::from_secs(5));
    let mut natural_completion = false;
    let command_deadline = deadline
        .checked_sub(cleanup_reserve)
        .expect("cleanup reserve fits the command deadline");
    while Instant::now() < command_deadline {
        if group.poll() {
            natural_completion = true;
            break;
        }
        // A leader that exits while retaining descendants must not leave them behind.
        if group.status.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let drained = group.stop();
    let success = natural_completion && group.status.is_some_and(|status| status.success());
    assert!(
        drained,
        "owned process-group drain unproved; retain dependent resources"
    );
    (
        success,
        std::fs::read(private.path().join("stdout")).unwrap(),
        std::fs::read(private.path().join("stderr")).unwrap(),
    )
}

pub(super) fn run(health: &Health, command: &mut Command, input: Option<&[u8]>) -> Vec<u8> {
    let (success, stdout, _) = execute(health, command, input);
    assert!(
        success,
        "owned acceptance command failed (private output withheld)"
    );
    stdout
}

#[cfg(test)]
mod tests {
    use super::{absent, execute, execute_with_budget, healthy, Group, Health};
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn group(command: &mut Command, health: &Health, budget: Duration) -> Group {
        let child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        Group {
            pid: Pid::from_raw(i32::try_from(child.id()).unwrap()),
            child,
            deadline: Instant::now() + budget,
            health: Arc::clone(health),
            status: None,
        }
    }

    #[test]
    fn nonreading_stdin_and_descendants_have_a_bounded_owned_lifetime() {
        let health = Arc::new(AtomicBool::new(true));
        let input = vec![0; 1024 * 1024];
        let started = Instant::now();
        let (success, _, _) = execute(
            &health,
            Command::new("/bin/sh").args(["-c", "sleep 0.05 & wait"]),
            Some(&input),
        );
        assert!(success);
        assert!(started.elapsed() < Duration::from_secs(5));
        let private = tempfile::tempdir().unwrap();
        let ready = private.path().join("ready");
        let mut child = group(
            Command::new("/bin/sh").args(["-c", "trap 'wait; exit' TERM; sleep 30 & printf '%s\n' \"$!\" > \"$OWNED_PROCESS_PROBE_READY\"; wait"])
                .env("OWNED_PROCESS_PROBE_READY", &ready),
            &health,
            Duration::from_secs(5),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant = Pid::from_raw(
            std::fs::read_to_string(&ready)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        assert!(kill(descendant, None).is_ok());
        let pid = child.pid;
        assert!(child.stop());
        assert!(absent(pid));
        assert!(matches!(kill(descendant, None), Err(Errno::ESRCH)));
        assert!(healthy(&health));
    }

    #[test]
    fn unproved_kill_refuses_dependent_cleanup_then_reaps_the_owned_probe() {
        let health = Arc::new(AtomicBool::new(true));
        let mut child = group(
            Command::new("/bin/sleep").arg("30"),
            &health,
            Duration::from_millis(50),
        );
        assert!(!child.stop_with(|_, _| Err(Errno::EPERM)));
        assert!(
            !healthy(&health),
            "dependent resource deletion must be refused"
        );
        child.deadline = Instant::now() + Duration::from_secs(2);
        assert!(child.stop());
        assert!(absent(child.pid));
    }
    #[test]
    fn forward_diagnostics_hide_private_output_and_drain_the_owned_group() {
        let health = Arc::new(AtomicBool::new(true));
        let mut forward = super::Forward::start(
            &health,
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf '%s\\n' \"$PRIVATE_FORWARD_MARKER\" 'connection reset by peer' >&2; sleep 30 & wait",
                    "private-forward-argv-marker",
                ])
                .env("PRIVATE_FORWARD_MARKER", "synthetic-private-forward-marker"),
        );
        let pid = forward.group.pid;
        let until = Instant::now() + Duration::from_secs(2);
        let diagnostic = loop {
            let diagnostic = forward.diagnostic();
            if diagnostic["connection_reset"] == true {
                break diagnostic;
            }
            assert!(
                Instant::now() < until,
                "owned forward did not write its diagnostic"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(diagnostic["leader_running"], true);
        assert_eq!(diagnostic["group_absent"], false);
        let encoded = serde_json::to_string(&diagnostic).unwrap();
        for private in [
            "synthetic-private-forward-marker",
            "private-forward-argv-marker",
            "PRIVATE_FORWARD_MARKER",
            "connection reset by peer",
        ] {
            assert!(!encoded.contains(private));
        }
        assert!(forward.close());
        assert!(absent(pid));
        assert!(healthy(&health));
    }

    #[test]
    fn forced_cleanup_never_reports_natural_command_success() {
        let health = Arc::new(AtomicBool::new(true));
        // A real TERM trap exits zero, but exceeding the observation budget is failure.
        let (success, output, _) = execute_with_budget(
            &health,
            Command::new("/bin/sh").args([
                "-c",
                "trap 'printf term-zero; exit 0' TERM; while :; do :; done",
            ]),
            None,
            Duration::from_secs(2),
        );
        assert_eq!(output.as_slice(), b"term-zero");
        assert!(
            !success,
            "cleanup cannot turn a timed-out command into success"
        );
        assert!(healthy(&health));
        // A naturally successful leader still fails if its surviving child needs a signal.
        let (success, output, _) = execute_with_budget(
            &health,
            Command::new("/bin/sh").args(["-c", "sleep 30 & printf '%s' \"$!\"; exit 0"]),
            None,
            Duration::from_secs(2),
        );
        let descendant = Pid::from_raw(String::from_utf8(output).unwrap().parse().unwrap());
        assert!(
            !success,
            "forced descendant drain is not natural command completion"
        );
        assert!(matches!(kill(descendant, None), Err(Errno::ESRCH)));
        assert!(healthy(&health));
    }
}
