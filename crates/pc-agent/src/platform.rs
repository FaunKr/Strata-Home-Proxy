//! Everything that touches the operating system, behind a trait so the
//! service logic can be tested with a fake PC.

use std::{net::IpAddr, path::Path, process::Command, time::Duration};

use anyhow::{Result, bail};

use crate::command::{self, CommandSpec, Running};

pub trait Platform: Send + Sync + 'static {
    /// Time since the last keyboard or mouse input.
    fn user_idle(&self) -> Duration;
    /// Puts the PC into standby. Returns after the PC wakes up again.
    fn suspend(&self) -> Result<()>;
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<()>;
    /// Ends Strata. Each entry is a program name (`strata.exe`) or a folder
    /// (ending in `\` or `/`): every program from that folder ends, with all
    /// it started.
    fn stop_strata(&self, processes: &[String]) -> Result<()>;
    /// Starts a command whose output is read line by line.
    fn run(&self, spec: &CommandSpec) -> Result<Running>;
    /// MAC address of the network card that holds `local`, the address the
    /// NAS connected to. `None` if unknown. May be slow; the caller caches it.
    fn mac_for(&self, _local: IpAddr) -> Option<[u8; 6]> {
        None
    }
}

pub struct System;

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;

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
                "SetSuspendState fehlgeschlagen: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }

    pub fn start(program: &Path, args: &[String]) -> Result<()> {
        // `start` opens Strata in its own minimised console window, the same
        // as a double click on START-HERE.bat.
        // The window title must be quoted for `start`, so the command line is
        // built by hand instead of letting Command quote each argument.
        let mut line = format!("/C start \"Strata\" /MIN \"{}\"", program.display());
        for arg in args {
            line.push_str(&format!(" \"{arg}\""));
        }
        let mut cmd = Command::new("cmd");
        cmd.raw_arg(line);
        // A bare file name has an empty parent, which Windows rejects as
        // working directory ("The directory name is invalid").
        if let Some(dir) = program.parent().filter(|d| !d.as_os_str().is_empty()) {
            if !dir.is_dir() {
                bail!(
                    "Ordner {} aus strata_start gibt es nicht, Pfad in strata-pc-agent.toml prüfen",
                    dir.display()
                );
            }
            cmd.current_dir(dir);
        }
        hidden(&mut cmd).spawn()?;
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
            "Get-Process | Where-Object {{ $_.Path -like '{folder}\\*' }} | ForEach-Object {{ taskkill /F /T /PID $_.Id | Out-Null }}"
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
}

#[cfg(not(windows))]
mod imp {
    //! Stand-ins so the service builds and runs on Linux for development.
    use super::*;

    pub fn user_idle() -> Duration {
        Duration::from_secs(u64::MAX / 4)
    }

    pub fn suspend() -> Result<()> {
        bail!("Standby gibt es nur unter Windows")
    }

    pub fn start(program: &Path, args: &[String]) -> Result<()> {
        Command::new(program).args(args).spawn()?;
        Ok(())
    }

    pub fn stop(processes: &[String]) -> Result<()> {
        for entry in processes {
            if super::is_folder(entry) {
                // Every program whose command line starts in the folder.
                Command::new("pkill")
                    .arg("-f")
                    .arg(format!("^{entry}"))
                    .status()?;
            } else {
                Command::new("pkill").arg("-x").arg(entry).status()?;
            }
        }
        Ok(())
    }
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
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<()> {
        imp::start(program, args)
    }
    fn stop_strata(&self, processes: &[String]) -> Result<()> {
        imp::stop(processes)
    }
    fn run(&self, spec: &CommandSpec) -> Result<Running> {
        command::spawn(spec)
    }
    #[cfg(windows)]
    fn mac_for(&self, local: IpAddr) -> Option<[u8; 6]> {
        windows_mac_for(local)
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
