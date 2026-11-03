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
    machine::MachineFacts,
};

pub trait Platform: Send + Sync + 'static {
    /// Time since the last keyboard or mouse input.
    fn user_idle(&self) -> Duration;
    /// Puts the PC into standby. Returns after the PC wakes up again.
    fn suspend(&self) -> Result<()>;
    /// Restarts the PC (a Planned restart), without installing updates and
    /// without forcing programs closed. Returns once Windows has begun.
    fn restart(&self) -> Result<()> {
        anyhow::bail!("not supported here")
    }
    /// Starts Strata without a window; its console output comes through the
    /// returned command.
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<Running>;
    /// Ends Strata. Each entry is a program name (`strata.exe`) or a folder
    /// (ending in `\` or `/`): every program from that folder ends, with all
    /// it started.
    fn stop_strata(&self, processes: &[String]) -> Result<()>;
    /// Asks the Strata this service started last to end cleanly, as
    /// Ctrl+Break does in its console: Strata tells the engine to quit,
    /// which saves a learning expert profile. Returns at once; an error
    /// when the signal could not be sent.
    fn break_strata(&self) -> Result<()> {
        anyhow::bail!("not supported here")
    }
    /// Starts a command whose output is read line by line.
    fn run(&self, spec: &CommandSpec) -> Result<Running>;
    /// Installed RAM in bytes; 0 if unknown.
    fn ram_bytes(&self) -> u64;
    /// Dedicated VRAM of the biggest graphics card in bytes; 0 without one.
    fn vram_bytes(&self) -> u64 {
        self.hardware()
            .gpu
            .and_then(|gpu| gpu.vram_total_bytes)
            .unwrap_or(0)
    }
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
    /// What the PC is; `data_dir` is the Strata data folder. Slow: the
    /// caller reads it once.
    fn machine(&self, _data_dir: &Path) -> MachineFacts {
        MachineFacts::default()
    }
}

pub struct System;

/// The process id of the Strata this service started last.
static LAST_STRATA: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

#[cfg(windows)]
mod imp {
    use std::{
        os::windows::{ffi::OsStrExt, process::CommandExt},
        process::Stdio,
    };

    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::{
            Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
                SetStdHandle,
            },
            Power::SetSuspendState,
            SystemInformation::GetTickCount,
        },
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };

    use super::*;

    /// Without a window and with an empty stdin, so whatever the service's
    /// own stdin holds never reaches the command.
    pub fn hidden(cmd: &mut Command) -> &mut Command {
        cmd.creation_flags(super::CREATE_NO_WINDOW)
            .stdin(Stdio::null())
    }

    /// [`hidden`], for a command whose output nobody reads: it gets empty
    /// stdout and stderr too, so it starts whatever the service's own
    /// handles hold.
    pub fn silent(cmd: &mut Command) -> &mut Command {
        hidden(cmd).stdout(Stdio::null()).stderr(Stdio::null())
    }

    /// The standard handles: stdin, stdout, stderr.
    pub const STD: [STD_HANDLE; 3] = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];

    /// This process's stdin, stdout and stderr as they were when saved; put
    /// back when dropped. Joining a console sets them to its handles, and
    /// they stay set when the process leaves it: every program started
    /// after that with inherited handles then fails with "The handle is
    /// invalid".
    pub struct StdHandles([HANDLE; 3]);

    impl StdHandles {
        pub fn save() -> Self {
            // SAFETY: plain calls without pointers.
            Self(STD.map(|which| unsafe { GetStdHandle(which) }))
        }
    }

    impl Drop for StdHandles {
        fn drop(&mut self) {
            for (which, handle) in STD.into_iter().zip(self.0) {
                // SAFETY: puts back a value GetStdHandle gave; Windows
                // stores it without using it.
                unsafe { SetStdHandle(which, handle) };
            }
        }
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

    /// Restarts Windows: with the shutdown privilege, which a program must
    /// switch on for itself, and without `SHUTDOWN_INSTALL_UPDATES`, so
    /// updates stay the user's. An update that only waits for a restart
    /// still finishes; programs that hold the restart are not forced.
    pub fn restart() -> Result<()> {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE},
            Security::{
                AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
                SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
                TOKEN_QUERY,
            },
            System::{
                Shutdown::{
                    InitiateShutdownW, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
                    SHTDN_REASON_MINOR_OTHER, SHUTDOWN_RESTART,
                },
                Threading::{GetCurrentProcess, OpenProcessToken},
            },
        };
        // SAFETY: the token handle is checked and closed; every pointer is to
        // a local that lives through the call; the strings are Windows' own
        // NUL-terminated constants or null.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            ) == 0
            {
                bail!(
                    "process token not opened: {}",
                    std::io::Error::last_os_error()
                );
            }
            let mut privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: std::mem::zeroed(),
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let looked_up = LookupPrivilegeValueW(
                std::ptr::null(),
                SE_SHUTDOWN_NAME,
                &mut privileges.Privileges[0].Luid,
            );
            let adjusted = looked_up != 0
                && AdjustTokenPrivileges(
                    token,
                    0,
                    &privileges,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) != 0;
            let error = std::io::Error::last_os_error();
            CloseHandle(token);
            if !adjusted {
                bail!("shutdown privilege not switched on: {error}");
            }
            let code = InitiateShutdownW(
                std::ptr::null(),
                std::ptr::null(),
                0,
                SHUTDOWN_RESTART,
                SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_MINOR_OTHER | SHTDN_REASON_FLAG_PLANNED,
            );
            if code != 0 {
                bail!(
                    "InitiateShutdownW failed: {}",
                    std::io::Error::from_raw_os_error(code as i32)
                );
            }
        }
        Ok(())
    }

    /// Ctrl+Break to the console of process `pid` (Strata runs in a hidden
    /// console of its own): this service joins that console for the send.
    /// The event goes to `pid`'s process group alone, which `pid` started
    /// as [`CommandSpec::own_group`]: sent to every process on the console,
    /// it would end this service too, as nothing here ignores Ctrl+Break.
    /// The service's stdin, stdout and stderr are as before afterwards, so
    /// the programs it starts later still start.
    pub fn break_process(pid: u32) -> Result<()> {
        use windows_sys::Win32::System::Console::{
            AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
            SetConsoleCtrlHandler,
        };
        // Back when this returns, after the console is left.
        let _handles = StdHandles::save();
        // SAFETY: plain calls without pointers; the service has no console
        // of its own to lose (it runs hidden), and frees the one it joins.
        unsafe {
            FreeConsole();
            if AttachConsole(pid) == 0 {
                bail!(
                    "console of {pid} not joined: {}",
                    std::io::Error::last_os_error()
                );
            }
            SetConsoleCtrlHandler(None, 1);
            let sent = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
            let error = std::io::Error::last_os_error();
            FreeConsole();
            if sent == 0 {
                bail!("Ctrl+Break not sent: {error}");
            }
        }
        Ok(())
    }

    pub fn stop(processes: &[String]) -> Result<()> {
        for entry in processes {
            if super::is_folder(entry) {
                stop_folder(entry)?;
            } else {
                // Exit code 128 means "not running", which is fine.
                silent(Command::new("taskkill").args(["/F", "/T", "/IM", entry])).status()?;
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
        silent(Command::new("powershell").args([
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

    pub fn restart() -> Result<()> {
        bail!("Restart exists only on Windows")
    }

    pub fn break_process(_pid: u32) -> Result<()> {
        bail!("Ctrl+Break exists only on Windows")
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
    // A windowless program: it uses none, and inheriting this one's could
    // keep it from starting.
    #[cfg(windows)]
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
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
        let status = silent(Command::new("taskkill").args([
            "/F",
            "/PID",
            &pid.to_string(),
            "/FI",
            &format!("IMAGENAME eq {name}"),
        ]))
        .status()?;
        // Exit code 128 means "not running", which is fine.
        if !status.success() && status.code() != Some(128) {
            bail!("taskkill ended with {status}");
        }
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
        // The clean stop sends Ctrl+Break to this group alone.
        own_group: true,
        ..Default::default()
    })
}

/// Keeps helper programs from flashing a console window.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts a program as the first of a process group of its own.
#[cfg(windows)]
pub(crate) const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[cfg(windows)]
pub(crate) use imp::{hidden, silent};

/// Whether an entry of `strata_processes` names a folder rather than a program.
pub fn is_folder(entry: &str) -> bool {
    entry.ends_with(['\\', '/'])
}

/// The `strata_processes` entry for every program from `dir`.
pub fn folder_entry(dir: &Path) -> String {
    let mut entry = dir.display().to_string();
    if !is_folder(&entry) {
        entry.push(std::path::MAIN_SEPARATOR);
    }
    entry
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
    fn restart(&self) -> Result<()> {
        imp::restart()
    }
    fn start_strata(&self, program: &Path, args: &[String]) -> Result<Running> {
        let run = command::spawn(&strata_command(program, args)?)?;
        *LAST_STRATA.lock().unwrap() = run.pid();
        Ok(run)
    }
    fn break_strata(&self) -> Result<()> {
        let Some(pid) = *LAST_STRATA.lock().unwrap() else {
            anyhow::bail!("no Strata started by this service");
        };
        imp::break_process(pid)
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
    /// At once, not from the readings: their first call has no values yet.
    #[cfg(windows)]
    fn vram_bytes(&self) -> u64 {
        crate::hardware::vram_bytes()
    }
    #[cfg(windows)]
    fn machine(&self, data_dir: &Path) -> MachineFacts {
        crate::machine::read(data_dir)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strata_starts_in_its_own_process_group() {
        let spec = strata_command(Path::new("start-strata.bat"), &[]).unwrap();
        assert!(spec.own_group);
    }

    #[test]
    fn commands_other_than_strata_share_the_service_group() {
        assert!(!CommandSpec::default().own_group);
    }

    /// The clean stop on real Windows: a program started as Strata is, sent
    /// the clean stop's Ctrl+Break. Run by the workflow *Windows*.
    #[cfg(windows)]
    mod ctrl_break {
        use std::time::Duration;

        use super::*;

        /// Joining a console is for the whole process: one test at a time.
        static CONSOLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        const SLEEPER: &str = "platform::tests::ctrl_break::sleeper_for_ctrl_break";

        /// Not a test: the program the tests send Ctrl+Break to, this test
        /// binary run with only this function, which waits a minute. A Rust
        /// program has no handler of its own, so Ctrl+Break ends it.
        #[test]
        #[ignore = "started by the Ctrl+Break tests"]
        fn sleeper_for_ctrl_break() {
            std::thread::sleep(Duration::from_secs(60));
        }

        /// Starts the sleeper the way Strata is started and sends it the
        /// clean stop's Ctrl+Break.
        async fn break_sleeper() -> Running {
            let spec = CommandSpec {
                program: std::env::current_exe().unwrap(),
                args: ["--exact", SLEEPER, "--ignored", "--nocapture"]
                    .map(String::from)
                    .to_vec(),
                ..strata_command(&std::env::current_exe().unwrap(), &[]).unwrap()
            };
            let run = command::spawn(&spec).unwrap();
            // Time for the sleeper to come up with its console.
            tokio::time::sleep(Duration::from_secs(2)).await;
            imp::break_process(run.pid().unwrap()).unwrap();
            run
        }

        #[tokio::test]
        async fn ctrl_break_ends_the_program_in_its_own_group() {
            let _console = CONSOLE.lock().await;
            let run = break_sleeper().await;
            let cancel = run.canceller();
            let ended = tokio::time::timeout(Duration::from_secs(10), run.wait()).await;
            if ended.is_err() {
                cancel();
            }
            // STATUS_CONTROL_C_EXIT: ended by the signal, not for another reason.
            assert_eq!(
                ended.ok().flatten(),
                Some(0xC000_013A_u32 as i32),
                "the program did not end by Ctrl+Break"
            );
        }

        #[tokio::test]
        async fn programs_start_after_ctrl_break() {
            let _console = CONSOLE.lock().await;
            // Like the service, which runs without a console: no standard
            // handles. As they were when the test ends.
            let _handles = imp::StdHandles::save();
            for which in imp::STD {
                // SAFETY: a null handle is what a windowless program has.
                unsafe {
                    windows_sys::Win32::System::Console::SetStdHandle(which, std::ptr::null_mut())
                };
            }
            let run = break_sleeper().await;
            // Started the way the service starts its own commands, with the
            // output inherited from the service.
            let started = hidden(&mut Command::new("cmd"))
                .args(["/C", "exit 0"])
                .status();
            run.canceller()();
            let status = started.expect("no program starts after Ctrl+Break");
            assert!(status.success(), "the program ended with {status}");
        }

        #[tokio::test]
        async fn ctrl_break_leaves_the_service_running() {
            let _console = CONSOLE.lock().await;
            let run = break_sleeper().await;
            // Sent to the whole console, Ctrl+Break ends this whole test
            // process within this time: the run fails as a crash, not as
            // this test failing.
            tokio::time::sleep(Duration::from_secs(2)).await;
            run.canceller()();
        }
    }
}
