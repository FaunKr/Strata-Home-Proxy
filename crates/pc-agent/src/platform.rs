//! Everything that touches the operating system, behind a trait so the
//! service logic can be tested with a fake PC.

use std::{path::Path, process::Command, time::Duration};

use anyhow::{Result, bail};

pub trait Platform: Send + Sync + 'static {
    /// Time since the last keyboard or mouse input.
    fn user_idle(&self) -> Duration;
    /// Puts the PC into standby. Returns after the PC wakes up again.
    fn suspend(&self) -> Result<()>;
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<()>;
    fn stop_strata(&self, processes: &[String]) -> Result<()>;
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

    /// Keeps helper programs from flashing a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn hidden(cmd: &mut Command) -> &mut Command {
        cmd.creation_flags(CREATE_NO_WINDOW)
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
        if let Some(dir) = program.parent() {
            cmd.current_dir(dir);
        }
        hidden(&mut cmd).spawn()?;
        Ok(())
    }

    pub fn stop(processes: &[String]) -> Result<()> {
        for name in processes {
            // Exit code 128 means "not running", which is fine.
            hidden(Command::new("taskkill").args(["/F", "/T", "/IM", name])).status()?;
        }
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
        for name in processes {
            Command::new("pkill").arg("-x").arg(name).status()?;
        }
        Ok(())
    }
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
