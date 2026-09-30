//! Bounded observer subprocesses retained across cancellation of setup futures.
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
struct Entry {
    child: Child,
    output: File,
    program: String,
    reaped: bool,
    code: Option<i32>,
    signal: Option<i32>,
    forced: bool,
}
impl Entry {
    fn status(&mut self, status: ExitStatus) {
        use std::os::unix::process::ExitStatusExt;
        self.reaped = true;
        self.code = status.code();
        self.signal = status.signal();
    }
    fn poll(&mut self) -> Result<(), String> {
        if !self.reaped
            && let Some(status) = self
                .child
                .try_wait()
                .map_err(|_| "observer process poll failed")?
        {
            self.status(status);
        }
        Ok(())
    }
    fn kill(&mut self) {
        if !self.reaped && !self.forced {
            self.forced = true;
            let _ = self.child.kill();
        }
    }
}
#[derive(Default)]
pub struct Commands {
    entries: Vec<Entry>,
}
impl Commands {
    pub async fn capture(
        &mut self,
        program: &str,
        args: &[&str],
        cwd: Option<&Path>,
        budget: Duration,
    ) -> Result<String, String> {
        let output = tempfile::tempfile().map_err(|_| "observer capture file failed")?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                output
                    .try_clone()
                    .map_err(|_| "observer capture clone failed")?,
            ))
            .stderr(Stdio::null());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command
            .spawn()
            .map_err(|_| format!("{program} observer unavailable"))?;
        self.entries.push(Entry {
            child,
            output,
            program: program.into(),
            reaped: false,
            code: None,
            signal: None,
            forced: false,
        });
        let owned = self.entries.last_mut().unwrap();
        let deadline = Instant::now() + budget;
        let force_at = deadline - budget.min(Duration::from_secs(1));
        loop {
            owned.poll()?;
            if owned.reaped {
                break;
            }
            if Instant::now() >= force_at {
                owned.kill();
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} observer deadline; PID{} reap unproven",
                    owned.program,
                    owned.child.id()
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if owned.forced || owned.code != Some(0) {
            return Err(format!("{program} observer failed or exceeded deadline"));
        }
        owned
            .output
            .rewind()
            .map_err(|_| "observer capture rewind failed")?;
        let mut text = String::new();
        (&mut owned.output)
            .take(32 * 1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "observer capture read failed")?;
        if text.len() > 32 * 1024 * 1024 {
            return Err("observer capture limit exceeded".into());
        }
        Ok(text)
    }
    pub async fn cleanup(&mut self) -> Result<(), String> {
        for e in &mut self.entries {
            let _ = e.poll();
            e.kill();
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            for e in &mut self.entries {
                let _ = e.poll();
            }
            if self.entries.iter().all(|e| e.reaped) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("observer subprocess reap unproven after1s".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub fn receipts(&self) -> Value {
        json!(self.entries.iter().map(|e|json!({"pid":e.child.id(),"program":e.program,"reap_confirmed":e.reaped,"exit_code":e.code,"exit_signal":e.signal,"forced":e.forced})).collect::<Vec<_>>())
    }
}
impl Drop for Commands {
    fn drop(&mut self) {
        for e in &mut self.entries {
            let _ = e.poll();
            e.kill();
            let _ = e.poll();
        }
    }
}
#[tokio::test]
async fn canceled_observer_remains_owned_and_is_reaped() {
    let mut commands = Commands::default();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            commands.capture(
                "/bin/sh",
                &["-c", "exec sleep 30"],
                None,
                Duration::from_secs(10)
            )
        )
        .await
        .is_err()
    );
    assert_eq!(commands.entries.len(), 1);
    commands.cleanup().await.unwrap();
    assert!(commands.entries[0].reaped);
    assert!(commands.entries[0].forced);
}
