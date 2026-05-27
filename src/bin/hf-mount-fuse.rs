#[cfg(not(unix))]
fn main() {
    eprintln!("hf-mount-fuse is Unix-only. On Windows, use the NFS backend.");
    std::process::exit(1);
}

#[cfg(unix)]
fn main() {
    use tracing::info;

    use hf_mount::fuse::mount_fuse;
    use hf_mount::setup::setup;

    let s = setup(false);
    let mut daemon_guard = hf_mount::daemon::DaemonGuard::from_env();

    let session = match mount_fuse(&s, daemon_guard.as_mut(), Vec::new()) {
        Ok(session) => session,
        Err(err) => {
            tracing::error!("FUSE mount failed: {}", err);
            std::process::exit(1);
        }
    };

    session.wait();
    info!("Unmounted cleanly");
}
