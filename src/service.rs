//! Running the scanner at boot (#23, spec US 42, ADR-0007).
//!
//! `radio-scout service install` and its five siblings, across three service
//! managers that agree on nothing: systemd wants an INI unit and `systemctl`,
//! launchd wants an XML property list and `launchctl`, and Windows wants a
//! UTF-16 task definition and `schtasks`.
//!
//! **The platform is a value, not a `cfg`.** [`Manager::plan`] takes an
//! [`Action`] and returns a [`Plan`] — an ordered script of [`Step`]s: make
//! this directory, write these bytes there, run this command, and whether its
//! failure matters. Nothing here is compiled conditionally, so every
//! platform's definition renders, and is asserted on, wherever the suite
//! happens to run. That is the whole reason Windows gets a *scheduled task*
//! rather than a real service: a service has to speak the service-control
//! protocol from inside the process, which would be Windows-only code that CI
//! compiles and never executes (ADR-0009 has no layer that could run it),
//! whereas a boot-triggered task runs an ordinary executable unmodified and
//! everything platform-specific about it is text this module can be tested on.
//!
//! The three `*_plan` functions deliberately do not share a skeleton. Their
//! *shapes* rhyme — install writes then registers, uninstall deregisters then
//! removes — but their contents have nothing in common, and a template with
//! three sets of hooks would hide each platform's script behind an abstraction
//! that exists only because the shapes rhyme.
//!
//! Being data buys two things that matter more than the tidiness:
//!
//! - **`--print` is a dry run that cannot lie.** It renders the same [`Plan`]
//!   value the real run applies, rather than describing a plan built some other
//!   way. rdio-scanner's `-service install` (via `kardianos/service`) has no
//!   equivalent: it registers a service and there is no way to see what it
//!   registered.
//! - **What the definition *says* is testable.** The systemd unit is confined
//!   (`ProtectSystem=strict` over one `ReadWritePaths`, a `@system-service`
//!   syscall filter, an empty capability set) and least-privileged — the
//!   capability to bind a privileged port is granted only to an install that
//!   asked for one, rather than by running the whole scanner as root, which is
//!   what rdio's documentation tells operators to do. Every one of those is an
//!   assertion below.
//!
//! What this module does *not* decide is what the service will be configured
//! with: [`super::config::service_params`] resolves that, refuses the one flag
//! that could put a credential into a world-readable file, and hands the result
//! here as [`Params`].

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use tracing::{debug, info, warn};

/// Which service manager a plan is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// Linux, and the one that matters: the Pi.
    Systemd,
    /// macOS.
    Launchd,
    /// Windows.
    ///
    /// A *scheduled task*, not a service: a Windows service has to speak the
    /// service-control protocol from inside the process, and a binary that did
    /// would carry Windows-only code no test in this repository could ever run
    /// — CI compiles for Windows, it never runs there. A boot-triggered task
    /// runs an ordinary executable unmodified, so everything platform-specific
    /// about it is the text below, which is asserted on wherever the suite runs.
    ScheduledTask,
}

/// What the operator asked for — and, verbatim, the verbs
/// `radio-scout service …` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Subcommand)]
pub enum Action {
    /// Register the service so the scanner starts at boot.
    Install,
    /// Remove it.
    Uninstall,
    /// Start it now.
    Start,
    /// Stop it now.
    Stop,
    /// Stop it and start it again.
    Restart,
    /// Ask the service manager how it is doing.
    Status,
}

/// The scanner a service will run.
#[derive(Debug, Clone)]
pub struct Params {
    /// Absolute path to the `radio-scout` binary.
    pub exec: PathBuf,
    /// The arguments after it — the settings baked into the service.
    pub args: Vec<String>,
    /// Where state lives.
    pub base_dir: PathBuf,
    /// The port it will bind.
    pub port: u16,
    /// The account to run as.
    pub user: Option<String>,
}

/// How a [`PlannedFile`]'s text becomes bytes on the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// UTF-16, little-endian, with a byte-order mark.
    ///
    /// `schtasks /Create /XML` rejects a UTF-8 task definition with
    /// `ERROR: The task XML is malformed` — it reads the file as Unicode and
    /// finds the declaration is not one. Nothing about the *content* is wrong,
    /// which is what makes it worth a type rather than a comment.
    Utf16Le,
}

/// A file the service manager reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub contents: String,
    pub encoding: Encoding,
}

impl PlannedFile {
    /// What actually lands on the disk.
    pub fn bytes(&self) -> Vec<u8> {
        match self.encoding {
            Encoding::Utf8 => self.contents.clone().into_bytes(),
            Encoding::Utf16Le => std::iter::once(0xFEFF_u16)
                .chain(self.contents.encode_utf16())
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }
}

/// One command to run, program first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv(pub Vec<String>);

impl Argv {
    fn new<'a>(args: impl IntoIterator<Item = &'a str>) -> Self {
        Argv(args.into_iter().map(str::to_string).collect())
    }
}

/// One thing an [`Action`] does. Order matters — an uninstall has to disable
/// the service *before* the unit describing it is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Make sure a directory exists, and its parents with it.
    Directory(PathBuf),
    /// Write a file the service manager reads.
    Write(PlannedFile),
    /// Remove one, if it is there.
    Remove(PathBuf),
    /// Run a command.
    Run {
        argv: Argv,
        /// A non-zero exit is reported and stepped over rather than ending the
        /// run. True for the parts of an uninstall that a half-installed
        /// service would fail — cleaning up has to work on the mess, not only
        /// on the tidy case.
        optional: bool,
    },
}

/// Everything an [`Action`] would do, as an ordered script of data — so what a
/// platform does is asserted on wherever the tests happen to run, rather than
/// only on that platform. It is also exactly what `--print` shows: the dry run
/// and the real run read the same plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    /// Every file this plan writes.
    #[cfg(test)]
    fn written(&self) -> Vec<&PlannedFile> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Step::Write(file) => Some(file),
                _ => None,
            })
            .collect()
    }

    /// The contents planned for `path`.
    #[cfg(test)]
    fn file(&self, path: &str) -> &str {
        let written = self.written();
        written
            .iter()
            .find(|file| file.path == Path::new(path))
            .unwrap_or_else(|| panic!("no file planned at {path}: {written:?}"))
            .contents
            .as_str()
    }

    /// The plan as an operator reads it — what `--print` shows.
    ///
    /// Deliberately the *same* value the real run applies, rendered: a dry run
    /// that describes a plan built some other way is a dry run that can lie.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            match step {
                Step::Directory(path) => {
                    out.push_str(&format!("--- mkdir {} ---\n", path.display()))
                }
                Step::Write(file) => {
                    out.push_str(&format!("--- write {} ---\n", file.path.display()));
                    out.push_str(&file.contents);
                    if !file.contents.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Step::Remove(path) => out.push_str(&format!("--- remove {} ---\n", path.display())),
                Step::Run { argv, optional } => out.push_str(&format!(
                    "--- run {}{} ---\n",
                    argv,
                    match optional {
                        true => " (failure is not fatal)",
                        false => "",
                    }
                )),
            }
        }
        out
    }
}

/// Why applying a plan stopped.
#[derive(Debug)]
pub enum Error {
    /// A file could not be written or removed.
    File { path: PathBuf, source: io::Error },
    /// A command could not be started at all — the service manager's own tool
    /// is not on this machine.
    Spawn { argv: Argv, source: io::Error },
    /// It ran, and refused.
    Refused { argv: Argv, status: ExitStatus },
    /// Nothing here knows how to run a service on this platform.
    Unsupported { os: &'static str },
    /// An account was named on a platform whose service cannot honour one.
    NoAccount,
}

impl std::fmt::Display for Argv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join(" "))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Every file a plan writes lives somewhere only root may write, so
            // this is the most likely failure of the whole subcommand — and the
            // bare OS error names neither the fix nor even, on some platforms,
            // the file.
            Error::File { path, source } if source.kind() == ErrorKind::PermissionDenied => {
                write!(
                    f,
                    "{}: permission denied — installing a service needs root, so try `sudo {NAME} service …`",
                    path.display()
                )
            }
            Error::File { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Spawn { argv, source } => write!(
                f,
                "could not run `{argv}`: {source} — is this the service manager this host uses?"
            ),
            Error::Refused { argv, status } => write!(f, "`{argv}` exited with {status}"),
            Error::Unsupported { os } => write!(
                f,
                "no service manager is known for {os}; run `{NAME}` from whatever starts programs on this platform"
            ),
            Error::NoAccount => write!(
                f,
                "--user is not supported on Windows: a scheduled task that runs as a named account needs that account's password stored with it, so the task runs as the system account"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// `path`, made absolute against `cwd`.
///
/// Everything a service definition names has to be absolute: the working
/// directory it runs from is the service manager's, not the shell's, so a
/// relative `./radio-scout-data` in a unit file would resolve against `/` and
/// the scanner would come up on an empty archive beside the one it was
/// installed to serve.
/// The `.` components come out, because the default base directory *is*
/// `./radio-scout-data` — so without this every definition an operator reads
/// names `/home/pi/./radio-scout-data`, three times over (`ExecStart`,
/// `WorkingDirectory`, `ReadWritePaths`). `..` is left alone: resolving it
/// lexically is wrong the moment a symlink is involved, and the kernel resolves
/// it correctly anyway.
pub fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    let joined = match path.is_absolute() {
        true => path.to_path_buf(),
        false => cwd.join(path),
    };
    joined
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect()
}

/// Run `radio-scout service …`: work out what this host uses, build the plan,
/// then either show it or carry it out.
pub fn dispatch(action: Action, print: bool, params: &Params) -> Result<(), Error> {
    let manager = Manager::detect().ok_or(Error::Unsupported {
        os: std::env::consts::OS,
    })?;
    manager.check(params)?;
    carry_out(&manager.plan(action, params), print)
}

/// Show the plan, or do it.
fn carry_out(plan: &Plan, print: bool) -> Result<(), Error> {
    match print {
        true => {
            show(plan);
            Ok(())
        }
        false => apply(plan),
    }
}

/// A dry run's output is a *document* — the file an operator is about to have
/// written on their behalf, and the commands that will run against their
/// machine — so it goes to stdout whole, rather than through `tracing` a line
/// at a time with a timestamp in front of every line of the unit file.
///
/// This is the third narrow exemption from ADR-0011 rule 1, and it is the same
/// one `examples/feed.rs` carries: a command whose stdout *is* its product. The
/// rule's purpose is untouched — a plan holds no credential ([`super::config::service_args`]
/// refuses the one flag that could carry one), and everything this subcommand
/// *does* still goes through `tracing`.
#[allow(clippy::print_stdout)]
fn show(plan: &Plan) {
    print!("{}", plan.render());
}

/// Carry out a plan: write what it writes, remove what it removes, run what it
/// runs, in order, stopping at the first required step that fails.
pub fn apply(plan: &Plan) -> Result<(), Error> {
    for step in &plan.steps {
        match step {
            Step::Directory(path) => {
                std::fs::create_dir_all(path).map_err(|source| Error::File {
                    path: path.clone(),
                    source,
                })?;
                debug!(path = %path.display(), "directory is there");
            }
            Step::Write(file) => {
                std::fs::write(&file.path, file.bytes()).map_err(|source| Error::File {
                    path: file.path.clone(),
                    source,
                })?;
                info!(path = %file.path.display(), "wrote");
            }
            Step::Remove(path) => match std::fs::remove_file(path) {
                Ok(()) => info!(path = %path.display(), "removed"),
                // Uninstalling something that is already half-gone is exactly
                // when uninstall is run, so a file that isn't there is the
                // outcome asked for rather than a failure.
                Err(source) if source.kind() == ErrorKind::NotFound => {
                    debug!(path = %path.display(), "nothing to remove")
                }
                Err(source) => {
                    return Err(Error::File {
                        path: path.clone(),
                        source,
                    });
                }
            },
            Step::Run { argv, optional } => {
                let (program, arguments) = argv.0.split_first().expect("a command has a program");
                info!(command = %argv, "running");
                let status = Command::new(program)
                    .args(arguments)
                    .status()
                    .map_err(|source| Error::Spawn {
                        argv: argv.clone(),
                        source,
                    })?;
                if !status.success() {
                    if !optional {
                        return Err(Error::Refused {
                            argv: argv.clone(),
                            status,
                        });
                    }
                    warn!(command = %argv, %status, "carrying on");
                }
            }
        }
    }
    Ok(())
}

/// A command that must succeed.
fn run<'a>(args: impl IntoIterator<Item = &'a str>) -> Step {
    Step::Run {
        argv: Argv::new(args),
        optional: false,
    }
}

/// A command whose failure is not the operator's problem.
fn try_run<'a>(args: impl IntoIterator<Item = &'a str>) -> Step {
    Step::Run {
        argv: Argv::new(args),
        optional: true,
    }
}

/// The service's name everywhere it is named: the systemd unit, the launchd
/// label's tail, the scheduled task, and the binary itself.
pub const NAME: &str = "radio-scout";

impl Manager {
    /// The service manager `os` uses, where `os` is spelled the way
    /// [`std::env::consts::OS`] spells it. `None` means a platform we have
    /// nothing to say about, which is better than guessing at one.
    pub fn for_os(os: &str) -> Option<Manager> {
        match os {
            "linux" => Some(Manager::Systemd),
            "macos" => Some(Manager::Launchd),
            "windows" => Some(Manager::ScheduledTask),
            _ => None,
        }
    }

    /// The one this host has.
    pub fn detect() -> Option<Manager> {
        Manager::for_os(std::env::consts::OS)
    }

    /// What this service manager cannot express, refused before anything is
    /// written.
    ///
    /// A [`Plan`] is data and cannot fail, so the one option that does not
    /// survive every platform is checked here instead — loudly, because the
    /// alternative is a scheduled task that quietly runs as the wrong account.
    pub fn check(self, params: &Params) -> Result<(), Error> {
        match (self, &params.user) {
            (Manager::ScheduledTask, Some(_)) => Err(Error::NoAccount),
            _ => Ok(()),
        }
    }

    /// What `action` does on this service manager.
    pub fn plan(self, action: Action, params: &Params) -> Plan {
        match self {
            Manager::Systemd => systemd_plan(action, params),
            Manager::Launchd => launchd_plan(action, params),
            Manager::ScheduledTask => scheduled_task_plan(action, params),
        }
    }
}

/// What every install does before it writes anything: make the directory the
/// service will run from, and — if it will run as somebody — hand it over.
///
/// Both exist because a service is routinely installed *before* the scanner has
/// ever been run, so nothing has created `base_dir` yet. Every definition names
/// it (a working directory, a writable path, a log, and on Windows the task
/// file itself lives in it), and the account named by `--user` has to be able
/// to write to it or the service starts and immediately cannot open its own
/// database. Doing the `chown` here also turns a misspelled account into a
/// failure at install time, which names it, rather than a failure at the next
/// boot, which does not.
fn install(params: &Params, rest: impl IntoIterator<Item = Step>) -> Vec<Step> {
    let base_dir = params.base_dir.to_string_lossy().into_owned();
    let mut steps = vec![Step::Directory(params.base_dir.clone())];
    if let Some(user) = &params.user {
        steps.push(run(["chown", "-R", user, &base_dir]));
    }
    steps.extend(rest);
    steps
}

fn systemd_plan(action: Action, params: &Params) -> Plan {
    let unit = systemd_unit_name();
    let steps = match action {
        Action::Install => install(
            params,
            [
                Step::Write(PlannedFile {
                    path: systemd_unit_path(),
                    contents: systemd_unit(params),
                    encoding: Encoding::Utf8,
                }),
                run(["systemctl", "daemon-reload"]),
                // `--now`: an install is a scanner that is running *and* one
                // that comes back after a reboot. rdio needs a second command.
                run(["systemctl", "enable", "--now", &unit]),
            ],
        ),
        Action::Uninstall => vec![
            try_run(["systemctl", "disable", "--now", &unit]),
            Step::Remove(systemd_unit_path()),
            run(["systemctl", "daemon-reload"]),
        ],
        Action::Start => vec![run(["systemctl", "start", &unit])],
        Action::Stop => vec![run(["systemctl", "stop", &unit])],
        Action::Restart => vec![run(["systemctl", "restart", &unit])],
        Action::Status => vec![try_run(["systemctl", "status", "--no-pager", &unit])],
    };
    Plan { steps }
}

/// The launchd label: reverse DNS, from where the project lives.
pub const LAUNCHD_LABEL: &str = "io.github.fxllencode.radio-scout";

fn launchd_plist_path() -> PathBuf {
    PathBuf::from("/Library/LaunchDaemons").join(format!("{LAUNCHD_LABEL}.plist"))
}

fn launchd_plan(action: Action, params: &Params) -> Plan {
    let path = launchd_plist_path();
    let path = path.to_string_lossy().into_owned();
    // Every control verb addresses the service by its *target* — the domain it
    // was bootstrapped into plus the label — not by the file.
    let target = format!("system/{LAUNCHD_LABEL}");
    let steps = match action {
        Action::Install => install(
            params,
            [
                Step::Write(PlannedFile {
                    path: launchd_plist_path(),
                    contents: launchd_plist(params),
                    encoding: Encoding::Utf8,
                }),
                // A label the operator (or a previous uninstall) disabled stays
                // disabled across a bootstrap, so re-enable it explicitly.
                run(["launchctl", "enable", &target]),
                run(["launchctl", "bootstrap", "system", &path]),
            ],
        ),
        Action::Uninstall => vec![
            try_run(["launchctl", "bootout", &target]),
            Step::Remove(launchd_plist_path()),
        ],
        Action::Start => vec![run(["launchctl", "kickstart", &target])],
        Action::Stop => vec![run(["launchctl", "kill", "SIGTERM", &target])],
        Action::Restart => vec![run(["launchctl", "kickstart", "-k", &target])],
        Action::Status => vec![try_run(["launchctl", "print", &target])],
    };
    Plan { steps }
}

/// The property list launchd reads.
///
/// `StandardOutPath` is the one place this project writes a log to a file, and
/// it is not the process doing it: macOS has no journald, and launchd sends a
/// daemon's stdout to `/dev/null` unless a path is given. The binary still only
/// writes stdout (ADR-0011) — launchd redirects it, exactly as journald
/// captures it on Linux. It lands beside the state it describes so an operator
/// looking for one finds the other.
fn launchd_plist(params: &Params) -> String {
    let mut arguments = String::new();
    for argument in std::iter::once(params.exec.to_string_lossy().into_owned())
        .chain(params.args.iter().cloned())
    {
        arguments.push_str(&format!(
            "      <string>{}</string>\n",
            xml_escape(&argument)
        ));
    }
    let base_dir = xml_escape(&params.base_dir.to_string_lossy());
    let log = format!("{base_dir}/{NAME}.log");
    let user = params.user.as_ref().map_or_else(String::new, |user| {
        format!(
            "    <key>UserName</key>\n    <string>{}</string>\n",
            xml_escape(user)
        )
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `{NAME} service install`. Reinstalling overwrites this file. -->
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>WorkingDirectory</key>
    <string>{base_dir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
      <key>SuccessfulExit</key>
      <false/>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
{user}    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
  </dict>
</plist>
"#
    )
}

/// The scheduled task's name, as `schtasks /TN` spells it.
pub const TASK_NAME: &str = "Radio-Scout";

/// Where the definition is kept. Beside the state it describes, rather than in
/// a temporary file, so `/Create` can be re-run and an operator can read what
/// was registered on their behalf.
fn task_definition_path(params: &Params) -> PathBuf {
    params.base_dir.join(format!("{NAME}-task.xml"))
}

fn scheduled_task_plan(action: Action, params: &Params) -> Plan {
    let definition = task_definition_path(params);
    let definition_arg = definition.to_string_lossy().into_owned();
    let steps = match action {
        Action::Install => install(
            params,
            [
                Step::Write(PlannedFile {
                    path: definition.clone(),
                    contents: task_definition(params),
                    encoding: Encoding::Utf16Le,
                }),
                run([
                    "schtasks",
                    "/Create",
                    "/TN",
                    TASK_NAME,
                    "/XML",
                    &definition_arg,
                    "/F",
                ]),
                run(["schtasks", "/Run", "/TN", TASK_NAME]),
            ],
        ),
        Action::Uninstall => vec![
            try_run(["schtasks", "/End", "/TN", TASK_NAME]),
            try_run(["schtasks", "/Delete", "/TN", TASK_NAME, "/F"]),
            Step::Remove(definition),
        ],
        Action::Start => vec![run(["schtasks", "/Run", "/TN", TASK_NAME])],
        Action::Stop => vec![run(["schtasks", "/End", "/TN", TASK_NAME])],
        Action::Restart => vec![
            try_run(["schtasks", "/End", "/TN", TASK_NAME]),
            run(["schtasks", "/Run", "/TN", TASK_NAME]),
        ],
        Action::Status => vec![try_run([
            "schtasks", "/Query", "/TN", TASK_NAME, "/V", "/FO", "LIST",
        ])],
    };
    Plan { steps }
}

/// The task definition `schtasks /Create /XML` reads.
///
/// The settings are all one decision: a scanner is infrastructure, not a
/// chore. It starts at boot, it is not deferred or skipped for being on
/// battery, it is never stopped for running too long, and if it dies it is
/// started again — the same four promises the systemd unit makes with
/// `Restart=on-failure` and the plist with `KeepAlive`.
///
/// It runs as the system account, unconditionally. A named account would need
/// its **password** stored with the task (`LogonType`s that don't are either
/// interactive-only or need a privilege the account may not hold), and
/// [`Params::user`] is refused rather than honoured on this platform — see
/// [`Manager::check`].
fn task_definition(params: &Params) -> String {
    let command = xml_escape(&params.exec.to_string_lossy());
    let arguments = xml_escape(
        &params
            .args
            .iter()
            .map(|argument| windows_quote(argument))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let working_directory = xml_escape(&params.base_dir.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<!-- Written by `{NAME} service install`. Reinstalling overwrites this file. -->
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Radio-Scout scanner audio server</Description>
    <URI>\{TASK_NAME}</URI>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <Enabled>true</Enabled>
    </BootTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>S-1-5-18</UserId>
      <LogonType>ServiceAccount</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
      <WorkingDirectory>{working_directory}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// One argument, as the Windows command line the task runs would need it.
///
/// Windows splits a command line inside the process it starts, so a path with a
/// space in it — `C:\Program Files\…`, the default place to install anything —
/// arrives as two arguments unless it is quoted here.
fn windows_quote(argument: &str) -> String {
    match argument.contains([' ', '\t', '"']) {
        false => argument.to_string(),
        true => format!("\"{}\"", argument.replace('"', "\\\"")),
    }
}

/// The characters that cannot appear literally in XML element content.
///
/// Only these three: everything interpolated below is element content, never an
/// attribute value, so a quote needs no escape — and leaving it alone keeps the
/// Windows quoting in `<Arguments>` legible to whoever reads the file.
fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn systemd_unit_name() -> String {
    format!("{NAME}.service")
}

fn systemd_unit_path() -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(systemd_unit_name())
}

/// The lowest port an unprivileged process may bind on Linux.
const FIRST_UNPRIVILEGED_PORT: u16 = 1024;

/// The unit file.
///
/// Two things distinguish it from the unit rdio-scanner's `-service install`
/// writes (via `kardianos/service`, whose template carries none of this). It is
/// **confined**: `ProtectSystem=strict` makes the whole filesystem read-only
/// except the one directory the scanner owns, and the syscall filter, the
/// namespace restrictions and `MemoryDenyWriteExecute` cost a Rust binary
/// nothing because it neither JITs nor forks. And it is **least-privileged**:
/// the capability to bind a privileged port is granted only to an install that
/// asked for one, rather than to every install by running the whole thing as
/// root.
///
/// `ProtectHome=` is deliberately absent. It would be the obvious next line, but
/// `yes` mounts a tmpfs over `/home`, and a `base_dir` under a home directory —
/// what a `curl | sh` install on a Pi produces — would then be invisible to the
/// service rather than merely read-only. `ProtectSystem=strict` already denies
/// writes there; [`ReadWritePaths`] re-opens the one path that needs them.
///
/// [`ReadWritePaths`]: https://www.freedesktop.org/software/systemd/man/systemd.exec.html#ReadWritePaths=
fn systemd_unit(params: &Params) -> String {
    let mut unit = String::new();
    unit.push_str(&format!(
        "# {NAME}.service — written by `{NAME} service install`.\n\
         # Reinstalling overwrites this file; edits belong in the configuration\n\
         # the ExecStart line points at.\n\
         [Unit]\n\
         Description=Radio-Scout scanner audio server\n\
         Documentation=https://github.com/FxllenCode/radio-scout\n\
         # `network-online` rather than `network`: an ingest key is registered\n\
         # and a store is opened at boot, and both can want a resolver.\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=exec\n\
         ExecStart={exec}\n\
         WorkingDirectory={base_dir}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # ADR-0011: the scanner logs to stdout and journald owns persistence\n\
         # and rotation. Nothing writes a log file.\n\
         StandardOutput=journal\n\
         StandardError=journal\n",
        exec = params.command_line(),
        base_dir = params.base_dir.display(),
    ));
    if let Some(user) = &params.user {
        // No `Group=`. Naming one that does not exist is a unit that refuses to
        // start, and `useradd --system radio-scout` does not always make a
        // group of the same name — left unset, systemd uses the account's own
        // primary group, which always exists.
        unit.push_str(&format!("User={user}\n"));
    }
    if params.port < FIRST_UNPRIVILEGED_PORT {
        unit.push_str(&format!(
            "# Port {port} is privileged, so grant the one capability that binds\n\
             # it rather than running the scanner as root.\n\
             AmbientCapabilities=CAP_NET_BIND_SERVICE\n\
             CapabilityBoundingSet=CAP_NET_BIND_SERVICE\n",
            port = params.port,
        ));
    } else {
        unit.push_str("CapabilityBoundingSet=\n");
    }
    unit.push_str(&format!(
        "\n\
         # The scanner reads its own state and nothing else on the disk.\n\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n\
         ReadWritePaths={base_dir}\n\
         PrivateTmp=yes\n\
         PrivateDevices=yes\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectControlGroups=yes\n\
         RestrictNamespaces=yes\n\
         RestrictRealtime=yes\n\
         RestrictSUIDSGID=yes\n\
         LockPersonality=yes\n\
         MemoryDenyWriteExecute=yes\n\
         RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\n\
         SystemCallArchitectures=native\n\
         SystemCallFilter=@system-service\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        base_dir = params.base_dir.display(),
    ));
    unit
}

impl Params {
    /// The binary and its arguments, as one command line.
    fn command_line(&self) -> String {
        std::iter::once(self.exec.display().to_string())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const LAUNCHD_PLIST: &str = "/Library/LaunchDaemons/io.github.fxllencode.radio-scout.plist";

    /// Windows never runs as a named account — [`check`] refuses one — so its
    /// plans are always built without one.
    fn windows_params() -> Params {
        Params {
            user: None,
            ..params()
        }
    }

    fn params() -> Params {
        Params {
            exec: PathBuf::from("/usr/local/bin/radio-scout"),
            args: vec!["--base-dir".into(), "/var/lib/radio-scout".into()],
            base_dir: PathBuf::from("/var/lib/radio-scout"),
            port: 3000,
            user: Some("radio-scout".into()),
        }
    }

    #[test]
    fn a_systemd_install_writes_a_unit_that_starts_the_scanner_at_boot() {
        let plan = Manager::Systemd.plan(Action::Install, &params());

        let unit = plan.file("/etc/systemd/system/radio-scout.service");
        assert!(
            unit.contains("ExecStart=/usr/local/bin/radio-scout --base-dir /var/lib/radio-scout"),
            "the unit runs the binary with the settings it was installed with:\n{unit}"
        );
        assert!(
            unit.contains("WantedBy=multi-user.target"),
            "and is wanted at boot:\n{unit}"
        );
        // The tail, because every install opens with `install`'s prologue.
        //
        // `--now`, so an install is a scanner that is running as well as one
        // that will come back after a reboot. rdio-scanner needs a second
        // command (`-service start`) and says nothing about it.
        assert_eq!(
            plan.steps[plan.steps.len() - 2..],
            [
                run(["systemctl", "daemon-reload"]),
                run(["systemctl", "enable", "--now", "radio-scout.service"]),
            ],
        );
    }

    #[test]
    fn a_launchd_install_writes_a_daemon_and_bootstraps_it() {
        let plan = Manager::Launchd.plan(Action::Install, &params());

        let plist = plan.file(LAUNCHD_PLIST);
        for fragment in [
            "<string>io.github.fxllencode.radio-scout</string>",
            "<string>/usr/local/bin/radio-scout</string>",
            "<string>--base-dir</string>",
            "<string>/var/lib/radio-scout</string>",
            // At boot, and back again if it dies.
            "<key>RunAtLoad</key>",
            "<key>KeepAlive</key>",
            "<key>UserName</key>",
            "<string>radio-scout</string>",
        ] {
            assert!(plist.contains(fragment), "missing `{fragment}`:\n{plist}");
        }
        assert_eq!(
            plan.steps[plan.steps.len() - 2..],
            [
                run([
                    "launchctl",
                    "enable",
                    "system/io.github.fxllencode.radio-scout"
                ]),
                run(["launchctl", "bootstrap", "system", LAUNCHD_PLIST]),
            ],
        );
    }

    /// macOS has no journald, and launchd sends a daemon's stdout to
    /// `/dev/null` unless it is told otherwise — so without this the log an
    /// operator is told to read (ADR-0011: everything goes to stdout) does not
    /// exist anywhere.
    #[test]
    fn a_launchd_daemon_has_somewhere_for_its_stdout_to_land() {
        let plan = Manager::Launchd.plan(Action::Install, &params());

        let plist = plan.file(LAUNCHD_PLIST);
        for key in ["StandardOutPath", "StandardErrorPath"] {
            assert!(
                plist.contains(&format!(
                    "<key>{key}</key>\n    <string>/var/lib/radio-scout/radio-scout.log</string>"
                )),
                "{key} does not point into the base directory:\n{plist}"
            );
        }
    }

    /// A plist is XML, and a path is whatever the operator's disk says it is.
    /// An unescaped `&` makes the file unparseable, and launchd then refuses to
    /// load a service whose installer said it succeeded.
    #[test]
    fn a_path_that_is_not_xml_safe_is_escaped_rather_than_pasted() {
        let plan = Manager::Launchd.plan(
            Action::Install,
            &Params {
                base_dir: PathBuf::from("/srv/fire & <rescue>"),
                ..params()
            },
        );

        let plist = plan.file(LAUNCHD_PLIST);
        assert!(
            plist.contains("<string>/srv/fire &amp; &lt;rescue&gt;</string>"),
            "{plist}"
        );
        assert!(!plist.contains("fire & <rescue>"), "{plist}");
    }

    #[rstest]
    #[case::start(Action::Start, vec!["launchctl", "kickstart", "system/io.github.fxllencode.radio-scout"], false)]
    #[case::restart(Action::Restart, vec!["launchctl", "kickstart", "-k", "system/io.github.fxllencode.radio-scout"], false)]
    #[case::stop(Action::Stop, vec!["launchctl", "kill", "SIGTERM", "system/io.github.fxllencode.radio-scout"], false)]
    // `launchctl print` exits non-zero for a daemon that was never
    // bootstrapped, which is an answer rather than a failure.
    #[case::status(Action::Status, vec!["launchctl", "print", "system/io.github.fxllencode.radio-scout"], true)]
    fn controlling_a_launchd_daemon_is_one_launchctl_call(
        #[case] action: Action,
        #[case] expected: Vec<&str>,
        #[case] optional: bool,
    ) {
        let plan = Manager::Launchd.plan(action, &params());

        let expected = match optional {
            true => try_run(expected),
            false => run(expected),
        };
        assert_eq!(plan.steps, [expected]);
    }

    /// Windows has no init system a console program can join without speaking
    /// the service-control protocol, so the boot trigger is a scheduled task —
    /// which runs an ordinary executable, unmodified.
    #[test]
    fn a_windows_install_registers_a_task_that_runs_at_boot() {
        let plan = Manager::ScheduledTask.plan(Action::Install, &windows_params());

        let xml = plan.file("/var/lib/radio-scout/radio-scout-task.xml");
        for fragment in [
            "<BootTrigger>",
            "<Command>/usr/local/bin/radio-scout</Command>",
            "<Arguments>--base-dir /var/lib/radio-scout</Arguments>",
            // A scanner is not a laptop chore: it runs on battery and it is
            // never killed for taking too long.
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<RestartOnFailure>",
        ] {
            assert!(xml.contains(fragment), "missing `{fragment}`:\n{xml}");
        }
        assert_eq!(
            plan.steps[plan.steps.len() - 2..],
            [
                run([
                    "schtasks",
                    "/Create",
                    "/TN",
                    "Radio-Scout",
                    "/XML",
                    "/var/lib/radio-scout/radio-scout-task.xml",
                    "/F",
                ]),
                run(["schtasks", "/Run", "/TN", "Radio-Scout"]),
            ],
        );
    }

    /// `schtasks` reads the definition as Unicode and rejects a UTF-8 one as
    /// malformed, which is the least informative way this could fail.
    #[test]
    fn a_task_definition_is_written_as_utf16_and_a_unit_file_is_not() {
        let plan = Manager::ScheduledTask.plan(Action::Install, &windows_params());
        let task = plan.written()[0];
        assert_eq!(&task.bytes()[..2], &[0xFF, 0xFE], "byte-order mark");
        assert_eq!(&task.bytes()[2..4], b"<\0", "little-endian UTF-16");

        let plan = Manager::Systemd.plan(Action::Install, &params());
        assert!(
            plan.written()[0]
                .bytes()
                .starts_with(b"# radio-scout.service")
        );
    }

    /// The alternative is worse than refusing: `schtasks` would take the
    /// definition, and the task would then fail to start with a Windows error
    /// code nobody can map back to the flag that caused it.
    #[test]
    fn windows_refuses_an_account_rather_than_running_as_the_wrong_one() {
        let refused = Manager::ScheduledTask
            .check(&params())
            .expect_err("--user was given");
        assert!(refused.to_string().contains("--user"), "{refused}");
        assert!(refused.to_string().contains("system account"), "{refused}");

        Manager::ScheduledTask
            .check(&windows_params())
            .expect("no account named");
        Manager::Systemd
            .check(&params())
            .expect("systemd honours an account");
        Manager::Launchd
            .check(&params())
            .expect("launchd honours an account");
    }

    /// A task's arguments are one string, so one containing a space would
    /// otherwise arrive as two.
    #[test]
    fn a_task_argument_with_a_space_is_quoted() {
        let plan = Manager::ScheduledTask.plan(
            Action::Install,
            &Params {
                args: vec!["--base-dir".into(), r"C:\Program Files\scanner".into()],
                ..windows_params()
            },
        );

        let xml = plan.file("/var/lib/radio-scout/radio-scout-task.xml");
        assert!(
            xml.contains(r#"<Arguments>--base-dir "C:\Program Files\scanner"</Arguments>"#),
            "{xml}"
        );
    }

    #[rstest]
    #[case::start(Action::Start, vec![run(["schtasks", "/Run", "/TN", "Radio-Scout"])])]
    #[case::stop(Action::Stop, vec![run(["schtasks", "/End", "/TN", "Radio-Scout"])])]
    #[case::restart(Action::Restart, vec![
        try_run(["schtasks", "/End", "/TN", "Radio-Scout"]),
        run(["schtasks", "/Run", "/TN", "Radio-Scout"]),
    ])]
    #[case::status(Action::Status, vec![
        try_run(["schtasks", "/Query", "/TN", "Radio-Scout", "/V", "/FO", "LIST"]),
    ])]
    fn controlling_a_scheduled_task_is_schtasks(
        #[case] action: Action,
        #[case] expected: Vec<Step>,
    ) {
        let plan = Manager::ScheduledTask.plan(action, &windows_params());

        assert_eq!(plan.steps, expected);
    }

    /// A task that is stopped and gone is what uninstall promises, and a
    /// definition file left behind would be reinstalled by the next `/Create`.
    #[test]
    fn a_windows_uninstall_deletes_the_task_and_its_definition() {
        let plan = Manager::ScheduledTask.plan(Action::Uninstall, &windows_params());

        assert_eq!(
            plan.steps,
            [
                try_run(["schtasks", "/End", "/TN", "Radio-Scout"]),
                try_run(["schtasks", "/Delete", "/TN", "Radio-Scout", "/F"]),
                Step::Remove(PathBuf::from("/var/lib/radio-scout/radio-scout-task.xml")),
            ],
        );
    }

    /// The platform is a value everywhere else in this module, and detecting it
    /// is the one place it isn't — so the mapping is a function of a name, and
    /// only the last hop reads the name of the host we happen to be on.
    #[rstest]
    #[case::linux("linux", Some(Manager::Systemd))]
    #[case::macos("macos", Some(Manager::Launchd))]
    #[case::windows("windows", Some(Manager::ScheduledTask))]
    #[case::freebsd("freebsd", None)]
    #[case::nonsense("", None)]
    fn each_platform_maps_to_the_service_manager_it_actually_has(
        #[case] os: &str,
        #[case] expected: Option<Manager>,
    ) {
        assert_eq!(Manager::for_os(os), expected);
    }

    #[test]
    fn this_host_has_a_service_manager() {
        assert_eq!(Manager::detect(), Manager::for_os(std::env::consts::OS));
        assert!(
            Manager::detect().is_some(),
            "the suite runs on a platform we ship on"
        );
    }

    /// The plan is data; applying it is the only part that touches the machine.
    #[test]
    fn applying_a_plan_writes_its_files_and_runs_its_commands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unit = dir.path().join("radio-scout.service");
        let plan = Plan {
            steps: vec![
                Step::Write(PlannedFile {
                    path: unit.clone(),
                    contents: "[Unit]\n".into(),
                    encoding: Encoding::Utf8,
                }),
                run(["true"]),
            ],
        };

        apply(&plan).expect("apply");

        assert_eq!(std::fs::read_to_string(&unit).expect("read"), "[Unit]\n");
    }

    /// `--base-dir /srv/scanner` on a machine that has never run the scanner:
    /// the whole path has to appear, parents and all, or the definition written
    /// next names somewhere that does not exist.
    #[test]
    fn applying_a_plan_makes_the_directory_and_its_parents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base_dir = dir.path().join("srv/scanner/data");
        let plan = Plan {
            steps: vec![Step::Directory(base_dir.clone())],
        };

        apply(&plan).expect("apply");

        assert!(base_dir.is_dir());
    }

    /// The same `sudo` hint as a file, and for the same reason: `/var/lib` is
    /// not writable by whoever is typing.
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_made_says_so_with_its_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let readonly = dir.path().join("var");
        std::fs::create_dir(&readonly).expect("create");
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let plan = Plan {
            steps: vec![Step::Directory(readonly.join("lib/radio-scout"))],
        };

        let error = apply(&plan).expect_err("the parent is read-only");

        let message = error.to_string();
        assert!(message.contains("radio-scout"), "{message}");
        assert!(message.contains("sudo"), "{message}");
    }

    #[test]
    fn applying_a_plan_removes_what_it_plans_to_remove_and_tolerates_what_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let there = dir.path().join("there");
        std::fs::write(&there, "x").expect("write");
        let plan = Plan {
            steps: vec![
                Step::Remove(there.clone()),
                Step::Remove(dir.path().join("never-existed")),
            ],
        };

        apply(&plan).expect("removing what is already gone is what uninstall wants");

        assert!(!there.exists());
    }

    #[test]
    fn a_command_that_refuses_ends_the_run_unless_it_was_optional() {
        let required = Plan {
            steps: vec![run(["false"])],
        };
        let error = apply(&required).expect_err("a required command failed");
        assert!(error.to_string().contains("false"), "{error}");

        let optional = Plan {
            steps: vec![try_run(["false"])],
        };
        apply(&optional).expect("an optional command's failure is stepped over");
    }

    /// The tool being absent and the tool saying no are different problems with
    /// different fixes, so they are different errors.
    #[test]
    fn a_service_manager_that_is_not_installed_is_named_in_the_error() {
        let plan = Plan {
            steps: vec![run(["radio-scout-no-such-program"])],
        };

        let error = apply(&plan).expect_err("nothing to run");
        assert!(
            error.to_string().contains("radio-scout-no-such-program"),
            "{error}"
        );
    }

    /// Every one of these files lives somewhere only root may write, so this is
    /// the single most likely way `service install` fails — and "Permission
    /// denied (os error 13)" alone does not tell an operator what to type next.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_written_says_to_try_again_with_sudo() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let readonly = dir.path().join("etc");
        std::fs::create_dir(&readonly).expect("create");
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let plan = Plan {
            steps: vec![Step::Write(PlannedFile {
                path: readonly.join("radio-scout.service"),
                contents: "[Unit]\n".into(),
                encoding: Encoding::Utf8,
            })],
        };

        let error = apply(&plan).expect_err("the directory is read-only");

        let message = error.to_string();
        assert!(message.contains("radio-scout.service"), "{message}");
        assert!(message.contains("sudo"), "{message}");
    }

    #[rstest]
    #[case::relative("radio-scout-data", "/home/pi/radio-scout-data")]
    // `./radio-scout-data` is the *default* base directory, so this is the
    // ordinary case rather than an odd one.
    #[case::dotted("./radio-scout-data", "/home/pi/radio-scout-data")]
    #[case::dot_in_the_middle("data/./archive", "/home/pi/data/archive")]
    #[case::already_absolute("/srv/scanner", "/srv/scanner")]
    fn a_relative_directory_is_resolved_before_it_is_written_down(
        #[case] path: &str,
        #[case] expected: &str,
    ) {
        let resolved = absolute(Path::new(path), Path::new("/home/pi"));

        // Compared as *text*, not as a `PathBuf`. `Path`'s `PartialEq` runs
        // over components, which normalise `.` away — so a comparison against a
        // `PathBuf` would pass for `/home/pi/./radio-scout-data` and see none of
        // the difference that matters, which is what a service definition ends
        // up *saying*.
        assert_eq!(resolved.display().to_string(), expected);
    }

    /// Not every write fails for want of root, and the ones that don't must not
    /// be told to try `sudo` — a missing directory is not a permissions problem
    /// and sending an operator to `sudo` for it wastes the one hint they get.
    #[test]
    fn a_write_that_fails_for_some_other_reason_reports_that_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = Plan {
            steps: vec![Step::Write(PlannedFile {
                path: dir.path().join("no-such-directory/radio-scout.service"),
                contents: "[Unit]\n".into(),
                encoding: Encoding::Utf8,
            })],
        };

        let error = apply(&plan).expect_err("the parent directory does not exist");

        let message = error.to_string();
        assert!(message.contains("radio-scout.service"), "{message}");
        assert!(!message.contains("sudo"), "{message}");
    }

    /// Removing is as capable of failing as writing, and for the same reasons.
    #[test]
    fn a_removal_that_fails_for_a_reason_other_than_absence_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A directory where a file was expected: `remove_file` refuses, and it
        // is not the "already gone" that uninstall steps over.
        let occupied = dir.path().join("radio-scout.service");
        std::fs::create_dir(&occupied).expect("create");
        let plan = Plan {
            steps: vec![Step::Remove(occupied)],
        };

        let error = apply(&plan).expect_err("a directory is not a file");

        assert!(error.to_string().contains("radio-scout.service"), "{error}");
    }

    /// Nothing here can be run on a platform with no service manager, so what
    /// it says instead is the whole of the behaviour.
    #[test]
    fn an_unshipped_platform_is_told_to_start_the_scanner_some_other_way() {
        let message = Error::Unsupported { os: "haiku" }.to_string();

        assert!(message.contains("haiku"), "{message}");
        assert!(message.contains("radio-scout"), "{message}");
    }

    /// The non-printing half of [`dispatch`], on a plan that touches only a
    /// temporary directory — the real one drives the host's service manager,
    /// which is a live test rather than a suite one.
    #[test]
    fn carrying_a_plan_out_rather_than_printing_it_writes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unit = dir.path().join("radio-scout.service");
        let plan = Plan {
            steps: vec![Step::Write(PlannedFile {
                path: unit.clone(),
                contents: "[Unit]\n".into(),
                encoding: Encoding::Utf8,
            })],
        };

        carry_out(&plan, false).expect("carrying it out");

        assert_eq!(std::fs::read_to_string(&unit).expect("read"), "[Unit]\n");
    }

    /// The strong assertion here is the one that isn't written: every plan this
    /// host produces writes into `/etc` or `/Library`, so a `--print` that
    /// applied by mistake would fail with permission denied rather than pass.
    #[test]
    fn printing_a_plan_changes_nothing_on_the_machine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let params = Params {
            base_dir: dir.path().to_path_buf(),
            // No account, so this is a plan every platform would accept —
            // including the one where `check` refuses one.
            user: None,
            ..params()
        };

        dispatch(Action::Install, true, &params).expect("printing an install plan");

        assert_eq!(
            std::fs::read_dir(dir.path()).expect("read").count(),
            0,
            "a dry run wrote something"
        );
    }

    /// The other half of `--print`: what an uninstall would take away, and
    /// which of its steps are allowed to fail. A plan that showed
    /// `systemctl disable` without saying it is best-effort would read like a
    /// command whose failure aborts the cleanup.
    #[test]
    fn printing_an_uninstall_shows_what_goes_and_what_may_fail() {
        let printed = Manager::Systemd.plan(Action::Uninstall, &params()).render();

        assert_eq!(
            printed,
            "--- run systemctl disable --now radio-scout.service (failure is not fatal) ---\n\
             --- remove /etc/systemd/system/radio-scout.service ---\n\
             --- run systemctl daemon-reload ---\n"
        );
    }

    /// A file's contents run straight into the next step's header, so one
    /// without a trailing newline would print `[Install]--- run …`.
    #[test]
    fn a_printed_file_is_separated_from_what_follows_it() {
        let plan = Plan {
            steps: vec![
                Step::Write(PlannedFile {
                    path: PathBuf::from("/etc/example"),
                    contents: "no trailing newline".into(),
                    encoding: Encoding::Utf8,
                }),
                run(["true"]),
            ],
        };

        assert_eq!(
            plan.render(),
            "--- write /etc/example ---\nno trailing newline\n--- run true ---\n"
        );
    }

    /// `--print` is the same plan the real run would apply, which is the only
    /// thing that makes a dry run worth trusting.
    /// The whole artifact, for each platform. Everything else in this module
    /// asserts one decision at a time; these pin the file an operator ends up
    /// with — including the parts nobody thought to write a test about.
    #[rstest]
    #[case::systemd(Manager::Systemd, "systemd_install_plan")]
    #[case::launchd(Manager::Launchd, "launchd_install_plan")]
    #[case::scheduled_task(Manager::ScheduledTask, "scheduled_task_install_plan")]
    fn printing_a_plan_shows_every_file_and_command_in_order(
        #[case] manager: Manager,
        #[case] snapshot: &str,
    ) {
        let params = match manager {
            Manager::ScheduledTask => windows_params(),
            _ => params(),
        };
        let printed = manager.plan(Action::Install, &params).render();

        insta::assert_snapshot!(snapshot, printed);
    }

    #[test]
    fn a_launchd_uninstall_boots_the_daemon_out_before_removing_its_plist() {
        let plan = Manager::Launchd.plan(Action::Uninstall, &params());

        assert_eq!(
            plan.steps,
            [
                try_run([
                    "launchctl",
                    "bootout",
                    "system/io.github.fxllencode.radio-scout"
                ]),
                Step::Remove(PathBuf::from(LAUNCHD_PLIST)),
            ],
        );
    }

    #[rstest]
    #[case::start(Action::Start, ["systemctl", "start", "radio-scout.service"])]
    #[case::stop(Action::Stop, ["systemctl", "stop", "radio-scout.service"])]
    #[case::restart(Action::Restart, ["systemctl", "restart", "radio-scout.service"])]
    fn controlling_a_systemd_service_is_one_systemctl_call_and_no_files(
        #[case] action: Action,
        #[case] expected: [&str; 3],
    ) {
        let plan = Manager::Systemd.plan(action, &params());

        assert_eq!(plan.steps, [run(expected)]);
    }

    /// `systemctl status` exits 3 for a service that is simply not running, and
    /// a paged one waits forever on a terminal nobody is watching. Neither is
    /// an error to report.
    #[test]
    fn asking_for_status_never_pages_and_never_fails() {
        let plan = Manager::Systemd.plan(Action::Status, &params());

        assert_eq!(
            plan.steps,
            [try_run([
                "systemctl",
                "status",
                "--no-pager",
                "radio-scout.service"
            ])],
        );
    }

    /// The order is the whole point: a unit removed before it is disabled
    /// leaves a dangling `multi-user.target.wants` symlink that systemd
    /// complains about on every boot.
    #[test]
    fn a_systemd_uninstall_disables_the_service_before_removing_its_unit() {
        let plan = Manager::Systemd.plan(Action::Uninstall, &params());

        assert_eq!(
            plan.steps,
            [
                try_run(["systemctl", "disable", "--now", "radio-scout.service"]),
                Step::Remove(PathBuf::from("/etc/systemd/system/radio-scout.service")),
                run(["systemctl", "daemon-reload"]),
            ],
        );
    }

    /// The unit names `base_dir` three times — `WorkingDirectory`,
    /// `ReadWritePaths`, and the `--base-dir` it runs with — and on Windows the
    /// task definition is *written into* it. A `service install` run before the
    /// scanner has ever been started therefore installs a service that cannot
    /// start (or, on Windows, fails at the first step), which is exactly the
    /// order `docs/deploy.md` tells an operator to do it in.
    #[rstest]
    #[case::systemd(Manager::Systemd)]
    #[case::launchd(Manager::Launchd)]
    #[case::scheduled_task(Manager::ScheduledTask)]
    fn an_install_makes_the_directory_the_service_will_run_from(#[case] manager: Manager) {
        let plan = manager.plan(
            Action::Install,
            &Params {
                user: None,
                ..params()
            },
        );

        assert_eq!(
            plan.steps.first(),
            Some(&Step::Directory(PathBuf::from("/var/lib/radio-scout"))),
            "the first thing an install does is make somewhere to run from"
        );
    }

    /// The account is named in the unit but nothing else gives it the data —
    /// and a `base_dir` that a root-run first launch created is root-owned, so
    /// the service comes up and fails to open its own database. Doing it here
    /// also means a misspelled account fails *at install*, naming itself,
    /// rather than at the next boot.
    #[rstest]
    #[case::systemd(Manager::Systemd)]
    #[case::launchd(Manager::Launchd)]
    fn an_account_is_given_the_directory_it_has_to_write_to(#[case] manager: Manager) {
        let plan = manager.plan(Action::Install, &params());

        assert_eq!(
            plan.steps[1],
            run(["chown", "-R", "radio-scout", "/var/lib/radio-scout"]),
        );
    }

    #[test]
    fn with_no_account_there_is_nothing_to_hand_the_directory_to() {
        let plan = Manager::Systemd.plan(
            Action::Install,
            &Params {
                user: None,
                ..params()
            },
        );

        assert!(!plan.render().contains("chown"), "{}", plan.render());
    }

    #[test]
    fn a_systemd_unit_runs_as_its_account_and_may_write_only_its_base_dir() {
        let plan = Manager::Systemd.plan(Action::Install, &params());
        let unit = plan.file("/etc/systemd/system/radio-scout.service");

        for line in [
            "User=radio-scout",
            "Restart=on-failure",
            // ADR-0011: the process writes to stdout and journald owns
            // persistence — so a unit that redirects it elsewhere would lose
            // the log entirely.
            "StandardOutput=journal",
            // The scanner reads its own state and nothing else on the disk.
            "ProtectSystem=strict",
            "ReadWritePaths=/var/lib/radio-scout",
            "NoNewPrivileges=yes",
        ] {
            assert!(unit.contains(line), "missing `{line}`:\n{unit}");
        }
    }

    /// A port under 1024 needs a capability an unprivileged account does not
    /// have — and granting it unconditionally would hand every install a
    /// privilege it has no use for.
    #[rstest]
    #[case::http(80, true)]
    #[case::last_privileged(1023, true)]
    #[case::first_unprivileged(1024, false)]
    #[case::default(3000, false)]
    fn a_privileged_port_is_granted_the_capability_to_bind_it(
        #[case] port: u16,
        #[case] granted: bool,
    ) {
        let plan = Manager::Systemd.plan(Action::Install, &Params { port, ..params() });
        let unit = plan.file("/etc/systemd/system/radio-scout.service");

        assert_eq!(
            unit.contains("AmbientCapabilities=CAP_NET_BIND_SERVICE"),
            granted,
            "port {port}:\n{unit}"
        );
    }

    /// Without an account the unit says nothing about one, so systemd runs it
    /// as root — which is what an operator who did not name a user asked for,
    /// and what rdio-scanner does unconditionally.
    #[test]
    fn no_account_means_no_user_line_rather_than_an_empty_one() {
        let plan = Manager::Systemd.plan(
            Action::Install,
            &Params {
                user: None,
                ..params()
            },
        );

        let unit = plan.file("/etc/systemd/system/radio-scout.service");
        assert!(!unit.contains("User="), "{unit}");
    }
}
