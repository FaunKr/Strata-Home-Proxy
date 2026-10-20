//! Everything that touches the operating system, behind a trait so the
//! service logic can be tested with a fake PC.

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use crate::{
    command::{self, CommandSpec, Running},
    hardware::Hardware,
};

pub trait Platform: Send + Sync + 'static {
    /// Time since the last keyboard or mouse input.
    fn user_idle(&self) -> Duration;
    /// Puts the PC into standby. Returns after the PC wakes up again.
    fn suspend(&self) -> Result<()>;
    /// Starts Strata without a window; its console output comes through the
    /// returned command.
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<Running>;
    /// Ends Strata. Each entry is a program name (`strata.exe`) or a folder
    /// (ending in `\` or `/`): every program from that folder ends, with all
    /// it started.
    fn stop_strata(&self, processes: &[String]) -> Result<()>;
    /// Starts a command whose output is read line by line.
    fn run(&self, spec: &CommandSpec) -> Result<Running>;
    /// Installed RAM in bytes; 0 if unknown.
    fn ram_bytes(&self) -> u64;
    /// Bytes free for this user on the drive that holds `path`, which need
    /// not exist yet.
    fn disk_free(&self, path: &Path) -> Option<u64>;
    /// MAC address of the network card that holds `local`, the address the
    /// NAS connected to. `None` if unknown. May be slow; the caller caches it.
    fn mac_for(&self, _local: IpAddr) -> Option<[u8; 6]> {
        None
    }
    /// Update: puts `downloaded` where the running program is, so the
    /// scheduled task and the desktop shortcut start the new one, and starts
    /// it with the same arguments. The running program stays next to it as
    /// `.alt` until the new one has taken over. On an error everything is as
    /// before.
    fn replace_program(&self, downloaded: &Path) -> Result<()>;
    /// Ends this process, so the new program gets the port.
    fn exit(&self);
    /// The hardware readings; returns at once, `/status` asks every few seconds.
    fn hardware(&self) -> Hardware {
        Hardware::default()
    }
}

pub struct System;

#[cfg(windows)]
mod imp {
    use std::os::windows::{ffi::OsStrExt, process::CommandExt};

    use windows_sys::Win32::{
        System::{Power::SetSuspendState, SystemInformation::GetTickCount},
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };

    use super::*;

    pub fn hidden(cmd: &mut Command) -> &mut Command {
        cmd.creation_flags(super::CREATE_NO_WINDOW)
    }

    pub fn user_idle() -> Duration {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid LASTINPUTINFO with cbSize set.
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return Duration::ZERO;
        }
        // SAFETY: no preconditions.
        let now = unsafe { GetTickCount() };
        Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
    }

    pub fn suspend() -> Result<()> {
        // Hibernate off (`powercfg -h off`), so this is standby (S3).
        // SAFETY: plain flags, no pointers.
        if !unsafe { SetSuspendState(false, false, false) } {
            bail!(
                "SetSuspendState failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }

    pub fn stop(processes: &[String]) -> Result<()> {
        for entry in processes {
            if super::is_folder(entry) {
                stop_folder(entry)?;
            } else {
                // Exit code 128 means "not running", which is fine.
                hidden(Command::new("taskkill").args(["/F", "/T", "/IM", entry])).status()?;
            }
        }
        Ok(())
    }

    /// Ends every program started from `folder`, with the programs they
    /// started: a venv's python.exe starts the real interpreter elsewhere.
    fn stop_folder(folder: &str) -> Result<()> {
        let folder = folder.trim_end_matches(['\\', '/']).replace('\'', "''");
        let script = format!(
            // StartsWith, not -like: brackets in the folder name are no wildcards.
            "Get-Process | Where-Object {{ $_.Path -and $_.Path.StartsWith('{folder}\\', [StringComparison]::OrdinalIgnoreCase) }} | ForEach-Object {{ taskkill /F /T /PID $_.Id | Out-Null }}"
        );
        hidden(Command::new("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ]))
        .status()?;
        Ok(())
    }

    pub fn ram_bytes() -> u64 {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: MEMORYSTATUSEX is plain data; dwLength is set below.
        let mut info: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        info.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: `info` is a valid MEMORYSTATUSEX with dwLength set.
        if unsafe { GlobalMemoryStatusEx(&mut info) } == 0 {
            return 0;
        }
        info.ullTotalPhys
    }

    pub fn disk_free(path: &Path) -> Option<u64> {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let existing = path.ancestors().find(|p| p.is_dir())?;
        let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated; the unused outputs may be null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }
}

#[cfg(not(windows))]
mod imp {
    //! Stand-ins so the service builds and runs on Linux for development.
    use super::*;

    pub fn user_idle() -> Duration {
        Duration::from_secs(u64::MAX / 4)
    }

    pub fn suspend() -> Result<()> {
        bail!("Standby exists only on Windows")
    }

    pub fn stop(processes: &[String]) -> Result<()> {
        for entry in processes {
            if super::is_folder(entry) {
                // Every program whose command line starts in the folder.
                Command::new("pkill")
                    .arg("-f")
                    .arg(format!("^{}", regex_escape(entry)))
                    .status()?;
            } else {
                Command::new("pkill").arg("-x").arg(entry).status()?;
            }
        }
        Ok(())
    }

    /// The folder as a literal in pkill's extended regex.
    fn regex_escape(text: &str) -> String {
        text.chars()
            .flat_map(|c| {
                let special = "\\.^$|?*+()[]{}".contains(c);
                special.then_some('\\').into_iter().chain([c])
            })
            .collect()
    }

    pub fn ram_bytes() -> u64 {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
                let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
                Some(kb * 1024)
            })
            .unwrap_or(0)
    }

    pub fn disk_free(path: &Path) -> Option<u64> {
        let existing = path.ancestors().find(|p| p.is_dir())?;
        let out = Command::new("df").arg("-Pk").arg(existing).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let kb: u64 = text
            .lines()
            .nth(1)?
            .split_whitespace()
            .nth(3)?
            .parse()
            .ok()?;
        Some(kb * 1024)
    }
}

/// The running program after an update, until the new one has started.
fn replaced_path(program: &Path) -> PathBuf {
    let mut old = program.as_os_str().to_owned();
    old.push(".alt");
    PathBuf::from(old)
}

/// Removes what an update left of the program before; Windows keeps a
/// running program's file, so the new program does this once the old one
/// has ended.
/// True once it is gone, or there was none.
pub fn remove_replaced_program() -> bool {
    let Ok(program) = std::env::current_exe() else {
        return true;
    };
    match std::fs::remove_file(replaced_path(&program)) {
        Ok(()) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn replace_program(downloaded: &Path) -> Result<()> {
    let program = std::env::current_exe().context("Path of the running program unknown")?;
    let old = replaced_path(&program);
    let _ = std::fs::remove_file(&old);
    // Windows lets a running program be renamed, not overwritten.
    std::fs::rename(&program, &old)
        .with_context(|| format!("{} cannot be renamed", program.display()))?;
    let undo = |e: anyhow::Error| {
        let _ = std::fs::remove_file(&program);
        let _ = std::fs::rename(&old, &program);
        e
    };
    std::fs::rename(downloaded, &program)
        .or_else(|_| std::fs::copy(downloaded, &program).map(drop))
        .with_context(|| format!("{} cannot be created", program.display()))
        .map_err(undo)?;
    let mut cmd = Command::new(&program);
    cmd.args(std::env::args_os().skip(1));
    cmd.env(REPLACES_ENV, std::process::id().to_string());
    if let Ok(dir) = std::env::current_dir() {
        cmd.current_dir(dir);
    }
    cmd.spawn()
        .context("New program does not start")
        .map_err(undo)?;
    let _ = std::fs::remove_file(downloaded);
    Ok(())
}

/// Set for the program an update starts: the process ID of the program it
/// replaces, so the new one can end it when it keeps the port.
pub const REPLACES_ENV: &str = "STRATA_PC_AGENT_REPLACES";

/// The program this one replaces, when an update started it.
pub fn replaced_pid() -> Option<u32> {
    std::env::var(REPLACES_ENV).ok()?.parse().ok()
}

/// Ends the program an update replaced, when it still runs. Only a process
/// of this program's name, so a reused process ID ends nothing else.
pub fn end_replaced(pid: u32) -> Result<()> {
    let program = std::env::current_exe().context("Path of the running program unknown")?;
    let name = program
        .file_name()
        .context("Running program has no file name")?
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    {
        // Exit code 128 means "not running", which is fine.
        hidden(Command::new("taskkill").args([
            "/F",
            "/PID",
            &pid.to_string(),
            "/FI",
            &format!("IMAGENAME eq {name}"),
        ]))
        .status()?;
    }
    #[cfg(not(windows))]
    {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim() == name {
            Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status()?;
        }
    }
    Ok(())
}

/// Strata as a command: in the folder of its start script, as a double click
/// on it would run it.
fn strata_command(program: &Path, args: &[String]) -> Result<CommandSpec> {
    // A bare file name has an empty parent, which Windows rejects as
    // working directory ("The directory name is invalid").
    let dir = program.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = dir
        && !dir.is_dir()
    {
        bail!(
            "Folder {} from strata_start does not exist; check the path in strata-pc-agent.toml",
            dir.display()
        );
    }
    Ok(CommandSpec {
        program: program.to_path_buf(),
        args: args.to_vec(),
        dir: dir.map(Path::to_path_buf),
    })
}

/// Keeps helper programs from flashing a console window.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
pub(crate) use imp::hidden;

/// Whether an entry of `strata_processes` names a folder rather than a program.
pub fn is_folder(entry: &str) -> bool {
    entry.ends_with(['\\', '/'])
}

/// Asks Windows which adapter holds `local` and reads its MAC address.
#[cfg(windows)]
fn windows_mac_for(local: IpAddr) -> Option<[u8; 6]> {
    let script = format!(
        "(Get-NetAdapter -InterfaceIndex (Get-NetIPAddress -IPAddress '{local}' -ErrorAction Stop).InterfaceIndex).MacAddress"
    );
    let out = hidden(Command::new("powershell").args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &script,
    ]))
    .output()
    .ok()?;
    if !out.status.success() {
        return None;
    }
    crate::config::parse_mac(String::from_utf8_lossy(&out.stdout).trim())
}

impl Platform for System {
    fn user_idle(&self) -> Duration {
        imp::user_idle()
    }
    fn suspend(&self) -> Result<()> {
        imp::suspend()
    }
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<Running> {
        command::spawn(&strata_command(program, args)?)
    }
    fn stop_strata(&self, processes: &[String]) -> Result<()> {
        imp::stop(processes)
    }
    fn run(&self, spec: &CommandSpec) -> Result<Running> {
        command::spawn(spec)
    }
    fn ram_bytes(&self) -> u64 {
        imp::ram_bytes()
    }
    fn disk_free(&self, path: &Path) -> Option<u64> {
        imp::disk_free(path)
    }
    #[cfg(windows)]
    fn mac_for(&self, local: IpAddr) -> Option<[u8; 6]> {
        windows_mac_for(local)
    }
    fn replace_program(&self, downloaded: &Path) -> Result<()> {
        replace_program(downloaded)
    }
    fn exit(&self) {
        std::process::exit(0);
    }
    #[cfg(windows)]
    fn hardware(&self) -> Hardware {
        crate::hardware::read()
    }
}

/// Shows a message to whoever clicked the desktop shortcut.
pub fn notify(title: &str, message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (t, m) = (wide(title), wide(message));
        // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                m.as_ptr(),
                t.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            )
        };
    }
    #[cfg(not(windows))]
    println!("{title}: {message}");
}
