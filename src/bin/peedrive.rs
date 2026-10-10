use anyhow::Result;
use clap::{Parser, Subcommand};
use peedrive::service;
use serde_json::json;

#[derive(Parser)]
#[command(version, about = "Sync local folder pairs with Proton Drive")]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create a dedicated test folder and verify live Proton transfers and conflict handling.
    LiveTest,
    /// Stop background sync until Peedrive is opened again or you next log in.
    Quit,
    Daemon,
    Status,
    Setup {
        folder: String,
    },
    SelectDrive {
        paths: Vec<String>,
    },
    Login,
    Logout,
    Browse {
        uid: Option<String>,
    },
    Add {
        local: String,
        remote: String,
    },
    Preview {
        id: String,
    },
    Approve {
        id: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Sync {
        id: String,
    },
    Remove {
        id: String,
    },
    Ignore {
        id: String,
        patterns: Vec<String>,
    },
    /// Put recovered files back where they were (IDs from `status`).
    Restore {
        ids: Vec<i64>,
    },
    /// List a file's versions in Proton Drive, newest first.
    Versions {
        id: String,
        path: String,
    },
    /// Make an older version current; it then downloads like any Drive edit.
    RestoreVersion {
        id: String,
        path: String,
        revision: String,
    },
    /// Change a preference, e.g. `set bandwidth_limit 1000000` (bytes per second) or `null`.
    Set {
        key: String,
        value: String,
    },
}
fn main() -> Result<()> {
    // User-only state, recovery files, and IPC child processes.
    unsafe {
        libc::umask(0o077);
    }
    let command = Args::parse().command;
    let request = match command {
        Command::LiveTest => json!({"action":"live-test"}),
        Command::Quit => json!({"action":"quit"}),
        Command::Daemon => return service::daemon(),
        Command::Status => {
            println!("{}", serde_json::to_string_pretty(&service::get_status()?)?);
            return Ok(());
        }
        Command::Setup { folder } => json!({"action":"setup","folder":folder}),
        Command::SelectDrive { paths } => json!({"action":"select-drive","paths":paths}),
        Command::Login => json!({"action":"login"}),
        Command::Logout => json!({"action":"logout"}),
        Command::Browse { uid } => json!({"action":"browse","uid":uid}),
        Command::Add { local, remote } => json!({"action":"add","local":local,"remote":remote}),
        Command::Preview { id } => json!({"action":"preview","id":id}),
        Command::Approve { id } => json!({"action":"approve","id":id}),
        Command::Pause { id } => json!({"action":"pause","id":id}),
        Command::Resume { id } => json!({"action":"resume","id":id}),
        Command::Sync { id } => json!({"action":"sync","id":id}),
        Command::Remove { id } => json!({"action":"remove","id":id}),
        Command::Ignore { id, patterns } => json!({"action":"ignore","id":id,"patterns":patterns}),
        Command::Restore { ids } => json!({"action":"restore-recovered","ids":ids}),
        Command::Versions { id, path } => json!({"action":"versions","id":id,"path":path}),
        Command::RestoreVersion { id, path, revision } => {
            json!({"action":"restore-version","id":id,"path":path,"revision":revision})
        }
        Command::Set { key, value } => {
            let value: serde_json::Value = serde_json::from_str(&value).unwrap_or(json!(value));
            json!({"action":"settings","settings":{key:value}})
        }
    };
    let result = service::call(request)?;
    if let Some(url) = result["login_url"].as_str() {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        println!("Sign in in your browser. {}", service::THIRD_PARTY);
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}
