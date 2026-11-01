use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use workbench_core::runtime::command_output;
#[cfg(not(windows))]
use workbench_core::runtime::kill_tree;

fn helper_arguments() -> Vec<String> {
    vec![
        "--exact".into(),
        "command_lifecycle_child".into(),
        "--ignored".into(),
        "--nocapture".into(),
    ]
}

fn configure(root: &Path, mode: &str) {
    std::fs::write(root.join("mode.txt"), mode).unwrap();
}

// A real process fixture launched through the same platform process APIs as a
// normal tool. It is deliberately excluded from ordinary direct test execution.
#[test]
#[ignore = "Internal command lifecycle subprocess helper"]
fn command_lifecycle_child() {
    let mode = std::fs::read_to_string("mode.txt").unwrap();
    match mode.as_str() {
        "wait" => {
            println!("started");
            std::thread::sleep(Duration::from_secs(30));
        }
        "noisy" => {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            for _ in 0..300 {
                stdout.write_all(&[b'x'; 8192]).unwrap();
            }
            stdout.write_all(b"finished noisy command").unwrap();
        }
        "parent" | "running-parent" => {
            std::fs::create_dir_all("grandchild").unwrap();
            configure(Path::new("grandchild"), "wait");
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(helper_arguments())
                .current_dir("grandchild")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let child = command.spawn().unwrap();
            std::fs::write("grandchild.pid", child.id().to_string()).unwrap();
            // The parent must be able to exit while its child holds both output
            // handles, recreating the former unbounded drain after wait().
            std::mem::forget(child);
            if mode == "running-parent" {
                std::thread::sleep(Duration::from_secs(30));
            }
        }
        other => panic!("Unknown lifecycle helper mode: {other}"),
    }
}

#[cfg(windows)]
fn assert_process_stopped(pid: u32) {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if !process.is_null() {
        let mut exit_code = 0;
        let queried = unsafe { GetExitCodeProcess(process, &mut exit_code) };
        unsafe { CloseHandle(process) };
        assert_ne!(queried, 0);
        assert_ne!(exit_code, 259, "Job Object left the descendant running");
    }
}

#[tokio::test]
async fn cancellation_terminates_a_running_command_within_a_bound() {
    let temp = tempfile::tempdir().unwrap();
    configure(temp.path(), "wait");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let result = command_output(
        temp.path(),
        std::env::current_exe().unwrap().to_str().unwrap(),
        &helper_arguments(),
        30,
        &cancel,
        false,
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("canceled"));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn deadline_covers_a_nonterminating_command() {
    let temp = tempfile::tempdir().unwrap();
    configure(temp.path(), "wait");
    let started = Instant::now();
    let result = command_output(
        temp.path(),
        std::env::current_exe().unwrap().to_str().unwrap(),
        &helper_arguments(),
        1,
        &CancellationToken::new(),
        false,
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn excessive_output_is_drained_without_breaking_the_command() {
    let temp = tempfile::tempdir().unwrap();
    configure(temp.path(), "noisy");
    let result = command_output(
        temp.path(),
        std::env::current_exe().unwrap().to_str().unwrap(),
        &helper_arguments(),
        10,
        &CancellationToken::new(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(result["exitCode"], 0);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["outputIncomplete"], false);
    assert!(result["stdout"].as_str().unwrap().len() <= 1024 * 1024);
}

#[tokio::test]
async fn exited_parent_with_inherited_pipes_cannot_hang_the_turn() {
    let temp = tempfile::tempdir().unwrap();
    configure(temp.path(), "parent");
    let started = Instant::now();
    let result = command_output(
        temp.path(),
        std::env::current_exe().unwrap().to_str().unwrap(),
        &helper_arguments(),
        10,
        &CancellationToken::new(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(result["exitCode"], 0);
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid: u32 = std::fs::read_to_string(temp.path().join("grandchild.pid"))
        .unwrap()
        .parse()
        .unwrap();
    #[cfg(windows)]
    {
        assert_process_stopped(pid);
        assert_eq!(result["backgroundDescendantsMayContinue"], false);
    }
    // Also clean up the explicit fixture PID on non-Windows, where only bounded
    // pipe draining is promised by the current platform implementation.
    #[cfg(not(windows))]
    kill_tree(pid).await;
}

#[cfg(windows)]
#[tokio::test]
async fn abandoning_a_command_future_closes_its_job_and_descendants() {
    let temp = tempfile::tempdir().unwrap();
    configure(temp.path(), "running-parent");
    let root = temp.path().to_path_buf();
    let task = tokio::spawn(async move {
        command_output(
            &root,
            std::env::current_exe().unwrap().to_str().unwrap(),
            &helper_arguments(),
            30,
            &CancellationToken::new(),
            false,
        )
        .await
    });
    let pid_file = temp.path().join("grandchild.pid");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // Windows termination initiated by closing a job is asynchronous.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_process_stopped(pid);
}

#[tokio::test]
#[ignore = "Requires a running Docker daemon and prepared python:3.12-slim image"]
async fn docker_cancel_removes_the_owned_container() {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        trigger.cancel();
    });
    let result = command_output(
        temp.path(),
        "python",
        &["-c".into(), "import time; time.sleep(60)".into()],
        30,
        &cancel,
        true,
    )
    .await;
    assert!(result.is_err());
    let label = format!(
        "label=workbench.command-root={:x}",
        Sha256::digest(temp.path().to_string_lossy().as_bytes())
    );
    let check = std::process::Command::new("docker")
        .args(["ps", "-aq", "--filter", &label])
        .output()
        .unwrap();
    assert!(check.status.success());
    assert!(
        String::from_utf8_lossy(&check.stdout).trim().is_empty(),
        "Canceled command left a Docker container running"
    );
}
