//! Which tools this machine can open a worktree in, and how (task 026).
//!
//! Written the way [`doctor::checks`](crate::doctor::checks) is written and for
//! the same reason: **a function over injected inputs, never over whatever
//! happens to be installed on the machine running the tests.** The platform,
//! the home directory and the Windows program directories arrive in
//! [`Machine`]; every question about the disk goes through [`Probe`]. That is
//! what lets the suite assert "Zed present, Cursor absent, on Windows" from a
//! macOS runner with none of the three installed.
//!
//! # Two probes per editor, and getting it backwards is the failure
//!
//! A CLI shim on `PATH` — `code`, `cursor`, `zed` — is the launch mechanism,
//! but on macOS it is installed *separately from the app*: VS Code ships it
//! only after the user runs "Shell Command: Install 'code' command in PATH". An
//! app that is installed with no shim must still be offered, through its
//! application bundle; an app with neither must not be offered at all.
//!
//! The asymmetry is deliberate and worth stating, because it is what decides
//! which mistake this module is allowed to make. A menu that lists Cursor and
//! does nothing when clicked is worse than a menu that never mentions Cursor:
//! the second is merely incomplete, the first is lying.
//!
//! # Two targets are not editors and are not modelled as ones
//!
//! **The file manager always exists** — there is no probe, and opening it is
//! the OS's default handler for a directory, which is exactly what task 007's
//! `reveal_task_worktree` already does.
//!
//! **A terminal is not one program.** macOS has Terminal.app and iTerm,
//! Windows has Windows Terminal and `cmd`, and Linux has a dozen with no common
//! launcher and no common way to spell "start here". So the terminal carries a
//! small per-platform table of candidates *with their own working-directory
//! flag*, rather than being a fourth editor with a different name.
//!
//! # Nothing here spawns anything
//!
//! Detection answers with an argument vector, or with "hand it to the default
//! handler"; the shell runs it. That keeps this module free of `tauri`
//! (ADR-0015) and keeps the launching half assertable byte for byte — which
//! matters because worktree paths contain spaces by construction, and
//! `TempRepo` puts one there on purpose. **Never `sh -c`.**

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Which of the five a menu entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    VsCode,
    Cursor,
    Zed,
    Terminal,
    FileManager,
}

impl Target {
    /// Every target, in the order the menu lists them: editors first, in the
    /// order they are most likely to be the one the user works in, then the two
    /// that are not editors. Fixed here rather than left to detection order so
    /// the menu does not reshuffle between two probes.
    pub const ALL: [Target; 5] = [
        Target::VsCode,
        Target::Cursor,
        Target::Zed,
        Target::Terminal,
        Target::FileManager,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Target::VsCode => "vs_code",
            Target::Cursor => "cursor",
            Target::Zed => "zed",
            Target::Terminal => "terminal",
            Target::FileManager => "file_manager",
        }
    }

    /// What the entry is called on screen. Carried on the wire rather than
    /// re-spelled in `src/types.ts`, for the reason `doctor::Check::label` is.
    pub const fn label(self) -> &'static str {
        match self {
            Target::VsCode => "VS Code",
            Target::Cursor => "Cursor",
            Target::Zed => "Zed",
            Target::Terminal => "Terminal",
            Target::FileManager => "File manager",
        }
    }

    /// The CLI shim this target would be launched by, when it has one.
    ///
    /// The same three names on every platform — VS Code, Cursor and Zed all
    /// install a shim under the name they are known by, and Windows resolves
    /// `code` to `code.cmd` through `PATHEXT` rather than under another name.
    const fn shim(self) -> Option<&'static str> {
        match self {
            Target::VsCode => Some("code"),
            Target::Cursor => Some("cursor"),
            Target::Zed => Some("zed"),
            Target::Terminal | Target::FileManager => None,
        }
    }
}

/// Which platform's conventions to detect against.
///
/// A value rather than `cfg!`, so the suite can assert the Windows and Linux
/// tables from a macOS runner. Task 026 asks for detection written for all
/// three and unit-tested on all three; a `#[cfg]` would make two thirds of this
/// module untestable anywhere one person could run the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this build is for. The one place `cfg!` is read.
    pub const HOST: Platform = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    };
}

/// Everything about the machine that detection needs and may not read for
/// itself — the shape [`doctor::Programs`](crate::doctor::Programs) has, one
/// level up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub platform: Platform,
    /// The user's home directory. `None` on a machine that has none, which is
    /// a real state for a service account and simply drops the per-user
    /// candidates rather than failing.
    pub home: Option<PathBuf>,
    /// `%LOCALAPPDATA%`. Windows installs both VS Code and Cursor per-user by
    /// default, so this is the *first* place to look there, not a fallback.
    pub local_app_data: Option<PathBuf>,
    /// `%ProgramFiles%`, for a machine-wide Windows install.
    pub program_files: Option<PathBuf>,
}

impl Machine {
    /// The machine this process is running on.
    pub fn host() -> Self {
        Self {
            platform: Platform::HOST,
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            program_files: std::env::var_os("ProgramFiles").map(PathBuf::from),
        }
    }

    /// A machine with nothing but a platform — every per-user candidate absent.
    pub fn bare(platform: Platform) -> Self {
        Self {
            platform,
            home: None,
            local_app_data: None,
            program_files: None,
        }
    }
}

/// The two questions detection is allowed to ask the disk.
///
/// A trait rather than two closures so a test hands over one value, and so the
/// real implementation can be the only thing in this module that touches
/// `PATH`.
pub trait Probe {
    /// Whether an executable by this name is on `PATH`.
    fn on_path(&self, program: &str) -> bool;
    /// Whether this path exists — a `.app` bundle directory, a `.exe`, a
    /// `.desktop` entry.
    fn exists(&self, path: &Path) -> bool;
}

/// The real one: `PATH` for shims, the filesystem for bundles.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn on_path(&self, program: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        // `PATHEXT` is why a bare `code` is found on Windows: VS Code installs
        // `code.cmd`, not `code`. Spelled out rather than read from the
        // environment, because the three that matter are the three every
        // Windows carries and a hand-edited `PATHEXT` is not worth a second
        // failure mode.
        let names: Vec<String> = if cfg!(target_os = "windows") {
            [".cmd", ".exe", ".bat"]
                .iter()
                .map(|extension| format!("{program}{extension}"))
                .chain(std::iter::once(program.to_string()))
                .collect()
        } else {
            vec![program.to_string()]
        };

        std::env::split_paths(&path).any(|directory| {
            names
                .iter()
                .any(|name| directory.join(name).is_file() || directory.join(name).is_symlink())
        })
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// How one detected target is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Hand the directory to the OS's default handler — `tauri-plugin-opener`'s
    /// `open_path(path, None)`, the same call `reveal_task_worktree` makes.
    DefaultHandler(PathBuf),
    /// Spawn this argument vector. Already complete, worktree path included,
    /// and never a shell string: a path with a space in it is the normal case
    /// here, not the edge one.
    Command(Vec<OsString>),
}

/// One entry of the menu, and how clicking it opens the worktree.
///
/// `how` is not serialized: the frontend needs the identity and the words, and
/// an argument vector on the wire would be one more thing a renderer could get
/// wrong. The open command re-detects rather than trusting a value the window
/// sent back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedTarget {
    pub target: Target,
    pub label: &'static str,
    #[serde(skip)]
    how: How,
}

impl DetectedTarget {
    /// The launch for one worktree directory.
    pub fn launch(&self, worktree: &Path) -> Launch {
        match &self.how {
            How::DefaultHandler => Launch::DefaultHandler(worktree.to_path_buf()),
            How::Command(prefix) => {
                let mut argv = prefix.clone();
                argv.push(worktree.as_os_str().to_os_string());
                Launch::Command(argv)
            }
        }
    }
}

/// The argument vector minus the worktree path, or the default handler.
///
/// The path is appended by [`DetectedTarget::launch`] rather than baked in
/// here, because detection happens once per window and opening happens per
/// card — and because every candidate below then has exactly one thing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
enum How {
    DefaultHandler,
    Command(Vec<OsString>),
}

fn command(parts: &[&str]) -> How {
    How::Command(parts.iter().map(OsString::from).collect())
}

/// macOS launches an installed application by bundle, and this is the binary
/// that does it. An absolute path because `PATH` is not something a launch
/// should depend on when the whole point is that the shim is missing.
const MACOS_OPEN: &str = "/usr/bin/open";

/// Every target this machine can actually open a worktree in, in
/// [`Target::ALL`] order.
///
/// An uninstalled editor is **absent**, never present-and-disabled: task 026's
/// requirement, not a nicety. The terminal and the file manager are always
/// here, so the menu is never empty.
pub fn detect(machine: &Machine, probe: &impl Probe) -> Vec<DetectedTarget> {
    Target::ALL
        .into_iter()
        .filter_map(|target| {
            how(target, machine, probe).map(|how| DetectedTarget {
                target,
                label: target.label(),
                how,
            })
        })
        .collect()
}

/// The one target, or `None` when this machine cannot open it.
///
/// What `open_task_worktree_in` calls, so an entry the window is holding from a
/// probe five minutes ago fails at the point of *opening* — with the service's
/// own error on the card — rather than silently doing nothing.
pub fn resolve(target: Target, machine: &Machine, probe: &impl Probe) -> Option<DetectedTarget> {
    how(target, machine, probe).map(|how| DetectedTarget {
        target,
        label: target.label(),
        how,
    })
}

fn how(target: Target, machine: &Machine, probe: &impl Probe) -> Option<How> {
    match target {
        // Always present by definition, and it is the same call task 007
        // already makes.
        Target::FileManager => Some(How::DefaultHandler),
        Target::Terminal => terminal(machine, probe),
        editor => {
            // The shim first: it is the launch mechanism the editor's own
            // authors intended, it opens a *folder* rather than a document, and
            // it is the same argument vector on all three platforms. The bundle
            // is the fallback for the macOS case that makes this a two-probe
            // question at all.
            let shim = editor.shim()?;
            if probe.on_path(shim) {
                return Some(command(&[shim]));
            }
            bundles(editor, machine)
                .into_iter()
                .find(|candidate| probe.exists(candidate))
                .map(|bundle| open_installed(machine.platform, &bundle))
        }
    }
}

/// Launching something found on disk rather than on `PATH`.
///
/// macOS needs `open -a`, because a `.app` is a directory and not an
/// executable. The other two found an executable and run it.
fn open_installed(platform: Platform, installed: &Path) -> How {
    match platform {
        Platform::MacOs => How::Command(vec![
            OsString::from(MACOS_OPEN),
            OsString::from("-a"),
            installed.as_os_str().to_os_string(),
        ]),
        Platform::Windows | Platform::Linux => {
            How::Command(vec![installed.as_os_str().to_os_string()])
        }
    }
}

/// Where an editor is installed when its shim is not on `PATH`.
///
/// Per-user locations first on every platform: a machine with both a system and
/// a user install has the user one as the copy that is actually being run.
fn bundles(target: Target, machine: &Machine) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    match machine.platform {
        Platform::MacOs => {
            let bundle = match target {
                Target::VsCode => "Visual Studio Code.app",
                Target::Cursor => "Cursor.app",
                Target::Zed => "Zed.app",
                Target::Terminal | Target::FileManager => return candidates,
            };
            if let Some(home) = &machine.home {
                candidates.push(home.join("Applications").join(bundle));
            }
            candidates.push(PathBuf::from("/Applications").join(bundle));
        }
        Platform::Windows => {
            // `Programs\` under `%LOCALAPPDATA%` is where all three put a
            // per-user install, which is the default for every one of them.
            let (folder, executable) = match target {
                Target::VsCode => ("Microsoft VS Code", "Code.exe"),
                Target::Cursor => ("cursor", "Cursor.exe"),
                Target::Zed => ("Zed", "zed.exe"),
                Target::Terminal | Target::FileManager => return candidates,
            };
            if let Some(local) = &machine.local_app_data {
                candidates.push(local.join("Programs").join(folder).join(executable));
            }
            if let Some(program_files) = &machine.program_files {
                candidates.push(program_files.join(folder).join(executable));
            }
        }
        Platform::Linux => {
            // No bundle concept, so the fallback is the packaged binary under
            // the paths a distribution or a snap would put it at. This is much
            // less load-bearing than on macOS — on Linux the shim *is* the
            // install — but an editor unpacked outside `PATH` is still real.
            let executable = match target {
                Target::VsCode => "code",
                Target::Cursor => "cursor",
                Target::Zed => "zed",
                Target::Terminal | Target::FileManager => return candidates,
            };
            if let Some(home) = &machine.home {
                candidates.push(home.join(".local/bin").join(executable));
            }
            candidates.push(PathBuf::from("/usr/local/bin").join(executable));
            candidates.push(PathBuf::from("/usr/bin").join(executable));
            candidates.push(PathBuf::from("/snap/bin").join(executable));
        }
    }

    candidates
}

/// A terminal, opened *at* the worktree.
///
/// Every candidate carries its own way of spelling "start here", which is the
/// whole reason this is a table and not an editor with a different name. The
/// working-directory flag is part of the prefix, so the worktree path appended
/// by [`DetectedTarget::launch`] lands in the right place for each of them.
fn terminal(machine: &Machine, probe: &impl Probe) -> Option<How> {
    match machine.platform {
        // `open -a` on a directory opens that directory in the terminal app,
        // and both bundles are stable locations. iTerm first: a machine that
        // has it installed it on purpose.
        Platform::MacOs => {
            let bundles = [
                PathBuf::from("/Applications/iTerm.app"),
                PathBuf::from("/System/Applications/Utilities/Terminal.app"),
                PathBuf::from("/Applications/Utilities/Terminal.app"),
            ];
            let found = bundles
                .into_iter()
                .find(|bundle| probe.exists(bundle))
                // Terminal.app is part of the OS. If neither path resolved,
                // something is unusual about this machine and asking `open` for
                // it by name is still likelier to work than offering nothing.
                .unwrap_or_else(|| PathBuf::from("/System/Applications/Utilities/Terminal.app"));
            Some(open_installed(Platform::MacOs, &found))
        }
        // Windows Terminal takes `-d`; `cmd` has no such flag at all, so the
        // fallback starts a shell whose first act is to change directory. Both
        // are argument vectors — `start` is a `cmd` builtin, which is why
        // `cmd /C start` is the spelling rather than a bare `start`.
        Platform::Windows => Some(if probe.on_path("wt") {
            command(&["wt", "-d"])
        } else {
            command(&["cmd.exe", "/C", "start", "cmd.exe", "/K", "cd", "/D"])
        }),
        Platform::Linux => LINUX_TERMINALS
            .iter()
            .find(|(program, _)| probe.on_path(program))
            .map(|(program, flag)| match flag {
                // A flag that takes its value as the next argument, which is
                // what the appended path becomes.
                Some(flag) => command(&[program, flag]),
                None => command(&[program]),
            }),
    }
}

/// How to open one task's worktree in one target, or the reason it cannot be.
///
/// The whole of the rule, in core, so the command that calls it is three lines
/// (ADR-0006) — and so the two things that can be wrong are told apart in the
/// same sentence a user reads. A task with no worktree is the *normal* state of
/// most of the board (task 007), which is why the card offers no control at
/// all rather than a disabled one; reaching here anyway means the window is
/// holding a card whose run has since been undone, and it says so plainly.
///
/// A target that has stopped resolving is the other one, and it is why this
/// re-detects rather than trusting the menu the window built minutes ago: task
/// 026 wants a stale probe to fail at the point of *opening*, on the card,
/// rather than silently doing nothing.
///
/// Deliberately does **not** check that the directory still exists. That race
/// cannot be won — the check and the launch are two moments — and the opener
/// reports a path that has gone in its own words.
pub async fn launch_for_task(
    ctx: &crate::context::ServiceContext,
    machine: &Machine,
    probe: &impl Probe,
    target: Target,
    task_id: &str,
) -> crate::error::Result<Launch> {
    let detail = crate::tasks::get_task(ctx, task_id).await?;
    let worktree = detail.task.worktree_path.ok_or_else(|| {
        crate::error::Error::invalid("this task has no worktree yet — start a run to create one")
    })?;

    let detected = resolve(target, machine, probe).ok_or_else(|| {
        crate::error::Error::invalid(format!(
            "{} is not installed on this machine any more — re-check the Open in menu",
            target.label()
        ))
    })?;

    Ok(detected.launch(Path::new(&worktree)))
}

/// Linux terminals, most-likely-to-be-the-right-one first, each with the flag
/// that puts it in a directory.
///
/// `x-terminal-emulator` is first because on Debian and its derivatives it is
/// the user's *own* choice, expressed through the alternatives system — but it
/// is only an alias, so the flag has to be one the thing behind it accepts, and
/// `--working-directory` is what the Debian alternative documents.
const LINUX_TERMINALS: [(&str, Option<&str>); 7] = [
    ("x-terminal-emulator", Some("--working-directory")),
    ("gnome-terminal", Some("--working-directory")),
    ("konsole", Some("--workdir")),
    ("xfce4-terminal", Some("--working-directory")),
    ("kitty", Some("--directory")),
    ("alacritty", Some("--working-directory")),
    ("wezterm", Some("--cwd")),
];

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashSet;

    /// A machine with exactly the things a test says it has, and nothing else.
    #[derive(Default)]
    struct FakeProbe {
        on_path: HashSet<String>,
        exists: HashSet<PathBuf>,
    }

    impl FakeProbe {
        fn with_shims(shims: &[&str]) -> Self {
            Self {
                on_path: shims.iter().map(|shim| shim.to_string()).collect(),
                exists: HashSet::new(),
            }
        }

        fn and_installed(mut self, paths: &[&str]) -> Self {
            self.exists = paths.iter().map(PathBuf::from).collect();
            self
        }
    }

    impl Probe for FakeProbe {
        fn on_path(&self, program: &str) -> bool {
            self.on_path.contains(program)
        }

        fn exists(&self, path: &Path) -> bool {
            self.exists.contains(path)
        }
    }

    fn mac() -> Machine {
        Machine {
            platform: Platform::MacOs,
            home: Some(PathBuf::from("/Users/ea")),
            ..Machine::bare(Platform::MacOs)
        }
    }

    fn targets_of(detected: &[DetectedTarget]) -> Vec<Target> {
        detected.iter().map(|entry| entry.target).collect()
    }

    /// A path spelled with the *host's* separator, so a test that names a
    /// macOS or Linux location still describes the same file on Windows.
    ///
    /// `PathBuf::join` uses the host's separator, and `detect` builds its
    /// candidates with it — so a literal `"/Applications/Cursor.app"` compared
    /// against a joined path is a comparison of two different strings on
    /// Windows and the same string everywhere else. The `Platform` parameter
    /// exists so these tables are asserted from *any* runner; this is the other
    /// half of making that true.
    fn native(path: &str) -> String {
        let mut parts = path.split('/');
        let root = parts.next().unwrap_or_default();
        let mut built = PathBuf::from(if root.is_empty() { "/" } else { root });
        for part in parts.filter(|part| !part.is_empty()) {
            built.push(part);
        }
        built.to_string_lossy().into_owned()
    }

    fn argv(entry: &DetectedTarget, worktree: &str) -> Vec<String> {
        match entry.launch(Path::new(worktree)) {
            Launch::Command(argv) => argv
                .into_iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect(),
            Launch::DefaultHandler(path) => {
                panic!("expected a command, got the default handler for {path:?}")
            }
        }
    }

    fn entry(detected: &[DetectedTarget], target: Target) -> &DetectedTarget {
        detected
            .iter()
            .find(|entry| entry.target == target)
            .unwrap_or_else(|| panic!("{} is not in the menu", target.as_str()))
    }

    #[test]
    fn a_shim_on_path_is_what_the_editor_is_launched_with() {
        let detected = detect(&mac(), &FakeProbe::with_shims(&["code", "zed"]));

        assert_eq!(
            targets_of(&detected),
            vec![
                Target::VsCode,
                Target::Zed,
                Target::Terminal,
                Target::FileManager
            ],
            "Cursor has neither a shim nor a bundle and must not appear at all",
        );
        assert_eq!(
            argv(
                entry(&detected, Target::VsCode),
                "/src/my repo/worktrees/t1"
            ),
            vec!["code", "/src/my repo/worktrees/t1"],
            "the path is an argument, never a word in a shell string",
        );
    }

    #[test]
    fn an_editor_installed_without_its_shim_is_still_offered() {
        // The macOS case this module exists for: VS Code ships `code` only
        // after the user runs "Install 'code' command in PATH", and an app
        // that is plainly installed must not vanish from the menu because of
        // a step nobody took.
        let probe = FakeProbe::default().and_installed(&[&native("/Applications/Visual Studio Code.app")]);

        let detected = detect(&mac(), &probe);

        assert!(targets_of(&detected).contains(&Target::VsCode));
        assert_eq!(
            argv(entry(&detected, Target::VsCode), "/src/my repo/wt"),
            vec![
                MACOS_OPEN.to_string(),
                "-a".to_string(),
                native("/Applications/Visual Studio Code.app"),
                "/src/my repo/wt".to_string()
            ],
        );
    }

    #[test]
    fn a_user_installed_bundle_wins_over_the_system_one() {
        // Both installed is a real machine, and the per-user copy is the one
        // being kept up to date.
        let probe = FakeProbe::default().and_installed(&[
            &native("/Users/ea/Applications/Cursor.app"),
            &native("/Applications/Cursor.app"),
        ]);

        let detected = detect(&mac(), &probe);

        assert_eq!(
            argv(entry(&detected, Target::Cursor), "/wt"),
            vec![
                MACOS_OPEN.to_string(),
                "-a".to_string(),
                native("/Users/ea/Applications/Cursor.app"),
                "/wt".to_string()
            ],
        );
    }

    #[test]
    fn an_editor_with_neither_probe_never_appears() {
        // The requirement, stated as its own test: a menu that lists Cursor and
        // does nothing when clicked is worse than one that never mentions it.
        let detected = detect(&mac(), &FakeProbe::default());

        assert_eq!(
            targets_of(&detected),
            vec![Target::Terminal, Target::FileManager],
            "with all three editors absent the menu is the terminal and the file manager",
        );
        assert!(resolve(Target::Zed, &mac(), &FakeProbe::default()).is_none());
    }

    #[test]
    fn the_file_manager_needs_no_probe_and_goes_through_the_default_handler() {
        let detected = detect(&Machine::bare(Platform::Linux), &FakeProbe::default());

        assert_eq!(
            entry(&detected, Target::FileManager).launch(Path::new("/src/my repo/wt")),
            Launch::DefaultHandler(PathBuf::from("/src/my repo/wt")),
            "the same call task 007's reveal_task_worktree already makes",
        );
    }

    #[test]
    fn a_mac_terminal_prefers_iterm_when_it_is_installed() {
        let with_iterm = FakeProbe::default().and_installed(&[&native("/Applications/iTerm.app")]);
        let detected = detect(&mac(), &with_iterm);
        assert_eq!(
            argv(entry(&detected, Target::Terminal), "/wt"),
            vec![
                MACOS_OPEN.to_string(),
                "-a".to_string(),
                native("/Applications/iTerm.app"),
                "/wt".to_string()
            ],
        );

        // And falls back to the one macOS always has.
        let plain =
            FakeProbe::default().and_installed(&[&native("/System/Applications/Utilities/Terminal.app")]);
        let detected = detect(&mac(), &plain);
        assert_eq!(
            argv(entry(&detected, Target::Terminal), "/wt"),
            vec![
                MACOS_OPEN.to_string(),
                "-a".to_string(),
                native("/System/Applications/Utilities/Terminal.app"),
                "/wt".to_string()
            ],
        );
    }

    #[test]
    fn windows_finds_a_per_user_install_and_windows_terminal() {
        let local = PathBuf::from(r"C:\Users\ea\AppData\Local");
        let machine = Machine {
            platform: Platform::Windows,
            local_app_data: Some(local.clone()),
            program_files: Some(PathBuf::from(r"C:\Program Files")),
            ..Machine::bare(Platform::Windows)
        };
        // Composed rather than spelled out, because `PathBuf::join` uses the
        // *host's* separator — this test asserts the Windows candidate list
        // from whatever machine runs the suite, which is the whole point of
        // `Platform` being a value.
        let installed = local
            .join("Programs")
            .join("Microsoft VS Code")
            .join("Code.exe");
        let probe = FakeProbe::with_shims(&["wt"]).and_installed(&[&installed.to_string_lossy()]);

        let detected = detect(&machine, &probe);

        assert_eq!(
            argv(entry(&detected, Target::VsCode), r"C:\src\my repo\wt"),
            vec![
                installed.to_string_lossy().into_owned(),
                r"C:\src\my repo\wt".to_string()
            ],
            "an executable is run directly; `open -a` is a macOS idea",
        );
        assert_eq!(
            argv(entry(&detected, Target::Terminal), r"C:\src\my repo\wt"),
            vec!["wt", "-d", r"C:\src\my repo\wt"],
        );
    }

    #[test]
    fn windows_without_windows_terminal_falls_back_to_cmd_without_a_shell_string() {
        let detected = detect(&Machine::bare(Platform::Windows), &FakeProbe::default());

        assert_eq!(
            argv(entry(&detected, Target::Terminal), r"C:\src\my repo\wt"),
            vec![
                "cmd.exe",
                "/C",
                "start",
                "cmd.exe",
                "/K",
                "cd",
                "/D",
                r"C:\src\my repo\wt"
            ],
            "every part is its own argument — a path with a space is the normal case here",
        );
    }

    #[test]
    fn a_linux_terminal_carries_its_own_working_directory_flag() {
        // The reason the terminal is a table rather than a fourth editor: these
        // do not agree on how to spell "start here".
        let konsole = detect(
            &Machine::bare(Platform::Linux),
            &FakeProbe::with_shims(&["konsole"]),
        );
        assert_eq!(
            argv(entry(&konsole, Target::Terminal), "/src/my repo/wt"),
            vec!["konsole", "--workdir", "/src/my repo/wt"],
        );

        let kitty = detect(
            &Machine::bare(Platform::Linux),
            &FakeProbe::with_shims(&["kitty"]),
        );
        assert_eq!(
            argv(entry(&kitty, Target::Terminal), "/src/my repo/wt"),
            vec!["kitty", "--directory", "/src/my repo/wt"],
        );
    }

    #[test]
    fn a_linux_machine_with_no_terminal_at_all_still_offers_the_file_manager() {
        let detected = detect(&Machine::bare(Platform::Linux), &FakeProbe::default());

        assert_eq!(
            targets_of(&detected),
            vec![Target::FileManager],
            "a terminal is the one non-editor that can genuinely be absent",
        );
    }

    #[test]
    fn a_linux_editor_outside_path_is_found_at_its_packaged_location() {
        let machine = Machine {
            home: Some(PathBuf::from("/home/ea")),
            ..Machine::bare(Platform::Linux)
        };
        let probe = FakeProbe::default().and_installed(&[&native("/snap/bin/zed")]);

        let detected = detect(&machine, &probe);

        assert_eq!(
            argv(entry(&detected, Target::Zed), "/src/my repo/wt"),
            vec![native("/snap/bin/zed"), "/src/my repo/wt".to_string()],
        );
    }

    #[test]
    fn the_menu_keeps_one_order_however_the_probes_answered() {
        // A menu that reshuffles between two probes is a menu the user has to
        // read every time instead of aiming at.
        let everything = FakeProbe::with_shims(&["zed", "cursor", "code"])
            .and_installed(&[&native("/Applications/iTerm.app")]);

        assert_eq!(
            targets_of(&detect(&mac(), &everything)),
            Target::ALL.to_vec(),
        );
    }

    #[test]
    fn every_target_name_round_trips_through_its_spelling() {
        for target in Target::ALL {
            assert_eq!(
                serde_json::to_value(target).expect("a target must serialize"),
                serde_json::Value::String(target.as_str().to_string()),
            );
        }
    }
}
